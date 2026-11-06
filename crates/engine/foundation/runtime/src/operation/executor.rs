use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::{oneshot, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::context::{ProgressState, Shared};
use super::handle::{OperationHandle, OperationProgress};
use super::journal::{self, Journal};
use super::{
    DownloadHandler, ExecutionReport, ExtractHandler, OperationError, ProcessHandler, StepContext,
    StepError, builtin,
};
use crate::{
    Event, EventBus, OperationId, OperationOutcome, Plan, PlanId, PlanKind, Precondition,
    ProgressThrottle, Step, StepGraph, StepId, StepOutcome, Storage, StorageError,
};

/// Steps run at the same time by default; downloads add their own per-host limits.
pub const DEFAULT_STEP_CONCURRENCY: usize = 32;

/// Executes Plans as Operations.
///
/// Every Operation stages its writes in `data_root/.staging/<plan-id>/`.
/// A serializable Plan is saved next to it together with a journal of
/// finished steps, so [`Operations::resume`] can continue after a crash.
/// On success the staging directory, the saved Plan and the journal are
/// removed. On failure or cancellation the staging directory is removed
/// (rolling back everything not yet moved into place) and the journal
/// records the rollback.
///
/// Steps that only need [`Storage`] run in the executor. Download, extract
/// and process steps go to handlers installed with the `with_*_handler`
/// methods; custom steps run themselves.
#[derive(Clone)]
pub struct Operations {
    storage: Arc<dyn Storage>,
    data_root: Arc<Path>,
    events: EventBus,
    download: Option<Arc<dyn DownloadHandler>>,
    extract: Option<Arc<dyn ExtractHandler>>,
    process: Option<Arc<dyn ProcessHandler>>,
    concurrency: NonZeroUsize,
}

/// Step handlers and Plan data shared by the tasks of one Operation.
struct Work {
    steps: StepGraph,
    preconditions: Vec<Precondition>,
    download: Option<Arc<dyn DownloadHandler>>,
    extract: Option<Arc<dyn ExtractHandler>>,
    process: Option<Arc<dyn ProcessHandler>>,
}

impl Operations {
    /// Creates an executor without download, extract or process handlers.
    #[must_use]
    pub fn new(storage: Arc<dyn Storage>, data_root: impl AsRef<Path>, events: EventBus) -> Self {
        Self {
            storage,
            data_root: Arc::from(data_root.as_ref()),
            events,
            download: None,
            extract: None,
            process: None,
            concurrency: NonZeroUsize::new(DEFAULT_STEP_CONCURRENCY).unwrap_or(NonZeroUsize::MIN),
        }
    }

    /// Installs the handler for download steps.
    #[must_use]
    pub fn with_download_handler(self, handler: Arc<dyn DownloadHandler>) -> Self {
        Self {
            download: Some(handler),
            ..self
        }
    }

    /// Installs the handler for extract steps.
    #[must_use]
    pub fn with_extract_handler(self, handler: Arc<dyn ExtractHandler>) -> Self {
        Self {
            extract: Some(handler),
            ..self
        }
    }

    /// Installs the handler for process steps.
    #[must_use]
    pub fn with_process_handler(self, handler: Arc<dyn ProcessHandler>) -> Self {
        Self {
            process: Some(handler),
            ..self
        }
    }

    /// Sets how many steps may run at the same time.
    #[must_use]
    pub fn with_concurrency(self, concurrency: NonZeroUsize) -> Self {
        Self {
            concurrency,
            ..self
        }
    }

    /// Returns the event bus Operations publish to.
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// Starts executing a Plan. `target` is the directory `target:` paths resolve against.
    ///
    /// Must be called inside a tokio runtime; the Operation runs on a spawned task.
    pub fn execute<K: PlanKind>(
        &self,
        plan: Plan<K>,
        target: Option<PathBuf>,
    ) -> OperationHandle<K::Output> {
        self.start(plan, target, None)
    }

    /// Continues a Plan saved by an earlier, interrupted run.
    ///
    /// Steps the journal records as finished are skipped, unless the earlier
    /// run was rolled back.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError::NotResumable`] when no saved Plan exists or
    /// it is not a `K` Plan, and [`OperationError::Storage`] when reading fails.
    pub async fn resume<K: PlanKind>(
        &self,
        plan: PlanId,
        target: Option<PathBuf>,
    ) -> Result<OperationHandle<K::Output>, OperationError> {
        let not_resumable = |reason: String| OperationError::NotResumable { plan, reason };
        let saved = match self
            .storage
            .read(&journal::plan_file(&self.data_root, plan))
            .await
        {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => {
                return Err(not_resumable("no saved plan".to_owned()));
            }
            Err(error) => return Err(error.into()),
        };
        let restored: Plan<K> =
            serde_json::from_slice(&saved).map_err(|e| not_resumable(e.to_string()))?;
        let done = journal::read(
            &*self.storage,
            &journal::journal_file(&self.data_root, plan),
        )
        .await?;
        Ok(self.start(restored, target, Some(done)))
    }

    /// Lists Plans with a saved journal: interrupted or rolled-back Operations.
    ///
    /// # Errors
    ///
    /// Returns the [`StorageError`] of listing the staging directory.
    pub async fn pending(&self) -> Result<Vec<PlanId>, StorageError> {
        let dir = journal::staging_dir(&self.data_root);
        if self.storage.metadata(&dir).await?.is_none() {
            return Ok(Vec::new());
        }
        let mut plans: Vec<PlanId> = self
            .storage
            .read_dir(&dir)
            .await?
            .iter()
            .filter_map(|entry| {
                let name = entry.path.file_name()?.to_str()?;
                name.strip_suffix(".plan.json")?.parse().ok()
            })
            .collect();
        plans.sort();
        Ok(plans)
    }

    fn start<K: PlanKind>(
        &self,
        plan: Plan<K>,
        target: Option<PathBuf>,
        resumed: Option<HashSet<StepId>>,
    ) -> OperationHandle<K::Output> {
        let op = OperationId::random();
        let plan_id = plan.id();
        let cancel = CancellationToken::new();
        let (progress_tx, progress) = watch::channel(OperationProgress {
            steps_total: plan.steps().len(),
            ..OperationProgress::default()
        });
        let shared = Arc::new(Shared {
            op,
            plan: plan_id,
            storage: Arc::clone(&self.storage),
            data_root: self.data_root.to_path_buf(),
            staging_root: journal::staging_dir(&self.data_root).join(plan_id.to_string()),
            target,
            cancel: cancel.clone(),
            events: self.events.clone(),
            progress: Mutex::new(ProgressState {
                per_step: HashMap::new(),
                throttle: ProgressThrottle::default(),
            }),
            progress_tx,
        });
        let early_events = self.events.subscribe();
        let (sender, result) = oneshot::channel();
        let this = self.clone();
        tokio::spawn(async move {
            let outcome = this.run(plan, shared, resumed).await;
            // Nobody may be waiting for the result any more.
            let _ = sender.send(outcome);
        });
        OperationHandle {
            op,
            plan: plan_id,
            cancel,
            events: self.events.clone(),
            early_events: Some(early_events),
            progress,
            result,
        }
    }

    async fn run<K: PlanKind>(
        self,
        plan: Plan<K>,
        shared: Arc<Shared>,
        resumed: Option<HashSet<StepId>>,
    ) -> Result<K::Output, OperationError> {
        self.events.publish(Event::OperationStarted {
            op: shared.op,
            kind: K::NAME,
        });
        let serializable = !plan.has_custom_steps();
        let saved_plan = if serializable && resumed.is_none() {
            serde_json::to_vec(&plan).ok()
        } else {
            None
        };
        let (_, summary, steps, preconditions) = plan.into_parts();
        let work = Arc::new(Work {
            steps,
            preconditions,
            download: self.download.clone(),
            extract: self.extract.clone(),
            process: self.process.clone(),
        });
        let (journal, result) = match self.prepare(&shared, &work, saved_plan, serializable).await {
            Ok(mut journal) => {
                let result = self
                    .run_steps(
                        &shared,
                        &work,
                        resumed.unwrap_or_default(),
                        journal.as_mut(),
                    )
                    .await;
                (journal, result)
            }
            Err(error) => (None, Err(error)),
        };
        let storage = &*self.storage;
        match result {
            Ok(report) => {
                if let Some(journal) = journal {
                    journal.close().await?;
                }
                storage.remove_dir_all(&shared.staging_root).await?;
                storage
                    .remove_file(&journal::plan_file(&self.data_root, shared.plan))
                    .await?;
                storage
                    .remove_file(&journal::journal_file(&self.data_root, shared.plan))
                    .await?;
                self.events.publish(Event::OperationFinished {
                    op: shared.op,
                    outcome: OperationOutcome::Succeeded,
                });
                Ok(K::finish(summary, &report))
            }
            Err(error) => {
                // Roll back what is still staged; files already moved into place stay.
                let cleanup = storage.remove_dir_all(&shared.staging_root).await;
                if let Some(journal) = journal {
                    journal.rolled_back().await?;
                }
                let outcome = if error.is_cancelled() {
                    OperationOutcome::Cancelled
                } else {
                    OperationOutcome::Failed {
                        message: error.to_string(),
                    }
                };
                self.events.publish(Event::OperationFinished {
                    op: shared.op,
                    outcome,
                });
                cleanup?;
                Err(if error.is_cancelled() {
                    OperationError::Cancelled
                } else {
                    error
                })
            }
        }
    }

    /// Rechecks preconditions, creates the staging directory, saves the Plan and opens the journal.
    async fn prepare(
        &self,
        shared: &Shared,
        work: &Work,
        saved_plan: Option<Vec<u8>>,
        serializable: bool,
    ) -> Result<Option<Journal>, OperationError> {
        builtin::check_preconditions(shared, &work.preconditions)
            .await
            .map_err(|error| match error {
                StepError::PlanStale { path } => OperationError::PlanStale { path },
                StepError::Storage(error) => OperationError::Storage(error),
                other => OperationError::Step {
                    step: StepId(0),
                    label: "check preconditions".to_owned(),
                    source: other,
                },
            })?;
        let storage = &*self.storage;
        storage.create_dir_all(&shared.staging_root).await?;
        if let Some(saved) = saved_plan {
            storage
                .write_atomic(
                    &journal::plan_file(&self.data_root, shared.plan),
                    saved.into(),
                )
                .await?;
        }
        if !serializable {
            return Ok(None);
        }
        let path = journal::journal_file(&self.data_root, shared.plan);
        Ok(Some(Journal::open(storage, &path).await?))
    }

    async fn run_steps(
        &self,
        shared: &Arc<Shared>,
        work: &Arc<Work>,
        resumed: HashSet<StepId>,
        mut journal: Option<&mut Journal>,
    ) -> Result<ExecutionReport, OperationError> {
        let mut report = ExecutionReport {
            op: shared.op,
            plan: shared.plan,
            steps: resumed
                .iter()
                .map(|&step| (step, StepOutcome::Skipped))
                .collect(),
        };
        report.steps.sort_by_key(|(step, _)| *step);
        let mut finished = resumed;
        shared.update_progress(|p| p.steps_done = finished.len());
        let mut running = HashSet::new();
        let mut tasks = JoinSet::new();
        loop {
            for step in work.steps.ready(&finished) {
                if tasks.len() >= self.concurrency.get() {
                    break;
                }
                if !running.insert(step) {
                    continue;
                }
                let cx = StepContext {
                    shared: Arc::clone(shared),
                    step,
                };
                let work = Arc::clone(work);
                tasks.spawn(async move {
                    let result = run_step(&work, &cx).await;
                    (step, result)
                });
            }
            let Some(joined) = tasks.join_next().await else {
                break;
            };
            let (step, result) = joined.map_err(|_| OperationError::Aborted)?;
            running.remove(&step);
            match result {
                Ok(outcome) => {
                    finished.insert(step);
                    if let Some(journal) = journal.as_deref_mut() {
                        journal.step_done(step).await?;
                    }
                    shared.update_progress(|p| p.steps_done += 1);
                    self.events.publish(Event::StepFinished {
                        op: shared.op,
                        step,
                        outcome: outcome.clone(),
                    });
                    report.steps.push((step, outcome));
                }
                Err(source) => {
                    let outcome = match &source {
                        StepError::Cancelled => StepOutcome::Cancelled,
                        other => StepOutcome::Failed {
                            message: other.to_string(),
                        },
                    };
                    self.events.publish(Event::StepFinished {
                        op: shared.op,
                        step,
                        outcome,
                    });
                    // Stop the other steps; their results no longer matter.
                    shared.cancel.cancel();
                    while tasks.join_next().await.is_some() {}
                    let label = work
                        .steps
                        .get(step)
                        .map(|node| node.label().to_owned())
                        .unwrap_or_default();
                    return Err(OperationError::Step {
                        step,
                        label,
                        source,
                    });
                }
            }
        }
        Ok(report)
    }
}

async fn run_step(work: &Work, cx: &StepContext) -> Result<StepOutcome, StepError> {
    cx.ensure_not_cancelled()?;
    let Some(node) = work.steps.get(cx.step) else {
        return Err(StepError::other("step is not in the plan"));
    };
    match node.step() {
        Step::Download(step) => match &work.download {
            Some(handler) => handler.download(step, cx).await,
            None => Err(StepError::NoHandler("download")),
        },
        Step::Extract(step) => match &work.extract {
            Some(handler) => handler.extract(step, cx).await,
            None => Err(StepError::NoHandler("extract")),
        },
        Step::RunProcess(step) => match &work.process {
            Some(handler) => handler.run(step, cx).await,
            None => Err(StepError::NoHandler("process")),
        },
        Step::VerifyHash(step) => builtin::verify_hash(cx, step).await,
        Step::WriteFile(step) => builtin::write_file(cx, step).await,
        Step::MoveIntoPlace(step) => builtin::move_into_place(cx, step).await,
        Step::Remove(step) => builtin::remove(cx, step).await,
        Step::WriteMetadata(step) => builtin::write_metadata(cx, step, &work.preconditions).await,
        Step::Custom(step) => step.run(cx).await,
    }
}

impl std::fmt::Debug for Operations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operations")
            .field("data_root", &self.data_root)
            .field("concurrency", &self.concurrency)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    //! Executor scenarios: staging, rollback, cancellation, stale Plans and resume.

    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use coblet_types::FileHash;

    use super::Operations;

    use crate::operation::journal;
    use crate::{
        BoxFuture, CustomStep, DownloadHandler, DownloadStep, Event, EventBus, ExecutionReport,
        Expectation, MoveStep, OperationError, Plan, PlanKind, PlanPath, Precondition, Received,
        RelPath, Sources, Step, StepContext, StepError, StepGraph, StepOutcome, TokioStorage,
        WriteFileStep,
    };

    struct Install;

    impl PlanKind for Install {
        const NAME: &'static str = "install";
        type Summary = ();
        type Output = usize;
        fn finish((): (), report: &ExecutionReport) -> usize {
            report.steps.len()
        }
    }

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn operations(root: &Path) -> Operations {
        Operations::new(Arc::new(TokioStorage::new()), root, EventBus::new(256))
    }

    fn write(steps: &mut StepGraph, path: PlanPath, contents: &str) -> crate::StepId {
        steps
            .add(
                format!("write {path}"),
                Step::WriteFile(WriteFileStep {
                    path,
                    contents: contents.as_bytes().to_vec(),
                }),
                &[],
            )
            .unwrap()
    }

    /// Writes a file into staging, then moves it into the target directory.
    fn staged_install() -> StepGraph {
        let mut steps = StepGraph::new();
        let staged = write(&mut steps, PlanPath::staging(rel("mods/a.jar")), "jar");
        steps
            .add(
                "place a.jar",
                Step::MoveIntoPlace(MoveStep {
                    from: PlanPath::staging(rel("mods/a.jar")),
                    to: PlanPath::target(rel("mods/a.jar")),
                }),
                &[staged],
            )
            .unwrap();
        steps
    }

    #[derive(Debug)]
    struct Fail;

    impl CustomStep for Fail {
        fn kind(&self) -> &'static str {
            "test:fail"
        }
        fn describe(&self) -> String {
            "always fails".to_owned()
        }
        fn run<'a>(
            &'a self,
            _cx: &'a StepContext,
        ) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
            Box::pin(async { Err(StepError::other("boom")) })
        }
    }

    #[derive(Debug)]
    struct WaitForCancel;

    impl CustomStep for WaitForCancel {
        fn kind(&self) -> &'static str {
            "test:wait"
        }
        fn describe(&self) -> String {
            "waits until cancelled".to_owned()
        }
        fn run<'a>(&'a self, cx: &'a StepContext) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
            Box::pin(async move {
                cx.cancellation().cancelled().await;
                cx.ensure_not_cancelled().map(|()| StepOutcome::Succeeded)
            })
        }
    }

    #[tokio::test]
    async fn stages_then_moves_into_place_and_cleans_up() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let mut operation = operations(root.path()).execute(
            Plan::<Install>::new((), staged_install(), Vec::new()),
            Some(target.path().to_path_buf()),
        );
        let plan = operation.plan();
        let mut events = operation.subscribe();
        let progress = operation.progress();
        assert_eq!(operation.await.unwrap(), 2);
        assert_eq!(
            std::fs::read(target.path().join("mods/a.jar")).unwrap(),
            b"jar"
        );
        assert!(!root.path().join(".staging").join(plan.to_string()).exists());
        assert!(!journal::journal_file(root.path(), plan).exists());
        assert_eq!(progress.borrow().steps_done, 2);

        let mut seen = Vec::new();
        while let Some(Received::Event(event)) = events.recv().await {
            seen.push(event);
        }
        assert!(matches!(
            seen.first(),
            Some(Event::OperationStarted {
                kind: "install",
                ..
            })
        ));
        assert!(matches!(seen.last(), Some(Event::OperationFinished { .. })));
    }

    #[tokio::test]
    async fn rolls_back_staging_when_a_step_fails() {
        let root = tempfile::tempdir().unwrap();
        let mut steps = StepGraph::new();
        let staged = write(&mut steps, PlanPath::staging(rel("a.txt")), "a");
        steps
            .add("fail", Step::Custom(Arc::new(Fail)), &[staged])
            .unwrap();
        let operation =
            operations(root.path()).execute(Plan::<Install>::new((), steps, Vec::new()), None);
        let plan = operation.plan();
        let error = operation.await.unwrap_err();
        assert!(matches!(error, OperationError::Step { ref label, .. } if label == "fail"));
        assert!(!root.path().join(".staging").join(plan.to_string()).exists());
    }

    #[tokio::test]
    async fn cancellation_stops_running_steps() {
        let root = tempfile::tempdir().unwrap();
        let mut steps = StepGraph::new();
        steps
            .add("wait", Step::Custom(Arc::new(WaitForCancel)), &[])
            .unwrap();
        let operation =
            operations(root.path()).execute(Plan::<Install>::new((), steps, Vec::new()), None);
        tokio::time::sleep(Duration::from_millis(20)).await;
        operation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(5), operation)
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, OperationError::Cancelled));
    }

    #[tokio::test]
    async fn refuses_stale_plans() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("instance.cfg"), "name=changed\n").unwrap();
        let hash = FileHash::new("sha1", "a9993e364706816aba3e25717850c26c9cd0d89d").unwrap();
        let plan = Plan::<Install>::new(
            (),
            staged_install(),
            vec![Precondition::FileUnchanged {
                path: PlanPath::target(rel("instance.cfg")),
                hash: Some(hash),
            }],
        );
        let error = operations(root.path())
            .execute(plan, Some(target.path().to_path_buf()))
            .await
            .unwrap_err();
        assert!(matches!(error, OperationError::PlanStale { .. }));
        assert!(!target.path().join("mods").exists());
    }

    struct CountingDownloads(AtomicUsize);

    impl DownloadHandler for CountingDownloads {
        fn download<'a>(
            &'a self,
            step: &'a DownloadStep,
            cx: &'a StepContext,
        ) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                cx.report_progress(3, Some(3));
                let path = cx.resolve(&step.dest)?;
                cx.storage().write_atomic(&path, "abc".into()).await?;
                Ok(StepOutcome::Succeeded)
            })
        }
    }

    #[tokio::test]
    async fn resumes_from_the_journal_after_a_crash() {
        let root = tempfile::tempdir().unwrap();
        let mut steps = StepGraph::new();
        for name in ["a", "b"] {
            steps
                .add(
                    format!("download {name}"),
                    Step::Download(DownloadStep {
                        sources: Sources::single(
                            crate::HttpUrl::new(format!("https://example.com/{name}")).unwrap(),
                        ),
                        expect: Expectation::Unverified { size: None },
                        dest: PlanPath::data(rel(name)),
                        kind: crate::ArtifactKind::Library,
                    }),
                    &[],
                )
                .unwrap();
        }
        let plan = Plan::<Install>::new((), steps, Vec::new());
        // Simulate a crash after step 0 finished: the saved Plan and journal stay behind.
        let staging = journal::staging_dir(root.path());
        std::fs::create_dir_all(staging.join(plan.id().to_string())).unwrap();
        std::fs::write(
            journal::plan_file(root.path(), plan.id()),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        std::fs::write(journal::journal_file(root.path(), plan.id()), "done 0\n").unwrap();

        let downloads = Arc::new(CountingDownloads(AtomicUsize::new(0)));
        let operations = operations(root.path()).with_download_handler(downloads.clone());
        assert_eq!(operations.pending().await.unwrap(), [plan.id()]);
        let finished = operations
            .resume::<Install>(plan.id(), None)
            .await
            .unwrap()
            .await
            .unwrap();
        assert_eq!(finished, 2);
        assert_eq!(downloads.0.load(Ordering::SeqCst), 1);
        assert_eq!(operations.pending().await.unwrap(), Vec::new());
        assert!(matches!(
            operations.resume::<Install>(plan.id(), None).await,
            Err(OperationError::NotResumable { .. })
        ));
    }

    #[tokio::test]
    async fn reports_missing_handlers_and_targets() {
        let root = tempfile::tempdir().unwrap();
        let error = operations(root.path())
            .execute(Plan::<Install>::new((), staged_install(), Vec::new()), None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationError::Step {
                source: StepError::MissingTarget,
                ..
            }
        ));
    }
}
