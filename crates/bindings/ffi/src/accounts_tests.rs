#![allow(unsafe_code, reason = "drives the C ABI")]

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;

use coblet::runtime::{Secret, SecretKey, SecretStore as _};

use super::{
    coblet_account_add_offline, coblet_account_assign, coblet_account_refresh_profile,
    coblet_account_remove, coblet_account_set_default, coblet_accounts_json, coblet_bytes_free,
    coblet_download_texture,
};
use crate::builder::{
    coblet_builder_build, coblet_builder_new, coblet_builder_set_encrypted_secret_file,
    coblet_builder_set_msa_client_id, coblet_builder_set_secret_store,
};
use crate::context::{Coblet, coblet_error_free, coblet_free, coblet_string_free};
use crate::login::{CobletLoginInteraction, coblet_login_begin};
use crate::secrets::{
    COBLET_SECRET_KEY_LEN, CobletSecretCall, CobletSecretStore, ForeignSecrets,
    coblet_generate_secret_key, coblet_secret_call_fail, coblet_secret_call_set_value,
};
use crate::types::{CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString};

/// Takes an error out of `err`, returning its kind and message.
fn take_error(err: *mut CobletError) -> (CobletErrorKind, String) {
    assert!(!err.is_null());
    // SAFETY: the error was just returned and is freed once.
    unsafe {
        let kind = (*err).kind;
        let message = CStr::from_ptr((*err).message.ptr)
            .to_str()
            .unwrap()
            .to_owned();
        coblet_error_free(err);
        (kind, message)
    }
}

/// Takes a library string.
fn take_string(value: CobletString) -> String {
    // SAFETY: the string was just returned and is freed once.
    unsafe {
        let text = CStr::from_ptr(value.ptr).to_str().unwrap().to_owned();
        coblet_string_free(value);
        text
    }
}

fn json(value: &mut CobletString) -> serde_json::Value {
    let value = std::mem::replace(value, CobletString::empty());
    serde_json::from_str(&take_string(value)).unwrap()
}

fn builder(dir: &tempfile::TempDir) -> *mut crate::builder::CobletBuilder {
    let root = CString::new(dir.path().to_str().unwrap()).unwrap();
    let agent = CString::new("coblet-ffi-test/1.0").unwrap();
    let mut builder = std::ptr::null_mut();
    // SAFETY: valid strings and out-pointers.
    let status = unsafe {
        coblet_builder_new(
            root.as_ptr(),
            agent.as_ptr(),
            &raw mut builder,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, CobletStatus::Ok);
    builder
}

fn build(builder: *mut crate::builder::CobletBuilder) -> *mut Coblet {
    let mut coblet = std::ptr::null_mut();
    // SAFETY: a live builder that is not used afterwards.
    let status = unsafe { coblet_builder_build(builder, &raw mut coblet, std::ptr::null_mut()) };
    assert_eq!(status, CobletStatus::Ok);
    coblet
}

#[test]
fn offline_accounts_round_trip_through_json() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = build(builder(&dir));
    let mut out = CobletString::empty();
    let mut err = std::ptr::null_mut();

    let steve = CString::new("Steve").unwrap();
    // SAFETY: a live handle, valid strings and out-pointers.
    let status =
        unsafe { coblet_account_add_offline(coblet, steve.as_ptr(), &raw mut out, &raw mut err) };
    assert_eq!(status, CobletStatus::Ok);
    let account = json(&mut out);
    assert_eq!(account["schema"], 1);
    assert_eq!(account["method"], "coblet:offline");
    assert_eq!(account["state"], "ready");
    assert_eq!(account["player"]["name"], "Steve");
    assert!(account["session_expires_at"].is_null());
    let id = CString::new(account["id"].as_str().unwrap()).unwrap();

    let pack = CString::new("pack").unwrap();
    // SAFETY: as above; NULL clears the default.
    unsafe {
        assert_eq!(
            coblet_account_set_default(coblet, std::ptr::null(), &raw mut err),
            CobletStatus::Ok
        );
        assert_eq!(
            coblet_account_assign(coblet, pack.as_ptr(), id.as_ptr(), &raw mut err),
            CobletStatus::Ok
        );
        assert_eq!(
            coblet_accounts_json(coblet, &raw mut out, &raw mut err),
            CobletStatus::Ok
        );
    }
    let list = json(&mut out);
    assert!(list["default"].is_null());
    assert_eq!(list["accounts"].as_array().unwrap().len(), 1);

    // SAFETY: as above.
    let status =
        unsafe { coblet_account_refresh_profile(coblet, id.as_ptr(), &raw mut out, &raw mut err) };
    assert_eq!(status, CobletStatus::Ok);
    let refreshed = json(&mut out);
    assert_eq!(refreshed["id"], account["id"]);
    assert_eq!(refreshed["player"]["name"], "Steve");

    let mut png = CobletBytes::empty();
    let bad_url = CString::new("not a url").unwrap();
    // SAFETY: as above.
    let status =
        unsafe { coblet_download_texture(coblet, bad_url.as_ptr(), &raw mut png, &raw mut err) };
    assert_eq!(status, CobletStatus::InvalidArgument);
    take_error(err);
    // SAFETY: an empty value owns nothing.
    unsafe { coblet_bytes_free(png) };

    // SAFETY: as above.
    unsafe {
        assert_eq!(
            coblet_account_remove(coblet, id.as_ptr(), &raw mut err),
            CobletStatus::Ok
        );
        assert_eq!(
            coblet_account_remove(coblet, id.as_ptr(), &raw mut err),
            CobletStatus::Error
        );
    }
    assert_eq!(take_error(err).0, CobletErrorKind::NotFound);
    let bad = CString::new("not/an/id").unwrap();
    // SAFETY: as above.
    let status = unsafe { coblet_account_remove(coblet, bad.as_ptr(), &raw mut err) };
    assert_eq!(status, CobletStatus::InvalidArgument);
    take_error(err);
    // SAFETY: freed once.
    unsafe { coblet_free(coblet) };
}

