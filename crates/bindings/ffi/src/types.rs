use std::ffi::{CStr, c_char};

use coblet::SdkError;

/// Version of the C ABI; it grows with every incompatible change.
pub const COBLET_ABI_VERSION: u32 = 1;

/// Result of a call.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletStatus {
    /// The call succeeded.
    Ok = 0,
    /// The call failed; see the error.
    Error = 1,
    /// An argument was NULL or not valid UTF-8.
    InvalidArgument = 2,
    /// A panic was caught; the library state may be inconsistent.
    Panic = 3,
    /// A stream has no more items.
    End = 4,
}

/// Category of a [`CobletError`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletErrorKind {
    /// Anything else.
    Other = 0,
    /// An argument was NULL, malformed or not valid UTF-8.
    InvalidArgument = 1,
    /// Metadata or files could not be fetched.
    Network = 2,
    /// A version or file does not exist.
    NotFound = 3,
    /// The version cannot run on this platform.
    Unsupported = 4,
    /// A launch step failed.
    Launch = 5,
    /// A handle was used in the wrong order, such as after `coblet_game_wait`.
    InvalidState = 6,
    /// A panic was caught.
    Panic = 7,
}

/// UTF-8 string owned by the library; `ptr` is NUL-terminated and `len`
/// excludes the NUL. Free it with `coblet_string_free`.
#[repr(C)]
#[derive(Debug)]
pub struct CobletString {
    /// The bytes, or NULL for an empty value.
    pub ptr: *mut c_char,
    /// Length in bytes without the terminating NUL.
    pub len: usize,
}

impl CobletString {
    /// Returns an empty value that needs no freeing.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }

    /// Copies `text`; interior NUL bytes are replaced with U+FFFD.
    pub(crate) fn new(text: &str) -> Self {
        let mut bytes = text.replace('\0', "\u{fffd}").into_bytes();
        let len = bytes.len();
        bytes.push(0);
        let boxed = bytes.into_boxed_slice();
        Self {
            ptr: Box::into_raw(boxed).cast::<c_char>(),
            len,
        }
    }

    /// Frees the bytes.
    ///
    /// # Safety
    ///
    /// The value must come from [`CobletString::new`] and must not be freed twice.
    #[allow(unsafe_code, reason = "reclaims memory handed to C")]
    pub(crate) unsafe fn free(self) {
        if self.ptr.is_null() {
            return;
        }
        let slice = std::ptr::slice_from_raw_parts_mut(self.ptr.cast::<u8>(), self.len + 1);
        // SAFETY: the pointer and length come from a boxed slice of `len + 1`
        // bytes created in `new`, and the caller frees it only once.
        drop(unsafe { Box::from_raw(slice) });
    }
}

/// Error returned through `out_err`; free it with `coblet_error_free`.
#[repr(C)]
#[derive(Debug)]
pub struct CobletError {
    /// The category.
    pub kind: CobletErrorKind,
    /// Human-readable message.
    pub message: CobletString,
}

/// Error inside the library before it is handed to C.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) status: CobletStatus,
    pub(crate) kind: CobletErrorKind,
    pub(crate) message: String,
}

impl Failure {
    pub(crate) fn new(kind: CobletErrorKind, message: impl Into<String>) -> Self {
        let status = match kind {
            CobletErrorKind::InvalidArgument => CobletStatus::InvalidArgument,
            CobletErrorKind::Panic => CobletStatus::Panic,
            _ => CobletStatus::Error,
        };
        Self {
            status,
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn argument(name: &str, problem: &str) -> Self {
        Self::new(
            CobletErrorKind::InvalidArgument,
            format!("{name} {problem}"),
        )
    }
}

impl From<SdkError> for Failure {
    fn from(error: SdkError) -> Self {
        let kind = match &error {
            SdkError::Config(_) => CobletErrorKind::InvalidArgument,
            SdkError::Transport(_) | SdkError::Catalog(_) => CobletErrorKind::Network,
            SdkError::UnknownVersion(_) => CobletErrorKind::NotFound,
            SdkError::Profile(_) => CobletErrorKind::Unsupported,
            SdkError::Launch(_) => CobletErrorKind::Launch,
            _ => CobletErrorKind::Other,
        };
        Self::new(kind, error.to_string())
    }
}

/// Reads a required UTF-8 argument.
///
/// # Safety
///
/// `ptr` must be NULL or point to a NUL-terminated string that stays valid
/// for the call.
#[allow(unsafe_code, reason = "reads a C string argument")]
pub(crate) unsafe fn text<'a>(ptr: *const c_char, name: &str) -> Result<&'a str, Failure> {
    if ptr.is_null() {
        return Err(Failure::argument(name, "is NULL"));
    }
    // SAFETY: the caller passes a valid NUL-terminated string, checked for NULL above.
    let text = unsafe { CStr::from_ptr(ptr) };
    text.to_str()
        .map_err(|_| Failure::argument(name, "is not valid UTF-8"))
}

/// Reads an optional UTF-8 argument; NULL gives `None`.
///
/// # Safety
///
/// As for [`text`].
#[allow(unsafe_code, reason = "reads a C string argument")]
pub(crate) unsafe fn optional_text<'a>(
    ptr: *const c_char,
    name: &str,
) -> Result<Option<&'a str>, Failure> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { text(ptr, name) }.map(Some)
}
