#![allow(unsafe_code, reason = "drives the C ABI")]

use std::ffi::{CStr, CString};

use coblet::runtime::{
    Event, GameLogLine, LogLevel, LogSource, OperationId, TransferKind, TransferSnapshot,
};

use super::{
    CobletEvent, CobletEventKind, CobletTransferKind, coblet_event_clear, coblet_events_free,
    coblet_events_next, coblet_events_subscribe,
};
use crate::builder::{coblet_builder_build, coblet_builder_new};
use crate::context::{Coblet, coblet_free};
use crate::game::{COBLET_OPERATION_ID_LEN, coblet_generate_operation_id};
use crate::types::CobletStatus;

fn coblet(dir: &tempfile::TempDir) -> *mut Coblet {
    let root = CString::new(dir.path().to_str().unwrap()).unwrap();
    let agent = CString::new("coblet-ffi-test/1.0").unwrap();
    let mut builder = std::ptr::null_mut();
    let mut coblet = std::ptr::null_mut();
    // SAFETY: valid strings and out-pointers; the builder is consumed.
    unsafe {
        let status = coblet_builder_new(
            root.as_ptr(),
            agent.as_ptr(),
            &raw mut builder,
            std::ptr::null_mut(),
        );
        assert_eq!(status, CobletStatus::Ok);
        let status = coblet_builder_build(builder, &raw mut coblet, std::ptr::null_mut());
        assert_eq!(status, CobletStatus::Ok);
    }
    coblet
}

fn op_text(event: &CobletEvent) -> String {
    // SAFETY: `op` always holds a NUL within its length.
    unsafe { CStr::from_ptr(event.op.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned()
}

fn blank() -> CobletEvent {
    CobletEvent::new(CobletEventKind::Missed, None)
}

#[test]
fn reads_progress_without_json_and_logs_with_json() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = coblet(&dir);
    let mut events = std::ptr::null_mut();
    let mut id = [0; COBLET_OPERATION_ID_LEN];
    // SAFETY: a live handle and writable out-pointers.
    unsafe {
        assert_eq!(
            coblet_events_subscribe(coblet, &raw mut events, std::ptr::null_mut()),
            CobletStatus::Ok
        );
        assert_eq!(
            coblet_generate_operation_id(id.as_mut_ptr(), std::ptr::null_mut()),
            CobletStatus::Ok
        );
    }
    // SAFETY: the buffer was just filled with a NUL-terminated ID.
    let id = unsafe { CStr::from_ptr(id.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned();
    let op: OperationId = id.parse().unwrap();
    // SAFETY: the handle is live; only the engine side is touched.
    let bus = unsafe { &*coblet }.inner.events().clone();
    bus.publish(Event::Transfer {
        op,
        kind: TransferKind::JavaRuntime,
        progress: TransferSnapshot {
            files_done: 1,
            files_total: 4,
            bytes_done: 10,
            bytes_total: None,
        },
    });
    bus.publish(Event::GameLog {
        op,
        line: GameLogLine::plain(LogSource::Stderr, LogLevel::Warn, "careful"),
    });

    let mut event = blank();
    // SAFETY: live handle, writable event without json.
    unsafe {
        assert_eq!(
            coblet_events_next(events, -1, &raw mut event, std::ptr::null_mut()),
            CobletStatus::Ok
        );
    }
    assert_eq!(event.kind, CobletEventKind::Transfer);
    assert_eq!(event.transfer, CobletTransferKind::JavaRuntime);
    assert_eq!(
        (event.files_done, event.files_total, event.bytes_done),
        (1, 4, 10)
    );
    assert!(!event.has_bytes_total && event.json.ptr.is_null());
    assert_eq!(op_text(&event), id);

    // SAFETY: as above; the previous event holds no json.
    unsafe {
        assert_eq!(
            coblet_events_next(events, 1000, &raw mut event, std::ptr::null_mut()),
            CobletStatus::Ok
        );
    }
    assert_eq!(event.kind, CobletEventKind::GameLog);
    // SAFETY: the json string is NUL-terminated and owned by the event.
    let json = unsafe { CStr::from_ptr(event.json.ptr) }.to_str().unwrap();
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(json["message"], "careful");
    assert_eq!(json["raw"], "careful");
    assert!(json["time"].is_null());
    // SAFETY: frees the json once and zeroes the event.
    unsafe { coblet_event_clear(&raw mut event) };
    assert!(event.json.ptr.is_null());

    // SAFETY: as above.
    unsafe {
        assert_eq!(
            coblet_events_next(events, 0, &raw mut event, std::ptr::null_mut()),
            CobletStatus::Timeout
        );
        coblet_events_free(events);
        coblet_free(coblet);
    }
}
