use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use super::OperationError;
use crate::{Event, EventBus, EventSubscription, OperationId, PlanId, Received};

/// Aggregate progress of an Operation, updated as steps run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperationProgress {
    /// Steps in the Plan.
    pub steps_total: usize,
    /// Steps finished (including ones skipped because a resumed run had done them).
    pub steps_done: usize,
    /// Bytes reported by running and finished steps, usually downloaded bytes.
    pub bytes_done: u64,
}

/// Running Operation: await it for the result, cancel it, or watch its events.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_runtime::{
///     EventBus, ExecutionReport, Operations, PathRoot, Plan, PlanKind, PlanPath, RelPath, Step,
///     StepGraph, TokioStorage, WriteFileStep,
/// };
///
/// struct Note;
///
/// impl PlanKind for Note {
///     const NAME: &'static str = "note";
///     type Summary = String;
///     type Output = usize;
///     fn finish(summary: String, report: &ExecutionReport) -> usize {
///         report.steps.len() + summary.len()
///     }
/// }
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let root = tempfile::tempdir()?;
/// let operations = Operations::new(Arc::new(TokioStorage::new()), root.path(), EventBus::new(64));
///
/// let mut steps = StepGraph::new();
/// steps.add(
///     "write note",
///     Step::WriteFile(WriteFileStep {
///         path: PlanPath::data(RelPath::new("notes/a.txt")?),
///         contents: b"hello".to_vec(),
///     }),
///     &[],
/// )?;
/// let mut operation = operations.execute(Plan::<Note>::new("ab".to_owned(), steps, Vec::new()), None);
/// let _events = operation.subscribe();
/// assert_eq!(operation.await?, 3);
/// assert_eq!(std::fs::read(root.path().join("notes/a.txt"))?, b"hello");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// ```
pub struct OperationHandle<T> {
    pub(super) op: OperationId,
    pub(super) plan: PlanId,
    pub(super) cancel: CancellationToken,
    pub(super) events: EventBus,
    pub(super) early_events: Option<EventSubscription>,
    pub(super) progress: watch::Receiver<OperationProgress>,
    pub(super) result: oneshot::Receiver<Result<T, OperationError>>,
}

impl<T> OperationHandle<T> {
    /// Returns the Operation ID.
    #[must_use]
    pub fn id(&self) -> OperationId {
        self.op
    }

    /// Returns the ID of the Plan being executed; pass it to `resume` after a crash.
    #[must_use]
    pub fn plan(&self) -> PlanId {
        self.plan
    }

    /// Cancels the Operation. Running steps stop at their next IO boundary and
    /// staged changes are rolled back; awaiting the handle returns
    /// [`OperationError::Cancelled`].
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Returns a receiver for the aggregate progress.
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<OperationProgress> {
        self.progress.clone()
    }

    /// Receives the events of this Operation.
    ///
    /// The first call also delivers events published before it, starting
    /// with [`Event::OperationStarted`]; later calls start from the present.
    pub fn subscribe(&mut self) -> OperationEvents {
        OperationEvents {
            op: self.op,
            inner: self
                .early_events
                .take()
                .unwrap_or_else(|| self.events.subscribe()),
            finished: false,
        }
    }
}

impl<T> Future for OperationHandle<T> {
    type Output = Result<T, OperationError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.result)
            .poll(cx)
            .map(|received| received.unwrap_or(Err(OperationError::Aborted)))
    }
}

impl<T> fmt::Debug for OperationHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationHandle")
            .field("op", &self.op)
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

/// Events of one Operation, taken from the shared [`EventBus`].
#[derive(Debug)]
pub struct OperationEvents {
    op: OperationId,
    inner: EventSubscription,
    finished: bool,
}

impl OperationEvents {
    /// Waits for the next event of this Operation; returns `None` after
    /// [`Event::OperationFinished`] or once the bus is gone.
    pub async fn recv(&mut self) -> Option<Received> {
        if self.finished {
            return None;
        }
        loop {
            match self.inner.recv().await? {
                Received::Missed(count) => return Some(Received::Missed(count)),
                Received::Event(event) => {
                    let op = match &event {
                        Event::OperationStarted { op, .. }
                        | Event::StepProgress { op, .. }
                        | Event::StepFinished { op, .. } => Some(*op),
                        Event::OperationFinished { op, .. } => {
                            self.finished = *op == self.op;
                            Some(*op)
                        }
                        Event::Warning(warning) => warning.op,
                        Event::GameLog(_) => None,
                    };
                    if op == Some(self.op) {
                        return Some(Received::Event(event));
                    }
                }
            }
        }
    }
}
