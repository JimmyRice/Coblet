use std::ffi::c_char;

use coblet::catalog::Filter;
use coblet::types::{Libc, Platform, PlatformTrait};
use tokio::runtime::Runtime;

use crate::builder::{coblet_builder_build, coblet_builder_new};
use crate::guard::{guard, put};
use crate::types::{COBLET_ABI_VERSION, CobletError, CobletStatus, CobletString, Failure};

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

/// Creates a `Coblet` handle with the default transport and nothing added.
///
/// `data_root` is the directory for shared game files; `user_agent`
/// identifies the launcher to web services, such as
/// `MyLauncher/1.0 (contact@example.com)`. On success `*out_coblet`
/// receives a handle to free with `coblet_free`. Use `coblet_builder_new`
/// to add Platform Profiles or launch customizers.
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
    let mut builder = std::ptr::null_mut();
    // SAFETY: forwarded from the caller's guarantees.
    let status = unsafe { coblet_builder_new(data_root, user_agent, &raw mut builder, out_err) };
    if status != CobletStatus::Ok {
        return status;
    }
    // SAFETY: the builder was just created and is not used afterwards.
    unsafe { coblet_builder_build(builder, out_coblet, out_err) }
}

/// Describes a platform as `{"os","arch","os_version","libc","traits":[]}`.
pub(crate) fn platform_value(platform: &Platform) -> serde_json::Value {
    let traits: Vec<&str> = platform
        .traits()
        .iter()
        .map(PlatformTrait::as_str)
        .collect();
    serde_json::json!({
        "os": platform.os().as_str(),
        "arch": platform.arch().as_str(),
        "os_version": platform.os_version(),
        "libc": platform.libc().map(Libc::as_str),
        "traits": traits,
    })
}

/// Returns the platform launches are adapted to as JSON, as Platform
/// Profiles match it: detected from the build target, or set with
/// `coblet_builder_set_platform`.
///
/// The document is `{"schema":1,"os","arch","os_version","libc","traits":[]}`.
/// On success `*out_json` receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_platform_json(
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
            let mut json = platform_value(handle.inner.platform());
            if let Some(map) = json.as_object_mut() {
                map.insert("schema".into(), 1.into());
            }
            put(out_json, CobletString::new(&json.to_string()), "out_json")?;
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
#[path = "../tests/context.rs"]
mod tests;
