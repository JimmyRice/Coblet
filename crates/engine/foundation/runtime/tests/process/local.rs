use std::time::Duration;

use super::TokioSpawner;
use crate::{ProcessError, ProcessSpawner, ProcessSpec};

#[tokio::test]
async fn runs_and_captures_a_process() {
    let output = TokioSpawner::new()
        .spawn(
            ProcessSpec::new("/bin/sh")
                .arg("-c")
                .arg("printf \"$GREETING\"; printf oops >&2; exit 3")
                .env("GREETING", "hi"),
        )
        .unwrap()
        .capture(1024)
        .await
        .unwrap();
    assert_eq!(output.exit.code, Some(3));
    assert_eq!(&output.stdout[..], b"hi");
    assert_eq!(&output.stderr[..], b"oops");
}

#[tokio::test]
async fn kills_a_running_process() {
    let process = TokioSpawner::new()
        .spawn(ProcessSpec::new("/bin/sleep").arg("30"))
        .unwrap();
    assert!(process.id().is_some());
    process.kill_handle().kill();
    let exit = tokio::time::timeout(Duration::from_secs(5), process.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!exit.success());
}

#[test]
fn reports_missing_programs() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let error = TokioSpawner::new()
        .spawn(ProcessSpec::new("/nonexistent/java"))
        .unwrap_err();
    assert!(matches!(error, ProcessError::Spawn { .. }));
}
