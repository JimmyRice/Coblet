use std::sync::Mutex;

use coblet::launch::{GameSession, LogItem, LogStream};

use crate::{CobletError, ErrorKind};

/// Line of game output.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LogLine {
    /// `stdout`, `stderr` or `launcher`.
    pub source: String,
    /// Level such as `INFO`.
    pub level: String,
    /// Thread name, when known.
    pub thread: Option<String>,
    /// Logger name, when known.
    pub logger: Option<String>,
    /// The message, with secrets redacted.
    pub message: String,
}

/// How a game ended.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GameExitInfo {
    /// Exit code, when the process exited normally.
    pub code: Option<i32>,
    /// Whether `kill` ended the game.
    pub killed: bool,
    /// Whether the game probably crashed.
    pub suspected_crash: bool,
    /// Run time in milliseconds.
    pub duration_ms: u64,
}

/// Running game.
#[derive(uniffi::Object)]
pub struct GameHandle {
    pid: Option<u32>,
    report: String,
    session: Mutex<Option<GameSession>>,
    logs: tokio::sync::Mutex<LogStream>,
}

impl std::fmt::Debug for GameHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameHandle")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl GameHandle {
    pub(crate) fn new(session: GameSession) -> Self {
        Self {
            pid: session.pid(),
            report: session.profile().explain(),
            logs: tokio::sync::Mutex::new(session.logs()),
            session: Mutex::new(Some(session)),
        }
    }

    fn take(&self) -> Option<GameSession> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl GameHandle {
    /// Returns the process ID, when known.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Returns the launch report: where every library, argument and the Java
    /// requirement came from, including the changes of customizers.
    pub fn report(&self) -> String {
        self.report.clone()
    }

    /// Waits for the next log line; `None` once the game ended and all lines were read.
    pub async fn next_log(&self) -> Option<LogLine> {
        let mut logs = self.logs.lock().await;
        match logs.next().await? {
            LogItem::Line(line) => Some(LogLine {
                source: format!("{:?}", line.source).to_lowercase(),
                level: line.level.as_str().to_owned(),
                thread: line.thread,
                logger: line.logger,
                message: line.message,
            }),
            LogItem::Missed(count) => Some(LogLine {
                source: "launcher".to_owned(),
                level: "WARN".to_owned(),
                thread: None,
                logger: None,
                message: format!("{count} lines were skipped"),
            }),
        }
    }

    /// Kills the game; `wait` still reports the exit.
    pub fn kill(&self) {
        if let Some(session) = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            session.kill();
        }
    }

    /// Waits for the game to end; can be called once.
    ///
    /// # Errors
    ///
    /// Fails when called twice or when the exit status cannot be read.
    pub async fn wait(&self) -> Result<GameExitInfo, CobletError> {
        let session = self.take().ok_or_else(|| {
            CobletError::new(ErrorKind::InvalidState, "the game was already waited for")
        })?;
        let exit = session
            .wait()
            .await
            .map_err(|error| CobletError::new(ErrorKind::Launch, error.to_string()))?;
        Ok(GameExitInfo {
            code: exit.exit.code,
            killed: exit.killed,
            suspected_crash: exit.suspected_crash,
            duration_ms: u64::try_from(exit.duration.as_millis()).unwrap_or(u64::MAX),
        })
    }
}