#[test]
fn microsoft_login_needs_a_client_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut err = std::ptr::null_mut();
    let unset = builder(&dir);
    let empty = CString::new("").unwrap();
    // SAFETY: a live builder and valid strings.
    let status = unsafe { coblet_builder_set_msa_client_id(unset, empty.as_ptr(), &raw mut err) };
    assert_eq!(status, CobletStatus::InvalidArgument);
    take_error(err);
    let coblet = build(unset);
    let mut login = std::ptr::null_mut();
    // SAFETY: a live handle and out-pointers; NULL strings are optional here.
    let status = unsafe {
        coblet_login_begin(
            coblet,
            CobletLoginInteraction::DeviceCode,
            std::ptr::null(),
            std::ptr::null(),
            &raw mut login,
            &raw mut err,
        )
    };
    assert_eq!(status, CobletStatus::Error);
    assert_eq!(take_error(err).0, CobletErrorKind::Unsupported);
    assert!(login.is_null());
    // SAFETY: freed once.
    unsafe { coblet_free(coblet) };
}

/// A keychain in C terms: a map behind `user_data` that refuses `locked`.
struct Keychain {
    values: Mutex<HashMap<String, String>>,
    freed: *mut bool,
}

unsafe extern "C" fn get(
    data: *mut c_void,
    key: *const c_char,
    call: *mut CobletSecretCall,
) -> bool {
    // SAFETY: the test passes a `Keychain` and valid arguments.
    unsafe {
        let keychain = &*data.cast::<Keychain>();
        let key = CStr::from_ptr(key).to_str().unwrap();
        if let Some(value) = keychain.values.lock().unwrap().get(key) {
            let value = CString::new(value.as_str()).unwrap();
            assert_eq!(
                coblet_secret_call_set_value(call, value.as_ptr()),
                CobletStatus::Ok
            );
        }
        true
    }
}

unsafe extern "C" fn put(
    data: *mut c_void,
    key: *const c_char,
    value: *const c_char,
    call: *mut CobletSecretCall,
) -> bool {
    // SAFETY: as in `get`.
    unsafe {
        let keychain = &*data.cast::<Keychain>();
        let value = CStr::from_ptr(value).to_str().unwrap();
        if value == "locked" {
            let message = CString::new("keychain is locked").unwrap();
            coblet_secret_call_fail(call, message.as_ptr());
            return false;
        }
        let key = CStr::from_ptr(key).to_str().unwrap();
        keychain
            .values
            .lock()
            .unwrap()
            .insert(key.to_owned(), value.to_owned());
        true
    }
}

