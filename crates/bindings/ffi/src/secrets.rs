use std::ffi::{CString, c_char, c_void};

use coblet::auth::EncryptionKey;
use coblet::runtime::{BoxFuture, Secret, SecretKey, SecretStoreError};

use crate::customize::Owned;
use crate::guard::guard;
use crate::types::{CobletError, CobletErrorKind, CobletStatus, Failure, text};

/// Callback table of a secret store written in C, such as one backed by a
/// system keychain.
///
/// Keys look like `coblet:microsoft/<account id>/refresh_token`. Callbacks
/// run on the library's runtime threads, possibly several at once, and
/// should return quickly. Each gets a `CobletSecretCall` handle, valid only
/// during the call: `get` hands its value over with
/// `coblet_secret_call_set_value`, and a callback that returns false can
/// give the reason with `coblet_secret_call_fail`. The library owns
/// `user_data` from the call that receives the table, even when it fails,
/// and calls `free` once when the `Coblet` is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletSecretStore {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Looks up `key`; sets the value when there is one. Returns false on failure.
    pub get: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            key: *const c_char,
            call: *mut CobletSecretCall,
        ) -> bool,
    >,
    /// Stores `value` for `key`. Returns false on failure.
    pub put: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            key: *const c_char,
            value: *const c_char,
            call: *mut CobletSecretCall,
        ) -> bool,
    >,
    /// Removes `key`; a missing key is not a failure. Returns false on failure.
    pub delete: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            key: *const c_char,
            call: *mut CobletSecretCall,
        ) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// Result of one secret store callback.
#[derive(Debug, Default)]
pub struct CobletSecretCall {
    value: Option<String>,
    failure: Option<String>,
}

type Get = unsafe extern "C" fn(*mut c_void, *const c_char, *mut CobletSecretCall) -> bool;
type Put =
    unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut CobletSecretCall) -> bool;

/// C secret store used as coblet's secret store.
pub(crate) struct ForeignSecrets {
    get: Get,
    put: Put,
    delete: Get,
    owned: Owned,
}

impl ForeignSecrets {
    /// Takes over a callback table.
    pub(crate) fn new(table: &CobletSecretStore) -> Result<Self, Failure> {
        let owned = Owned {
            user_data: table.user_data,
            free: table.free,
        };
        let (Some(get), Some(put), Some(delete)) = (table.get, table.put, table.delete) else {
            return Err(Failure::argument(
                "secret store",
                "needs get, put and delete callbacks",
            ));
        };
        Ok(Self {
            get,
            put,
            delete,
            owned,
        })
    }

    #[allow(unsafe_code, reason = "calls a C secret store callback")]
    fn call(
        &self,
        key: &SecretKey,
        run: impl FnOnce(*mut c_void, *const c_char, *mut CobletSecretCall) -> bool,
    ) -> Result<Option<String>, SecretStoreError> {
        let key = CString::new(key.as_str())
            .map_err(|_| SecretStoreError::Unavailable("secret key contains NUL".to_owned()))?;
        let mut call = CobletSecretCall::default();
        if run(self.owned.user_data, key.as_ptr(), &raw mut call) {
            Ok(call.value)
        } else {
            let message = call
                .failure
                .unwrap_or_else(|| "the secret store callback failed".to_owned());
            Err(SecretStoreError::Unavailable(message))
        }
    }
}

impl coblet::runtime::SecretStore for ForeignSecrets {
    #[allow(unsafe_code, reason = "calls a C secret store callback")]
    fn get<'a>(
        &'a self,
        key: &'a SecretKey,
    ) -> BoxFuture<'a, Result<Option<Secret>, SecretStoreError>> {
        Box::pin(async move {
            let value = self.call(key, |data, key, call| {
                // SAFETY: the table owner promised a callback that accepts its
                // user data, a NUL-terminated key and the call handle, all
                // valid for the duration of the call.
                unsafe { (self.get)(data, key, call) }
            })?;
            Ok(value.map(Secret::new))
        })
    }

    #[allow(unsafe_code, reason = "calls a C secret store callback")]
    fn put<'a>(
        &'a self,
        key: &'a SecretKey,
        secret: Secret,
    ) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async move {
            let value = CString::new(secret.expose())
                .map_err(|_| SecretStoreError::Unavailable("secret contains NUL".to_owned()))?;
            self.call(key, |data, key, call| {
                // SAFETY: as in `get`; `value` outlives the call.
                unsafe { (self.put)(data, key, value.as_ptr(), call) }
            })?;
            Ok(())
        })
    }

    #[allow(unsafe_code, reason = "calls a C secret store callback")]
    fn delete<'a>(&'a self, key: &'a SecretKey) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async move {
            self.call(key, |data, key, call| {
                // SAFETY: as in `get`.
                unsafe { (self.delete)(data, key, call) }
            })?;
            Ok(())
        })
    }
}

/// Reads a call handle argument.
///
/// # Safety
///
/// `call` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn call_mut<'a>(call: *mut CobletSecretCall) -> Result<&'a mut CobletSecretCall, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { call.as_mut() }.ok_or_else(|| Failure::argument("call", "is NULL"))
}

/// Hands the value found by a `get` callback to the library, which copies it.
///
/// # Safety
///
/// `call` must be the handle passed to the running `get` callback; `value`
/// must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_secret_call_set_value(
    call: *mut CobletSecretCall,
    value: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            call_mut(call)?.value = Some(text(value, "value")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// Gives the reason the running secret store callback is about to return false.
///
/// # Safety
///
/// `call` must be the handle passed to the running callback; `message` must
/// be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_secret_call_fail(
    call: *mut CobletSecretCall,
    message: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            call_mut(call)?.failure = Some(text(message, "message")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// Size in bytes of a key for `coblet_builder_set_encrypted_secret_file`.
pub const COBLET_SECRET_KEY_LEN: usize = 32;

/// Writes a new random key of `COBLET_SECRET_KEY_LEN` bytes to `out_key`.
///
/// # Safety
///
/// `out_key` must point to `len` writable bytes; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_generate_secret_key(
    out_key: *mut u8,
    len: usize,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            if out_key.is_null() {
                return Err(Failure::argument("out_key", "is NULL"));
            }
            if len != COBLET_SECRET_KEY_LEN {
                return Err(Failure::argument("len", "must be COBLET_SECRET_KEY_LEN"));
            }
            let key = EncryptionKey::generate()
                .map_err(|error| Failure::new(CobletErrorKind::Other, error.to_string()))?;
            std::ptr::copy_nonoverlapping(key.as_bytes().as_ptr(), out_key, len);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Reads a key of `COBLET_SECRET_KEY_LEN` bytes.
///
/// # Safety
///
/// `key` must be NULL or point to `len` readable bytes.
#[allow(unsafe_code, reason = "reads a key argument")]
pub(crate) unsafe fn encryption_key(key: *const u8, len: usize) -> Result<EncryptionKey, Failure> {
    if key.is_null() {
        return Err(Failure::argument("key", "is NULL"));
    }
    if len != COBLET_SECRET_KEY_LEN {
        return Err(Failure::argument(
            "key_len",
            "must be COBLET_SECRET_KEY_LEN",
        ));
    }
    let mut bytes = [0_u8; COBLET_SECRET_KEY_LEN];
    // SAFETY: the caller guarantees `len` readable bytes, checked to be 32.
    unsafe { std::ptr::copy_nonoverlapping(key, bytes.as_mut_ptr(), len) };
    Ok(EncryptionKey::from_bytes(bytes))
}
