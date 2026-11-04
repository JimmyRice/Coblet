use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use bytes::BytesMut;
use coblet_launch_command::LaunchCommand;
use coblet_runtime::{
    EntryKind, Event, EventBus, GameLogLine, KillHandle, LogLevel, LogSource, OutputStream,
    ProcessExit, ProcessSpawner, SpawnedProcess, Storage,
};
use tokio::task::JoinHandle;

use crate::log::{LogParser, Redact};
use crate::{LaunchError, LaunchHook, LogBuffer, LogStream};

/// Longest partial line kept while waiting for its line break.
const MAX_LINE_BYTES: usize = 1 << 20;

/// How a game run ended.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameExit {
    /// Exit status of the process.
    pub exit: ProcessExit,
    /// Time from start to exit.
    pub duration: Duration,
    /// Whether [`GameSession::kill`] ended the game.
    pub killed: bool,
    /// Whether the game probably crashed: it was not killed and exited
    /// with an error or wrote a crash report.
    pub suspected_crash: bool,
    /// Crash reports written during the run.
    pub crash_reports: Vec<PathBuf>,
}

/// Running game started by a [`Launcher`](crate::Launcher).
pub struct GameSession {
    pid: Option<u32>,
    command: LaunchCommand,
    logs: Arc<LogBuffer>,
    kill: KillHandle,
    killed: Arc<AtomicBool>,
    monitor: JoinHandle<Result<GameExit, LaunchError>>,
}

impl std::fmt::Debug for GameSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameSession")
            .field("pid", &self.pid)
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

/// What the monitor needs besides the process.
pub(crate) struct Monitor {
    pub(crate) logs: Arc<LogBuffer>,
    pub(crate) redact: Redact,
    pub(crate) events: Option<EventBus>,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) spawner: Arc<dyn ProcessSpawner>,
    pub(crate) game_directory: PathBuf,
    pub(crate) post_exit: Option<LaunchHook>,
}

impl Monitor {
    fn emit(&self, line: GameLogLine) {
        if let Some(events) = &self.events {
            events.publish(Event::GameLog(line.clone()));
        }
        self.logs.push(line);
    }

    fn launcher_line(&self, level: LogLevel, message: &str) {
        let message = (self.redact)(message);
        self.emit(GameLogLine::plain(LogSource::Launcher, level, message));
    }

