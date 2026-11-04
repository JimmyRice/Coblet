use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::types::{CobletError, CobletErrorKind, CobletStatus, CobletString, Failure};

/// Runs an exported function body: catches panics and reports failures through `out_err`.
///
/// # Safety
///
/// `out_err` must be NULL or point to writable memory for one pointer.
#[allow(unsafe_code, reason = "writes the error out-parameter")]
pub(crate) unsafe fn guard(
    out_err: *mut *mut CobletError,
    body: impl FnOnce() -> Result<CobletStatus, Failure>,
) -> CobletStatus {
    let failure = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(status)) => return status,
        Ok(Err(failure)) => failure,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic without a message".to_owned());
            Failure::new(CobletErrorKind::Panic, format!("panic: {message}"))
        }
    };
    if !out_err.is_null() {
        let error = Box::new(CobletError {
            kind: failure.kind,
            message: CobletString::new(&failure.message),
        });
        // SAFETY: the caller guarantees `out_err` is writable when not NULL.
        unsafe { out_err.write(Box::into_raw(error)) };
    }
    failure.status
}

/// Writes an output value.
///
/// # Safety
///
/// `out` must be NULL or point to writable memory for one `T`.
#[allow(unsafe_code, reason = "writes an out-parameter")]
pub(crate) unsafe fn put<T>(out: *mut T, value: T, name: &str) -> Result<(), Failure> {
    if out.is_null() {
        return Err(Failure::argument(name, "is NULL"));
    }
    // SAFETY: checked for NULL; the caller guarantees it is writable.
    unsafe { out.write(value) };
    Ok(())
}