unsafe extern "C" fn delete(
    data: *mut c_void,
    key: *const c_char,
    _call: *mut CobletSecretCall,
) -> bool {
    // SAFETY: as in `get`.
    unsafe {
        let keychain = &*data.cast::<Keychain>();
        let key = CStr::from_ptr(key).to_str().unwrap();
        keychain.values.lock().unwrap().remove(key);
        true
    }
}

unsafe extern "C" fn free(data: *mut c_void) {
    // SAFETY: `data` is the boxed keychain, freed once.
    unsafe {
        let keychain = Box::from_raw(data.cast::<Keychain>());
        *keychain.freed = true;
    }
}

fn keychain_table(freed: &mut bool) -> CobletSecretStore {
    let keychain = Box::new(Keychain {
        values: Mutex::new(HashMap::new()),
        freed,
    });
    CobletSecretStore {
        user_data: Box::into_raw(keychain).cast(),
        get: Some(get),
        put: Some(put),
        delete: Some(delete),
        free: Some(free),
    }
}

#[test]
fn c_secret_stores_back_coblet_secrets() {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(secret_round_trip());
}

async fn secret_round_trip() {
    let mut freed = false;
    let table = keychain_table(&mut freed);
    let store = ForeignSecrets::new(&table).unwrap();
    let key = SecretKey::new("coblet:microsoft/a/refresh_token").unwrap();
    assert!(store.get(&key).await.unwrap().is_none());
    store.put(&key, Secret::new("token")).await.unwrap();
    assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "token");
    let error = store.put(&key, Secret::new("locked")).await.unwrap_err();
    assert!(error.to_string().contains("keychain is locked"), "{error}");
    store.delete(&key).await.unwrap();
    assert!(store.get(&key).await.unwrap().is_none());
    drop(store);
    assert!(freed);
}

#[test]
fn builders_take_a_secret_store_or_an_encrypted_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut err = std::ptr::null_mut();
    let mut freed = false;
    let table = keychain_table(&mut freed);
    let with_store = builder(&dir);
    // SAFETY: a live builder and a valid table.
    let status =
        unsafe { coblet_builder_set_secret_store(with_store, &raw const table, &raw mut err) };
    assert_eq!(status, CobletStatus::Ok);
    let coblet = build(with_store);
    // SAFETY: freed once.
    unsafe { coblet_free(coblet) };
    assert!(freed);

    let mut incomplete = keychain_table(&mut freed);
    freed = false;
    incomplete.delete = None;
    let other = builder(&dir);
    // SAFETY: as above; the library frees the table's data on failure.
    let status =
        unsafe { coblet_builder_set_secret_store(other, &raw const incomplete, &raw mut err) };
    assert_eq!(status, CobletStatus::InvalidArgument);
    take_error(err);
    assert!(freed);

    let mut key = [0_u8; COBLET_SECRET_KEY_LEN];
    // SAFETY: a writable buffer of the right size.
    unsafe {
        assert_eq!(
            coblet_generate_secret_key(key.as_mut_ptr(), key.len(), &raw mut err),
            CobletStatus::Ok
        );
        assert_eq!(
            coblet_generate_secret_key(key.as_mut_ptr(), 16, &raw mut err),
            CobletStatus::InvalidArgument
        );
    }
    take_error(err);
    assert_ne!(key, [0; COBLET_SECRET_KEY_LEN]);
    let path = CString::new(dir.path().join("secrets.bin").to_str().unwrap()).unwrap();
    // SAFETY: a live builder, a valid path and key.
    let status = unsafe {
        coblet_builder_set_encrypted_secret_file(
            other,
            path.as_ptr(),
            key.as_ptr(),
            key.len(),
            &raw mut err,
        )
    };
    assert_eq!(status, CobletStatus::Ok);
    // SAFETY: freed once.
    unsafe { coblet_free(build(other)) };
}
