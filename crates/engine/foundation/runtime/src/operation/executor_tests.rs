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
    fn run<'a>(&'a self, _cx: &'a StepContext) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
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