    async fn pump(&self, stream: Option<Box<dyn OutputStream>>, source: LogSource) {
        let Some(mut stream) = stream else {
            return;
        };
        let mut parser = LogParser::new(source, Arc::clone(&self.redact));
        let mut pending = BytesMut::new();
        loop {
            match stream.next_chunk().await {
                Ok(Some(chunk)) => {
                    pending.extend_from_slice(&chunk);
                    while let Some(end) = pending.iter().position(|&byte| byte == b'\n') {
                        let line = pending.split_to(end + 1);
                        self.feed(&mut parser, &line);
                    }
                    if pending.len() > MAX_LINE_BYTES {
                        let line = pending.split();
                        self.feed(&mut parser, &line);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    self.launcher_line(
                        LogLevel::Warn,
                        &format!("reading game output failed: {error}"),
                    );
                    break;
                }
            }
        }
        if !pending.is_empty() {
            self.feed(&mut parser, &pending);
        }
        for line in parser.finish() {
            self.emit(line);
        }
    }

    fn feed(&self, parser: &mut LogParser, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches(['\n', '\r']);
        for line in parser.push(text) {
            self.emit(line);
        }
    }

    async fn crash_reports(&self, since: SystemTime) -> Vec<PathBuf> {
        let dir = self.game_directory.join("crash-reports");
        let Ok(entries) = self.storage.read_dir(&dir).await else {
            return Vec::new();
        };
        let mut reports = Vec::new();
        for entry in entries {
            if entry.kind != EntryKind::File {
                continue;
            }
            if let Ok(Some(metadata)) = self.storage.metadata(&entry.path).await
                && metadata.modified.is_some_and(|modified| modified >= since)
            {
                reports.push(entry.path);
            }
        }
        reports.sort();
        reports
    }

    async fn run_post_exit(&self) {
        let Some(hook) = &self.post_exit else {
            return;
        };
        let spec = hook.spec(&self.game_directory);
        let result = match self.spawner.spawn(spec) {
            Ok(process) => process.wait().await,
            Err(error) => Err(error),
        };
        match result {
            Ok(exit) if exit.success() => {}
            Ok(exit) => self.launcher_line(
                LogLevel::Warn,
                &format!("post-exit command ended with {exit}"),
            ),
            Err(error) => self.launcher_line(
                LogLevel::Warn,
                &format!("post-exit command failed: {error}"),
            ),
        }
    }

    pub(crate) async fn run(
        self,
        mut process: SpawnedProcess,
        killed: Arc<AtomicBool>,
    ) -> Result<GameExit, LaunchError> {
        let started_at = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let started = Instant::now();
        let stdout = process.take_stdout();
        let stderr = process.take_stderr();
        tokio::join!(
            self.pump(stdout, LogSource::Stdout),
            self.pump(stderr, LogSource::Stderr)
        );
        let exit = process.wait().await;
        let duration = started.elapsed();
        let exit = match exit {
            Ok(exit) => exit,
            Err(error) => {
                self.launcher_line(
                    LogLevel::Error,
                    &format!("waiting for the game failed: {error}"),
                );
                self.logs.close();
                return Err(error.into());
            }
        };
        let crash_reports = self.crash_reports(started_at).await;
        let killed = killed.load(Ordering::Acquire);
        let suspected_crash = !killed && (!exit.success() || !crash_reports.is_empty());
        let level = if suspected_crash {
            LogLevel::Error
        } else {
            LogLevel::Info
        };
        self.launcher_line(
            level,
            &format!("game ended with {exit} after {}s", duration.as_secs()),
        );
        for report in &crash_reports {
            self.launcher_line(
                LogLevel::Error,
                &format!("crash report: {}", report.display()),
            );
        }
        self.run_post_exit().await;
        self.logs.close();
        Ok(GameExit {
            exit,
            duration,
            killed,
            suspected_crash,
            crash_reports,
        })
    }
}

impl GameSession {
    pub(crate) fn start(command: LaunchCommand, process: SpawnedProcess, monitor: Monitor) -> Self {
        let pid = process.id();
        let kill = process.kill_handle();
        let killed = Arc::new(AtomicBool::new(false));
        let logs = Arc::clone(&monitor.logs);
        let task = tokio::spawn(monitor.run(process, Arc::clone(&killed)));
        Self {
            pid,
            command,
            logs,
            kill,
            killed,
            monitor: task,
        }
    }

    /// Returns the operating system process ID, when known.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Returns the command that started the game; its `Display` is redacted.
    #[must_use]
    pub fn command(&self) -> &LaunchCommand {
        &self.command
    }

    /// Subscribes to the log: kept lines first, then live ones.
    #[must_use]
    pub fn logs(&self) -> LogStream {
        self.logs.subscribe()
    }

    /// Returns the kept log lines.
    #[must_use]
    pub fn log_snapshot(&self) -> Vec<GameLogLine> {
        self.logs.snapshot()
    }

    /// Kills the game. [`GameSession::wait`] still reports the exit.
    pub fn kill(&self) {
        self.killed.store(true, Ordering::Release);
        self.kill.kill();
    }

    /// Waits for the game to end and the post-exit command to finish.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::Process`] when the exit status cannot be read.
    pub async fn wait(self) -> Result<GameExit, LaunchError> {
        self.monitor.await.map_err(|error| LaunchError::Step {
            step: "monitor".to_owned(),
            message: error.to_string(),
        })?
    }
}
