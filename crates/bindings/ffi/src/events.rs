use std::ffi::c_char;
use std::time::Duration;

use coblet::runtime::{
    Event, EventSubscription, OperationId, OperationOutcome, Received, StepOutcome, TransferKind,
};

use crate::context::Coblet;
use crate::game::{COBLET_OPERATION_ID_LEN, log_json};
use crate::guard::{guard, put};
use crate::login::prompt_json;
use crate::types::{CobletError, CobletStatus, CobletString, Failure};

/// Kind of a [`CobletEvent`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletEventKind {
    /// An Operation or a launch started; `json` is `{"schema":1,
    /// "operation_kind"}` with `operation_kind` such as `launch`.
    OperationStarted = 0,
    /// An Operation or a launch ended (a launch ends once the game runs);
    /// `json` is `{"schema":1,"outcome","message"}` with `outcome`
    /// `succeeded`, `failed` or `cancelled`.
    OperationFinished = 1,
    /// A launch began a step; `json` is `{"schema":1,"step","index","count"}`.
    LaunchStep = 2,
    /// A batch of files made progress; read `transfer`, `files_*` and
    /// `bytes_*`. No `json`.
    Transfer = 3,
    /// A step of an Operation made progress; read `step`, `bytes_done` and
    /// `bytes_total`. No `json`.
    StepProgress = 4,
    /// A step of an Operation ended; `step` is set and `json` is
    /// `{"schema":1,"outcome","message"}` with `outcome` `succeeded`,
    /// `skipped`, `failed` or `cancelled`.
    StepFinished = 5,
    /// The game started; `json` is `{"schema":1,"pid"}`.
    GameStarted = 6,
    /// A game log line; `json` is the document of `coblet_game_next_log`.
    GameLog = 7,
    /// The game ended; `json` is `{"schema":1,"code","signal","killed",
    /// "suspected_crash"}`.
    GameExited = 8,
    /// Something went wrong without stopping the work; `json` is
    /// `{"schema":1,"code","message"}`.
    Warning = 9,
    /// A login waits for the user; `json` is the document of
    /// `coblet_login_prompt_json`.
    LoginPrompt = 10,
    /// The reader fell behind; `missed` events were dropped. No `json`.
    Missed = 11,
}

/// Group of files a transfer fetches.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CobletTransferKind {
    /// Anything else, or not a transfer.
    #[default]
    Other = 0,
    /// Libraries, the client jar, the asset index and the log configuration.
    GameFiles = 1,
    /// Asset objects: sounds, languages and textures.
    Assets = 2,
    /// Files of a Java runtime.
    JavaRuntime = 3,
}

/// One event, in a fixed layout so frequent progress events need no
/// allocation.
///
/// Fields a kind does not use are zero. Free `json` with
/// `coblet_event_clear` before the struct is reused or dropped.
#[repr(C)]
#[derive(Debug)]
pub struct CobletEvent {
    /// What happened.
    pub kind: CobletEventKind,
    /// Operation or launch ID with its NUL, or an empty string.
    pub op: [c_char; COBLET_OPERATION_ID_LEN],
    /// Files of a `COBLET_EVENT_KIND_TRANSFER` event.
    pub transfer: CobletTransferKind,
    /// Files finished.
    pub files_done: u64,
    /// Files in the batch.
    pub files_total: u64,
    /// Bytes received or found present; units done for step progress.
    pub bytes_done: u64,
    /// Bytes in the batch, when `has_bytes_total`.
    pub bytes_total: u64,
    /// Whether `bytes_total` is known.
    pub has_bytes_total: bool,
    /// Step index of `STEP_PROGRESS` and `STEP_FINISHED` events.
    pub step: u32,
    /// Dropped events of a `MISSED` event.
    pub missed: u64,
    /// Details as JSON, or an empty string for the kinds without any.
    pub json: CobletString,
}

