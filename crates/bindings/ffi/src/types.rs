use std::ffi::{CStr, c_char};

use coblet::auth::AuthError;
use coblet::platform::ProfileError;
use coblet::player::PlayerError;

/// Version of the C ABI; it grows with every incompatible change.
pub const COBLET_ABI_VERSION: u32 = 9;

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
    /// Nothing arrived before the timeout.
    Timeout = 5,
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
    /// A login failed or the account has to sign in again.
    Auth = 8,
    /// The account does not own the game; it can launch in demo mode.
    NoLicense = 9,
    /// The account has no player profile yet.
    NoProfile = 10,
    /// The operation was cancelled.
    Cancelled = 11,
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

/// Bytes owned by the library, such as a PNG image. Free them with
/// `coblet_bytes_free`.
#[repr(C)]
#[derive(Debug)]
pub struct CobletBytes {
    /// The bytes, or NULL when there are none.
    pub ptr: *mut u8,
    /// Number of bytes.
    pub len: usize,
}

impl CobletBytes {
    /// Returns an empty value that needs no freeing.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }

    /// Hands `bytes` over to C.
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }
        let boxed = bytes.into_boxed_slice();
        let len = boxed.len();
        Self {
            ptr: Box::into_raw(boxed).cast::<u8>(),
            len,
        }
    }

    /// Frees the bytes.
    ///
    /// # Safety
    ///
    /// The value must come from [`CobletBytes::new`] and must not be freed twice.
    #[allow(unsafe_code, reason = "reclaims memory handed to C")]
    pub(crate) unsafe fn free(self) {
        if self.ptr.is_null() {
            return;
        }
        let slice = std::ptr::slice_from_raw_parts_mut(self.ptr, self.len);
        // SAFETY: the pointer and length come from a boxed slice created in
        // `new`, and the caller frees it only once.
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

impl From<coblet::Error> for Failure {
    fn from(error: coblet::Error) -> Self {
        let kind = match &error {
            coblet::Error::Config(_)
            | coblet::Error::Registry(_)
            | coblet::Error::Profile(ProfileError::InvalidProfile { .. }) => {
                CobletErrorKind::InvalidArgument
            }
            coblet::Error::Transport(_) | coblet::Error::Catalog(_) => CobletErrorKind::Network,
            coblet::Error::UnknownVersion(_) => CobletErrorKind::NotFound,
            coblet::Error::Profile(ProfileError::Failed { .. }) => CobletErrorKind::Other,
            coblet::Error::Profile(_) | coblet::Error::Unsupported(_) => {
                CobletErrorKind::Unsupported
            }
            coblet::Error::Launch(_) => CobletErrorKind::Launch,
            coblet::Error::Auth(error) => auth_kind(error),
            coblet::Error::Player(error) => player_kind(error),
            _ => CobletErrorKind::Other,
        };
        Self::new(kind, error.to_string())
    }
}

impl From<AuthError> for Failure {
    fn from(error: AuthError) -> Self {
        Self::from(coblet::Error::Auth(error))
    }
}

fn auth_kind(error: &AuthError) -> CobletErrorKind {
    match error {
        AuthError::Unsupported(_) => CobletErrorKind::Unsupported,
        AuthError::UnknownMethod(_) | AuthError::UnknownAccount(_) => CobletErrorKind::NotFound,
        AuthError::InvalidName(_)
        | AuthError::InvalidOptions(_)
        | AuthError::InvalidCallback(_)
        | AuthError::SecretKey(_) => CobletErrorKind::InvalidArgument,
        AuthError::Transport { .. }
        | AuthError::Http { .. }
        | AuthError::InvalidResponse { .. }
        | AuthError::RateLimited { .. } => CobletErrorKind::Network,
        AuthError::Cancelled => CobletErrorKind::Cancelled,
        AuthError::CallbackClosed => CobletErrorKind::InvalidState,
        AuthError::NoLicense => CobletErrorKind::NoLicense,
        AuthError::NoProfile => CobletErrorKind::NoProfile,
        AuthError::Declined
        | AuthError::Expired
        | AuthError::StateMismatch
        | AuthError::OAuth { .. }
        | AuthError::InvalidGrant
        | AuthError::NoXboxAccount
        | AuthError::ChildAccount
        | AuthError::AdultVerificationRequired
        | AuthError::RegionUnavailable
        | AuthError::XboxBanned
        | AuthError::Xbox(_)
        | AuthError::ClientNotApproved => CobletErrorKind::Auth,
        _ => CobletErrorKind::Other,
    }
}

fn player_kind(error: &PlayerError) -> CobletErrorKind {
    match error {
        PlayerError::Unauthorized => CobletErrorKind::Auth,
        PlayerError::Offline => CobletErrorKind::Unsupported,
        PlayerError::InvalidSkin(_) | PlayerError::CapeNotOwned(_) => {
            CobletErrorKind::InvalidArgument
        }
        PlayerError::NoSkin => CobletErrorKind::InvalidState,
        PlayerError::UnknownPlayer(_) | PlayerError::UnknownSkin(_) => CobletErrorKind::NotFound,
        PlayerError::Transport { .. }
        | PlayerError::Http { .. }
        | PlayerError::InvalidResponse { .. } => CobletErrorKind::Network,
        _ => CobletErrorKind::Other,
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
