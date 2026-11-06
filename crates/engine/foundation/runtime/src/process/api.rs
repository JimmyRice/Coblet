use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;

use bytes::{Bytes, BytesMut};
use tokio_util::sync::CancellationToken;

use crate::BoxFuture;

/// Starts child processes: the game, Forge installer processors and Java probes.
///
/// [`TokioSpawner`](crate::TokioSpawner) (feature `process`) implements it
/// on `tokio::process`; tests substitute scripted processes.
pub trait ProcessSpawner: Send + Sync + 'static {
    /// Starts a process with piped stdout and stderr and a closed stdin.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Spawn`] when the program cannot be started.
    fn spawn(&self, spec: ProcessSpec) -> Result<SpawnedProcess, ProcessError>;
}

/// Program, arguments, environment and working directory of a child process.
///
/// Arguments and environment values are [`OsString`]s so paths that are not
/// valid UTF-8 survive.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSpec {
    /// The executable.
    pub program: PathBuf,
    /// Arguments after the program name.
    pub args: Vec<OsString>,
    /// Variables set on top of the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// Inherited variables to remove.
    pub env_remove: Vec<OsString>,
    /// Working directory; the parent's when unset.
    pub cwd: Option<PathBuf>,
}

impl ProcessSpec {
    /// Describes a process with no arguments.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            cwd: None,
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends several arguments.
    #[must_use]
    pub fn args<I: IntoIterator<Item = A>, A: Into<OsString>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets an environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Sets the working directory.
    #[must_use]
    pub fn cwd(self, dir: impl Into<PathBuf>) -> Self {
        Self {
            cwd: Some(dir.into()),
            ..self
        }
    }
}

/// How a child process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessExit {
    /// Exit code, when the process exited normally.
    pub code: Option<i32>,
    /// Signal number on Unix, when a signal ended the process.
    pub signal: Option<i32>,
}

impl ProcessExit {
    /// Reports whether the process exited with code 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

impl fmt::Display for ProcessExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.code, self.signal) {
            (Some(code), _) => write!(f, "exit code {code}"),
            (None, Some(signal)) => write!(f, "signal {signal}"),
            (None, None) => f.write_str("unknown exit status"),
        }
    }
}

/// Failure to start or wait for a child process.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The program could not be started.
    #[error("failed to start {program}: {source}")]
    Spawn {
        /// The program.
        program: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// Waiting for the process or reading its output failed.
    #[error("failed to {operation} the child process: {source}")]
    Io {
        /// What was being done, such as `wait for`.
        operation: &'static str,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The output is larger than the limit given to [`SpawnedProcess::capture`].
    #[error("process output exceeds the {limit} byte limit")]
    OutputTooLarge {
        /// The limit in bytes.
        limit: usize,
    },
}

/// Output pipe of a child process, read chunk by chunk.
pub trait OutputStream: Send {
    /// Returns the next chunk, or `None` once the pipe is closed.
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, ProcessError>>;
}

/// Asks a running process to stop; cloning gives another handle to the same process.
#[derive(Clone, Debug)]
pub struct KillHandle(CancellationToken);

impl KillHandle {
    /// Kills the process. Calling it after the process ended does nothing.
    pub fn kill(&self) {
        self.0.cancel();
    }
}

/// Running child process returned by [`ProcessSpawner::spawn`].
///
/// Dropping it neither waits for nor kills the process.
pub struct SpawnedProcess {
    id: Option<u32>,
    stdout: Option<Box<dyn OutputStream>>,
    stderr: Option<Box<dyn OutputStream>>,
    exit: BoxFuture<'static, Result<ProcessExit, ProcessError>>,
    kill: CancellationToken,
}

/// Exit status and complete output of a process run by [`SpawnedProcess::capture`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedOutput {
    /// How the process ended.
    pub exit: ProcessExit,
    /// Everything written to stdout.
    pub stdout: Bytes,
    /// Everything written to stderr.
    pub stderr: Bytes,
}

impl SpawnedProcess {
    /// Assembles a process handle; spawner implementations call this.
    ///
    /// `exit` resolves when the process ends. The implementation must kill
    /// the process once `kill` is cancelled and still resolve `exit`.
    #[must_use]
    pub fn new(
        id: Option<u32>,
        stdout: Option<Box<dyn OutputStream>>,
        stderr: Option<Box<dyn OutputStream>>,
        exit: BoxFuture<'static, Result<ProcessExit, ProcessError>>,
        kill: CancellationToken,
    ) -> Self {
        Self {
            id,
            stdout,
            stderr,
            exit,
            kill,
        }
    }

