use std::ffi::CString;

use super::{coblet_abi_version, coblet_error_free, coblet_free, coblet_new};
use crate::types::{CobletErrorKind, CobletStatus, CobletString};

#[test]
#[allow(unsafe_code, reason = "calls the C ABI")]
fn creates_and_frees_an_sdk_and_reports_bad_arguments() {
    assert_eq!(coblet_abi_version(), 11);
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
#[allow(unsafe_code, reason = "frees library bytes")]
fn bytes_round_trip() {
    let value = crate::types::CobletBytes::new(vec![1, 2, 3]);
    assert_eq!(value.len, 3);
    // SAFETY: the pointer covers the 3 bytes created above.
    let bytes = unsafe { std::slice::from_raw_parts(value.ptr, value.len) };
    assert_eq!(bytes, [1, 2, 3]);
    // SAFETY: created above and freed once.
    unsafe { value.free() };
    assert!(crate::types::CobletBytes::new(Vec::new()).ptr.is_null());
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