impl CobletEvent {
    fn new(kind: CobletEventKind, op: Option<OperationId>) -> Self {
        let mut text = [0; COBLET_OPERATION_ID_LEN];
        if let Some(op) = op {
            for (slot, byte) in text.iter_mut().zip(op.to_string().bytes()) {
                *slot = c_char::from_ne_bytes([byte]);
            }
        }
        Self {
            kind,
            op: text,
            transfer: CobletTransferKind::Other,
            files_done: 0,
            files_total: 0,
            bytes_done: 0,
            bytes_total: 0,
            has_bytes_total: false,
            step: 0,
            missed: 0,
            json: CobletString::empty(),
        }
    }

    fn with_json(mut self, json: &serde_json::Value) -> Self {
        self.json = CobletString::new(&json.to_string());
        self
    }
}

fn operation_outcome(outcome: OperationOutcome) -> serde_json::Value {
    let (name, message) = match outcome {
        OperationOutcome::Succeeded => ("succeeded", None),
        OperationOutcome::Failed { message } => ("failed", Some(message)),
        _ => ("cancelled", None),
    };
    serde_json::json!({ "schema": 1, "outcome": name, "message": message })
}

fn step_outcome(outcome: StepOutcome) -> serde_json::Value {
    let (name, message) = match outcome {
        StepOutcome::Succeeded => ("succeeded", None),
        StepOutcome::Skipped => ("skipped", None),
        StepOutcome::Failed { message } => ("failed", Some(message)),
        _ => ("cancelled", None),
    };
    serde_json::json!({ "schema": 1, "outcome": name, "message": message })
}

/// Converts an engine event; `None` for kinds this ABI does not know.
fn convert(event: Event) -> Option<CobletEvent> {
    use CobletEventKind as Kind;
    let op = event.op();
    let new = |kind| CobletEvent::new(kind, op);
    Some(match event {
        Event::OperationStarted { kind, .. } => new(Kind::OperationStarted)
            .with_json(&serde_json::json!({ "schema": 1, "operation_kind": kind })),
        Event::OperationFinished { outcome, .. } => {
            new(Kind::OperationFinished).with_json(&operation_outcome(outcome))
        }
        Event::LaunchStep {
            step, index, count, ..
        } => new(Kind::LaunchStep).with_json(&serde_json::json!({
            "schema": 1, "step": step, "index": index, "count": count,
        })),
        Event::Transfer { kind, progress, .. } => CobletEvent {
            transfer: match kind {
                TransferKind::GameFiles => CobletTransferKind::GameFiles,
                TransferKind::Assets => CobletTransferKind::Assets,
                TransferKind::JavaRuntime => CobletTransferKind::JavaRuntime,
                _ => CobletTransferKind::Other,
            },
            files_done: progress.files_done,
            files_total: progress.files_total,
            bytes_done: progress.bytes_done,
            bytes_total: progress.bytes_total.unwrap_or(0),
            has_bytes_total: progress.bytes_total.is_some(),
            ..new(Kind::Transfer)
        },
        Event::StepProgress {
            step, done, total, ..
        } => CobletEvent {
            step: step.index(),
            bytes_done: done,
            bytes_total: total.unwrap_or(0),
            has_bytes_total: total.is_some(),
            ..new(Kind::StepProgress)
        },
        Event::StepFinished { step, outcome, .. } => CobletEvent {
            step: step.index(),
            ..new(Kind::StepFinished).with_json(&step_outcome(outcome))
        },
        Event::GameStarted { pid, .. } => {
            new(Kind::GameStarted).with_json(&serde_json::json!({ "schema": 1, "pid": pid }))
        }
        Event::GameLog { line, .. } => new(Kind::GameLog).with_json(&log_json(&line)),
        Event::GameExited {
            exit,
            killed,
            suspected_crash,
            ..
        } => new(Kind::GameExited).with_json(&serde_json::json!({
            "schema": 1,
            "code": exit.and_then(|exit| exit.code),
            "signal": exit.and_then(|exit| exit.signal),
            "killed": killed,
            "suspected_crash": suspected_crash,
        })),
        Event::Warning(warning) => new(Kind::Warning).with_json(&serde_json::json!({
            "schema": 1, "code": warning.code.as_str(), "message": warning.message,
        })),
        Event::AuthPrompt(prompt) => new(Kind::LoginPrompt).with_json(&prompt_json(&prompt)),
        _ => return None,
    })
}

