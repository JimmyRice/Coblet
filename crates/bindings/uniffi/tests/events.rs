use std::time::SystemTime;

use coblet::runtime::{
    Event, GameLogLine, LogLevel, LogSource, OperationId, ProcessExit, TransferKind as Kind,
    TransferSnapshot,
};

use crate::{
    CobletBuilder, CobletError, CobletEvent, ErrorKind, LaunchMode, LaunchOptions, TransferKind,
    generate_operation_id,
};

fn coblet(dir: &tempfile::TempDir) -> std::sync::Arc<crate::Coblet> {
    CobletBuilder::new(
        dir.path().to_string_lossy().into_owned(),
        "coblet-uniffi-test/1.0".to_owned(),
    )
    .unwrap()
    .build()
    .unwrap()
}

#[tokio::test]
async fn streams_engine_events_with_their_launch_id() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = coblet(&dir);
    let stream = coblet.subscribe();
    let id = generate_operation_id();
    let op: OperationId = id.parse().unwrap();
    let bus = coblet.inner.events();
    bus.publish(Event::Transfer {
        op,
        kind: Kind::Assets,
        progress: TransferSnapshot {
            files_done: 3,
            files_total: 9,
            bytes_done: 30,
            bytes_total: Some(90),
        },
    });
    let mut line = GameLogLine::plain(LogSource::Stdout, LogLevel::Info, "hello");
    line.time = Some(SystemTime::UNIX_EPOCH);
    bus.publish(Event::GameLog { op, line });
    bus.publish(Event::GameExited {
        op,
        exit: Some(ProcessExit {
            code: Some(1),
            signal: None,
        }),
        killed: false,
        suspected_crash: true,
    });

    assert_eq!(
        stream.next().await.unwrap(),
        CobletEvent::Transfer {
            op: id.clone(),
            kind: TransferKind::Assets,
            files_done: 3,
            files_total: 9,
            bytes_done: 30,
            bytes_total: Some(90),
        }
    );
    let CobletEvent::GameLog { op: from, line } = stream.next().await.unwrap() else {
        panic!("expected a log line");
    };
    assert_eq!(from, id);
    assert_eq!(
        (line.raw.as_str(), line.time),
        ("hello", Some(SystemTime::UNIX_EPOCH))
    );
    assert!(matches!(
        stream.next().await.unwrap(),
        CobletEvent::GameExited {
            code: Some(1),
            suspected_crash: true,
            ..
        }
    ));
}

#[tokio::test]
async fn slow_readers_learn_how_much_they_missed() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = coblet(&dir);
    let stream = coblet.subscribe();
    let op = OperationId::random();
    for step in 0..5000_u32 {
        coblet.inner.events().publish(Event::LaunchStep {
            op,
            step: "spawn".to_owned(),
            index: step,
            count: 5000,
        });
    }
    assert!(matches!(
        stream.next().await.unwrap(),
        CobletEvent::Missed { count } if count > 0
    ));
}

#[tokio::test]
async fn launches_reject_malformed_operation_ids() {
    let dir = tempfile::tempdir().unwrap();
    let options = LaunchOptions {
        player_name: "Steve".to_owned(),
        mode: LaunchMode::Offline,
        game_dir: None,
        customizers: Vec::new(),
        operation_id: Some("not-a-uuid".to_owned()),
    };
    let error = coblet(&dir)
        .launch_with("1.20.1".to_owned(), options)
        .await
        .unwrap_err();
    let CobletError::Failed { kind, .. } = error;
    assert_eq!(kind, ErrorKind::InvalidArgument);
}
