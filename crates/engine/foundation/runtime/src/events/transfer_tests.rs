use std::time::Duration;

use super::{TransferKind, TransferProgress, TransferSnapshot};
use crate::{Event, EventBus, OperationId, Received};

#[test]
fn counts_files_and_bytes() {
    let progress = TransferProgress::new();
    progress.add_file(Some(10));
    progress.add_file(None);
    progress.add_bytes(7);
    progress.remove_bytes(9);
    progress.add_bytes(4);
    progress.file_done();
    assert_eq!(
        progress.clone().snapshot(),
        TransferSnapshot {
            files_done: 1,
            files_total: 2,
            bytes_done: 4,
            bytes_total: None,
        }
    );
    assert_eq!(TransferKind::JavaRuntime.as_str(), "java-runtime");
}

#[tokio::test(start_paused = true)]
async fn publishes_throttled_progress_and_the_final_totals() {
    let bus = EventBus::new(256);
    let mut events = bus.subscribe();
    let op = OperationId::random();
    let progress = TransferProgress::new();
    let work = {
        let progress = progress.clone();
        async move {
            for _ in 0..100 {
                progress.add_file(Some(1));
            }
            // 100 files over one second: about 30 samples, not 100.
            for _ in 0..100 {
                progress.add_bytes(1);
                progress.file_done();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            42
        }
    };
    let output = progress
        .track(Some(&bus), op, TransferKind::Assets, work)
        .await;
    assert_eq!(output, 42);
    drop(bus);
    let mut seen = Vec::new();
    while let Some(Received::Event(event)) = events.recv().await {
        let Event::Transfer {
            op: from,
            kind,
            progress,
        } = event
        else {
            panic!("unexpected event {event:?}");
        };
        assert_eq!((from, kind), (op, TransferKind::Assets));
        seen.push(progress);
    }
    assert!((25..=40).contains(&seen.len()), "{} events", seen.len());
    assert!(seen.windows(2).all(|pair| pair[0] != pair[1]));
    let last = seen.last().unwrap();
    assert_eq!((last.files_done, last.bytes_done), (100, 100));
    assert_eq!(last.bytes_total, Some(100));
}

#[tokio::test]
async fn stays_quiet_without_listeners_or_files() {
    let bus = EventBus::new(8);
    let progress = TransferProgress::new();
    let op = OperationId::random();
    progress
        .track(Some(&bus), op, TransferKind::Other, async {})
        .await;
    let mut events = bus.subscribe();
    progress
        .track(None, op, TransferKind::Other, async {})
        .await;
    progress
        .track(Some(&bus), op, TransferKind::Other, async {})
        .await;
    drop(bus);
    assert!(events.recv().await.is_none());
}