/// Event subscription handle.
pub struct CobletEvents {
    runtime: tokio::runtime::Handle,
    inner: EventSubscription,
}

/// Starts receiving events published from now on: launch steps and file
/// transfers, game starts, log lines and exits, warnings and login
/// prompts. On success `*out_events` receives a handle to free with
/// `coblet_events_free`.
///
/// Events are built only while a subscription exists, and progress is
/// throttled to 30 events per second per batch; a newer transfer or step
/// progress replaces the unread one of the same batch. A reader that still
/// falls behind loses game log copies first, then progress, and only then
/// other events, and gets a `COBLET_EVENT_KIND_MISSED` event with the
/// count instead of slowing coblet down. Read on a thread of your own while
/// a blocking call such as `coblet_launch` runs.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_events` must be writable;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_events_subscribe(
    coblet: *const Coblet,
    out_events: *mut *mut CobletEvents,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            if out_events.is_null() {
                return Err(Failure::argument("out_events", "is NULL"));
            }
            let events = Box::into_raw(Box::new(CobletEvents {
                runtime: handle.runtime.handle().clone(),
                inner: handle.inner.subscribe(),
            }));
            put(out_events, events, "out_events")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Waits for the next event and writes it to `*out_event`.
///
/// `timeout_ms` is how long to wait: 0 only checks, a negative value waits
/// without limit. Returns `COBLET_STATUS_TIMEOUT` when nothing arrived in
/// time and `COBLET_STATUS_END` once the `Coblet` is gone. The call blocks,
/// so run it on a thread of your own, never from a coblet callback; one
/// thread at a time may read a subscription.
///
/// # Safety
///
/// `events` must be a live handle used by one thread at a time;
/// `out_event` must be writable and hold no `json` that still needs
/// freeing; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_events_next(
    events: *mut CobletEvents,
    timeout_ms: i64,
    out_event: *mut CobletEvent,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let events = events
                .as_mut()
                .ok_or_else(|| Failure::argument("events", "is NULL"))?;
            if out_event.is_null() {
                return Err(Failure::argument("out_event", "is NULL"));
            }
            let limit = u64::try_from(timeout_ms).ok().map(Duration::from_millis);
            let CobletEvents { runtime, inner } = events;
            let event = runtime.block_on(async {
                loop {
                    let received = match limit {
                        Some(limit) => match tokio::time::timeout(limit, inner.recv()).await {
                            Ok(received) => received,
                            Err(_) => return Err(CobletStatus::Timeout),
                        },
                        None => inner.recv().await,
                    };
                    match received {
                        None => return Err(CobletStatus::End),
                        Some(Received::Missed(count)) => {
                            let mut event = CobletEvent::new(CobletEventKind::Missed, None);
                            event.missed = count;
                            return Ok(event);
                        }
                        Some(Received::Event(event)) => {
                            if let Some(event) = convert(event) {
                                return Ok(event);
                            }
                        }
                    }
                }
            });
            match event {
                Ok(event) => {
                    put(out_event, event, "out_event")?;
                    Ok(CobletStatus::Ok)
                }
                Err(status) => Ok(status),
            }
        })
    }
}

/// Frees an event's `json` and zeroes the event, so it can be reused.
///
/// # Safety
///
/// `event` must be NULL or point to an event written by
/// `coblet_events_next` or zeroed by this function.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_event_clear(event: *mut CobletEvent) {
    // SAFETY: forwarded from the caller's guarantee.
    let Some(event) = (unsafe { event.as_mut() }) else {
        return;
    };
    let cleared = CobletEvent::new(CobletEventKind::Missed, None);
    let old = std::mem::replace(event, cleared);
    // SAFETY: the string came from coblet_events_next and is freed once.
    unsafe { old.json.free() };
}

/// Ends a subscription.
///
/// # Safety
///
/// `events` must be NULL or a live handle not used afterwards.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_events_free(events: *mut CobletEvents) {
    if events.is_null() {
        return;
    }
    // SAFETY: the handle was created with Box::into_raw in coblet_events_subscribe.
    drop(unsafe { Box::from_raw(events) });
}

#[cfg(test)]
mod tests {
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
}
