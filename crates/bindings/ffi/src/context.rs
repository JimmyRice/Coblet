use std::ffi::c_char;
use std::sync::Arc;

use coblet::catalog::Filter;
use coblet::runtime::UserAgent;
use tokio::runtime::Runtime;

use crate::customize::{CobletCustomizer, ForeignCustomizer};
use crate::guard::{guard, put};
use crate::types::{
    COBLET_ABI_VERSION, CobletError, CobletErrorKind, CobletStatus, CobletString, Failure, text,
};

/// Library handle: the Rust `Coblet` and the runtime its work runs on.
pub struct Coblet {
    pub(crate) runtime: Runtime,
    pub(crate) inner: coblet::Coblet,
}

/// Returns the ABI version the library implements.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub extern "C" fn coblet_abi_version() -> u32 {
    COBLET_ABI_VERSION
}

/// Creates a `Coblet` handle with the default transport.
///
/// `data_root` is the directory for shared game files; `user_agent`
/// identifies the launcher to web services, such as
/// `MyLauncher/1.0 (contact@example.com)`. On success `*out_coblet` receives a
/// handle to free with `coblet_free`.
///
/// # Safety
///
/// String arguments must be NULL or NUL-terminated; `out_coblet` must be
/// writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_new(
    data_root: *const c_char,
    user_agent: *const c_char,
    out_coblet: *mut *mut Coblet,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees; no customizers.
    unsafe {
        coblet_new_with_customizers(
            data_root,
            user_agent,
            std::ptr::null(),
            0,
            out_coblet,
            out_err,
        )
    }
}

/// Creates a `Coblet` handle with the default transport and `count` launch customizers.
///
/// As `coblet_new`; a customizer runs only for launches that name its
/// ID in `coblet_launch_customized`. The library owns the user data of
/// every table from this call on, even when it fails, and calls each
/// `free` once: on failure before returning, otherwise from
/// `coblet_free`.
///
/// # Safety
///
/// As `coblet_new`; `customizers` must point to `count` tables, or be
/// NULL when `count` is 0, and their callbacks must stay valid until their
/// `free` runs.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_new_with_customizers(
    data_root: *const c_char,
    user_agent: *const c_char,
    customizers: *const CobletCustomizer,
    count: usize,
    out_coblet: *mut *mut Coblet,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    let tables: &[CobletCustomizer] = if customizers.is_null() || count == 0 {
        &[]
    } else {
        // SAFETY: the caller passes `count` tables at `customizers`.
        unsafe { std::slice::from_raw_parts(customizers, count) }
    };
    // Take ownership of every table first, so each `free` runs once even
    // when a later table or argument is rejected.
    let adapters: Vec<Result<ForeignCustomizer, Failure>> = tables
        .iter()
        // SAFETY: forwarded from the caller's guarantees.
        .map(|table| unsafe { ForeignCustomizer::new(table) })
        .collect();
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let adapters = adapters.into_iter().collect::<Result<Vec<_>, _>>()?;
            let data_root = text(data_root, "data_root")?;
            let user_agent = UserAgent::new(text(user_agent, "user_agent")?)
                .map_err(|error| Failure::argument("user_agent", &error.to_string()))?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("coblet")
                .build()
                .map_err(|error| Failure::new(CobletErrorKind::Other, error.to_string()))?;
            let mut builder = coblet::Coblet::builder()
                .data_root(data_root)
                .user_agent(user_agent);
            for adapter in adapters {
                builder = builder.customizer(Arc::new(adapter));
            }
            let inner = runtime.block_on(async { builder.build() })?;
            let handle = Box::into_raw(Box::new(Coblet { runtime, inner }));
            if let Err(failure) = put(out_coblet, handle, "out_coblet") {
                drop(Box::from_raw(handle));
                return Err(failure);
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Frees a `Coblet` handle; NULL is ignored.
///
/// # Safety
///
/// `coblet` must be NULL or a handle from `coblet_new` that is not used afterwards.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_free(coblet: *mut Coblet) {
    if !coblet.is_null() {
        // SAFETY: the handle came from `Box::into_raw` in `coblet_new`.
        drop(unsafe { Box::from_raw(coblet) });
    }
}