    /// Returns the operating system process ID, when known.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.id
    }

    /// Takes the stdout pipe; returns `None` on the second call.
    pub fn take_stdout(&mut self) -> Option<Box<dyn OutputStream>> {
        self.stdout.take()
    }

    /// Takes the stderr pipe; returns `None` on the second call.
    pub fn take_stderr(&mut self) -> Option<Box<dyn OutputStream>> {
        self.stderr.take()
    }

    /// Returns a handle that can kill the process from another task.
    #[must_use]
    pub fn kill_handle(&self) -> KillHandle {
        KillHandle(self.kill.clone())
    }

    /// Waits for the process to end.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Io`] when the exit status cannot be read.
    pub async fn wait(self) -> Result<ProcessExit, ProcessError> {
        self.exit.await
    }

    /// Reads stdout and stderr to the end, then waits for the exit status.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::OutputTooLarge`] (after killing the process)
    /// when either stream exceeds `limit` bytes, or the error of a pipe or of
    /// waiting.
    pub async fn capture(mut self, limit: usize) -> Result<CapturedOutput, ProcessError> {
        let stdout = self.stdout.take();
        let stderr = self.stderr.take();
        let (stdout, stderr) = tokio::join!(drain(stdout, limit), drain(stderr, limit));
        let (stdout, stderr) = match (stdout, stderr) {
            (Ok(out), Ok(err)) => (out, err),
            (Err(error), _) | (_, Err(error)) => {
                self.kill.cancel();
                // The process is being killed; its exit status adds nothing to the error.
                let _ = self.exit.await;
                return Err(error);
            }
        };
        let exit = self.exit.await?;
        Ok(CapturedOutput {
            exit,
            stdout,
            stderr,
        })
    }
}

async fn drain(stream: Option<Box<dyn OutputStream>>, limit: usize) -> Result<Bytes, ProcessError> {
    let Some(mut stream) = stream else {
        return Ok(Bytes::new());
    };
    let mut collected = BytesMut::new();
    while let Some(chunk) = stream.next_chunk().await? {
        if collected.len() + chunk.len() > limit {
            return Err(ProcessError::OutputTooLarge { limit });
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(collected.freeze())
}

impl fmt::Debug for SpawnedProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnedProcess")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use tokio_util::sync::CancellationToken;

    use super::{OutputStream, ProcessError, ProcessExit, ProcessSpec, SpawnedProcess};
    use crate::BoxFuture;

    struct Lines(Vec<&'static str>);

    impl OutputStream for Lines {
        fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, ProcessError>> {
            let next =
                (!self.0.is_empty()).then(|| Bytes::from_static(self.0.remove(0).as_bytes()));
            Box::pin(async move { Ok(next) })
        }
    }

    fn scripted(out: Vec<&'static str>) -> SpawnedProcess {
        SpawnedProcess::new(
            Some(7),
            Some(Box::new(Lines(out))),
            Some(Box::new(Lines(vec!["warn\n"]))),
            Box::pin(async {
                Ok(ProcessExit {
                    code: Some(0),
                    signal: None,
                })
            }),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn captures_both_streams() {
        let output = scripted(vec!["a", "b"]).capture(16).await.unwrap();
        assert!(output.exit.success());
        assert_eq!(&output.stdout[..], b"ab");
        assert_eq!(&output.stderr[..], b"warn\n");
    }

    #[tokio::test]
    async fn stops_at_the_output_limit() {
        let error = scripted(vec!["aaaa", "bbbb"]).capture(6).await.unwrap_err();
        assert!(matches!(error, ProcessError::OutputTooLarge { limit: 6 }));
    }

    #[test]
    fn builds_specs() {
        let spec = ProcessSpec::new("java")
            .arg("-version")
            .args(["-Xmx1G"])
            .env("A", "1")
            .cwd("/tmp");
        assert_eq!(spec.args.len(), 2);
        assert_eq!(spec.env.len(), 1);
        assert_eq!(
            ProcessExit {
                code: None,
                signal: Some(9)
            }
            .to_string(),
            "signal 9"
        );
    }
}
