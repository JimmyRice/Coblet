use std::process::{ExitStatus, Stdio};

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{OutputStream, ProcessError, ProcessExit, ProcessSpawner, ProcessSpec, SpawnedProcess};
use crate::BoxFuture;

/// Size of the chunks read from a pipe.
const PIPE_CHUNK: usize = 16 * 1024;

/// [`ProcessSpawner`] on `tokio::process`.
///
/// [`ProcessSpawner::spawn`] must run inside a tokio runtime: a task owns
/// the child, waits for it and kills it when asked.
///
/// # Examples
///
/// ```no_run
/// use coblet_runtime::{ProcessSpawner, ProcessSpec, TokioSpawner};
///
/// # async fn run() -> Result<(), coblet_runtime::ProcessError> {
/// let output = TokioSpawner::new()
///     .spawn(ProcessSpec::new("java").arg("-version"))?
///     .capture(64 * 1024)
///     .await?;
/// assert!(output.exit.success());
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioSpawner;

impl TokioSpawner {
    /// Creates the spawner.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

struct Pipe<R>(R);

impl<R: AsyncRead + Unpin + Send> OutputStream for Pipe<R> {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Bytes>, ProcessError>> {
        Box::pin(async move {
            let mut buffer = BytesMut::with_capacity(PIPE_CHUNK);
            let read = self
                .0
                .read_buf(&mut buffer)
                .await
                .map_err(|source| ProcessError::Io {
                    operation: "read output of",
                    source,
                })?;
            Ok((read > 0).then(|| buffer.freeze()))
        })
    }
}

fn exit_of(status: ExitStatus) -> ProcessExit {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal = None;
    ProcessExit {
        code: status.code(),
        signal,
    }
}

impl ProcessSpawner for TokioSpawner {
    fn spawn(&self, spec: ProcessSpec) -> Result<SpawnedProcess, ProcessError> {
        let mut command = tokio::process::Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in &spec.env_remove {
            command.env_remove(key);
        }
        command.envs(spec.env.iter().map(|(key, value)| (key, value)));
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
        let id = child.id();
        let stdout = child
            .stdout
            .take()
            .map(|pipe| Box::new(Pipe(pipe)) as Box<dyn OutputStream>);
        let stderr = child
            .stderr
            .take()
            .map(|pipe| Box::new(Pipe(pipe)) as Box<dyn OutputStream>);

        let kill = CancellationToken::new();
        let (sender, receiver) = oneshot::channel();
        let killed = kill.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                () = killed.cancelled() => match child.kill().await {
                    Ok(()) => child.wait().await,
                    Err(error) => Err(error),
                },
            };
            let exit = status.map(exit_of).map_err(|source| ProcessError::Io {
                operation: "wait for",
                source,
            });
            // The receiver is gone when nobody waits for the exit status.
            let _ = sender.send(exit);
        });
        let exit = Box::pin(async move {
            receiver.await.unwrap_or_else(|_| {
                Err(ProcessError::Io {
                    operation: "wait for",
                    source: std::io::Error::other("the process watcher stopped"),
                })
            })
        });
        Ok(SpawnedProcess::new(id, stdout, stderr, exit, kill))
    }
}

#[cfg(all(test, unix))]
#[path = "../../tests/process/local.rs"]
mod tests;