/// Lists game versions as JSON, newest first.
///
/// The document is `{"schema":1,"versions":[{"id","type","released",
/// "latest_release"}]}`. On success `*out_json` receives a string to free
/// with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err` must
/// be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_versions_json(
    coblet: *const Coblet,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let versions = handle
                .runtime
                .block_on(handle.inner.versions(&Filter::new()))?;
            let list: Vec<serde_json::Value> = versions
                .iter()
                .map(|version| {
                    serde_json::json!({
                        "id": version.id.as_str(),
                        "type": version.version_type.as_str(),
                        "released": version.released.to_string(),
                        "latest_release": version.latest_release,
                    })
                })
                .collect();
            let json = serde_json::json!({ "schema": 1, "versions": list }).to_string();
            put(out_json, CobletString::new(&json), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Frees a string returned by the library; an empty value is ignored.
///
/// # Safety
///
/// `value` must come from this library and must not be freed twice.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_string_free(value: CobletString) {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { value.free() }
}

/// Frees an error; NULL is ignored.
///
/// # Safety
///
/// `error` must be NULL or come from this library, and must not be freed twice.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_error_free(error: *mut CobletError) {
    if error.is_null() {
        return;
    }
    // SAFETY: the error was created with `Box::into_raw` in `guard`.
    let error = unsafe { Box::from_raw(error) };
    // SAFETY: the message belongs to this error and is freed once.
    unsafe { error.message.free() };
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::{coblet_abi_version, coblet_error_free, coblet_free, coblet_new};
    use crate::types::{CobletErrorKind, CobletStatus, CobletString};

    #[test]
    #[allow(unsafe_code, reason = "calls the C ABI")]
    fn creates_and_frees_an_sdk_and_reports_bad_arguments() {
        assert_eq!(coblet_abi_version(), 2);
        let dir = tempfile::tempdir().unwrap();
        let root = CString::new(dir.path().to_str().unwrap()).unwrap();
        let agent = CString::new("coblet-ffi-test/1.0").unwrap();

        let mut coblet = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        // SAFETY: valid strings and out-pointers.
        let status =
            unsafe { coblet_new(root.as_ptr(), agent.as_ptr(), &raw mut coblet, &raw mut err) };
        assert_eq!(status, CobletStatus::Ok);
        assert!(!coblet.is_null());
        assert!(err.is_null());
        // SAFETY: the handle is live and not used afterwards.
        unsafe { coblet_free(coblet) };

        let mut coblet = std::ptr::null_mut();
        // SAFETY: a NULL data root is reported, not dereferenced.
        let status = unsafe {
            coblet_new(
                std::ptr::null(),
                agent.as_ptr(),
                &raw mut coblet,
                &raw mut err,
            )
        };
        assert_eq!(status, CobletStatus::InvalidArgument);
        assert!(coblet.is_null());
        // SAFETY: the error was just returned by the library.
        let error = unsafe { &*err };
        assert_eq!(error.kind, CobletErrorKind::InvalidArgument);
        // SAFETY: the message is NUL-terminated and owned by the error.
        let message = unsafe { std::ffi::CStr::from_ptr(error.message.ptr) };
        assert_eq!(message.to_str().unwrap(), "data_root is NULL");
        // SAFETY: freed once.
        unsafe { coblet_error_free(err) };
    }

    #[test]
    #[allow(unsafe_code, reason = "frees library strings")]
    fn strings_replace_interior_nul() {
        let value = CobletString::new("a\0b");
        assert_eq!(value.len, 5);
        // SAFETY: created above and freed once.
        unsafe { value.free() };
        // SAFETY: an empty value owns nothing.
        unsafe { CobletString::empty().free() };
    }
}
