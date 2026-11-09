use std::ffi::c_char;
use std::time::{SystemTime, UNIX_EPOCH};

use coblet::auth::{Account, AccountId, PlayerProfile, SkinModel};
use coblet::player::SkinSource;
use coblet::runtime::HttpUrl;
use coblet::types::PlayerName;

use crate::context::Coblet;
use crate::guard::{guard, put};
use crate::types::{
    CobletBytes, CobletError, CobletStatus, CobletString, Failure, optional_text, text,
};

/// Arm width of a skin.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletSkinModel {
    /// Arms 4 pixels wide, called Steve or wide.
    Classic = 0,
    /// Arms 3 pixels wide, called Alex or slim.
    Slim = 1,
}

impl From<CobletSkinModel> for SkinModel {
    fn from(model: CobletSkinModel) -> Self {
        match model {
            CobletSkinModel::Classic => Self::Classic,
            CobletSkinModel::Slim => Self::Slim,
        }
    }
}

/// Describes a player profile as `{"name","uuid","skins":[{"id","url",
/// "model","active"}],"capes":[{"id","alias","url","active"}]}`.
fn player_value(profile: &PlayerProfile) -> serde_json::Value {
    let skins: Vec<serde_json::Value> = profile
        .skins
        .iter()
        .map(|skin| {
            serde_json::json!({
                "id": skin.id,
                "url": skin.url.as_str(),
                "model": skin.model.as_str(),
                "active": skin.active,
            })
        })
        .collect();
    let capes: Vec<serde_json::Value> = profile
        .capes
        .iter()
        .map(|cape| {
            serde_json::json!({
                "id": cape.id,
                "alias": cape.alias,
                "url": cape.url.as_str(),
                "active": cape.active,
            })
        })
        .collect();
    serde_json::json!({
        "name": profile.name.as_str(),
        "uuid": profile.uuid,
        "skins": skins,
        "capes": capes,
    })
}

fn unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// Describes an account as `{"id","method","state","player",
/// "session_expires_at","license","license_checked_at","owns_game","data"}`.
pub(crate) fn account_value(account: &Account) -> serde_json::Value {
    serde_json::json!({
        "id": account.id.as_str(),
        "method": account.method.as_str(),
        "state": account.state.as_str(),
        "player": account.profile.as_ref().map(player_value),
        "session_expires_at": account.session_expires_at.and_then(unix_seconds),
        "license": account.license.map(|check| check.license.as_str()),
        "license_checked_at": account.license.and_then(|check| unix_seconds(check.checked_at)),
        "owns_game": account.owns_game(),
        "data": account.data,
    })
}

fn with_schema(mut value: serde_json::Value) -> String {
    value["schema"] = 1.into();
    value.to_string()
}

/// Reads a handle and an account ID.
///
/// # Safety
///
/// `coblet` must be NULL or a live handle; `id` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "reads handle and string arguments")]
unsafe fn args<'a>(
    coblet: *const Coblet,
    id: *const c_char,
) -> Result<(&'a Coblet, AccountId), Failure> {
    // SAFETY: forwarded from the caller's guarantees.
    let handle =
        unsafe { coblet.as_ref() }.ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
    // SAFETY: forwarded from the caller's guarantees.
    let id = unsafe { account_id(id, "account_id") }?;
    Ok((handle, id))
}

/// Reads an account ID.
///
/// # Safety
///
/// `id` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "reads a string argument")]
pub(crate) unsafe fn account_id(id: *const c_char, name: &str) -> Result<AccountId, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    let id = unsafe { text(id, name) }?;
    AccountId::new(id).map_err(|error| Failure::argument(name, &error.to_string()))
}

/// Reads an optional account ID; NULL gives `None`.
///
/// # Safety
///
/// `id` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "reads a string argument")]
unsafe fn optional_account_id(id: *const c_char) -> Result<Option<AccountId>, Failure> {
    if id.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { account_id(id, "account_id") }.map(Some)
}

/// Lists the saved accounts as JSON: `{"schema":1,"default","accounts":[
/// {"id","method","state","player","session_expires_at","license",
/// "license_checked_at","owns_game","data"}]}`.
///
/// `default` is the default account's ID or null. `state` is `ready`,
/// `needs-refresh`, `needs-reauth`, `no-license` or `no-profile`. `player`
/// is null or `{"name","uuid","skins":[{"id","url","model","active"}],
/// "capes":[{"id","alias","url","active"}]}` with `model` `classic` or
/// `slim`. `session_expires_at` is in Unix seconds or null. `license` is
/// the last license check, `purchased`, `entitled` (for example through a
/// subscription) or `missing`, and null for offline or never checked
/// accounts; `license_checked_at` is its time in Unix seconds or null.
/// `owns_game` is true when `license` is `purchased`, like Prism
/// Launcher's "owns Minecraft". `data` holds the string values the
/// account's auth method keeps, such as the address of its login server.
/// On success `*out_json` receives a string
/// to free with `coblet_string_free`.
///
/// Reading the list needs no network. Each `player` is the snapshot from
/// the account's last login, token renewal or skin change;
/// `coblet_account_refresh_profile` fetches the current one.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_json` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_accounts_json(
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
            let store = handle.inner.accounts();
            let (accounts, default) = handle.runtime.block_on(async {
                Ok::<_, Failure>((store.list().await?, store.default_account().await?))
            })?;
            let list: Vec<serde_json::Value> = accounts.iter().map(account_value).collect();
            let json = serde_json::json!({
                "schema": 1,
                "default": default.map(|account| account.id.as_str().to_owned()),
                "accounts": list,
            });
            put(out_json, CobletString::new(&json.to_string()), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds an offline account, or returns the one with that name, as JSON in
/// the account format of `coblet_accounts_json` with a `schema` field.
///
/// # Safety
///
/// `coblet` must be a live handle; `name` must be NULL or NUL-terminated;
/// `out_json` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_add_offline(
    coblet: *const Coblet,
    name: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            let name = PlayerName::new(text(name, "name")?)
                .map_err(|error| Failure::argument("name", &error.to_string()))?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let account = handle
                .runtime
                .block_on(handle.inner.accounts().add_offline(name))?;
            put(
                out_json,
                CobletString::new(&with_schema(account_value(&account))),
                "out_json",
            )?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Removes an account and its stored tokens.
///
/// # Safety
///
/// `coblet` must be a live handle; `account_id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_remove(
    coblet: *const Coblet,
    account_id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let (handle, id) = args(coblet, account_id)?;
            handle
                .runtime
                .block_on(handle.inner.accounts().remove(&id))?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Makes an account the default; NULL clears the default.
///
/// # Safety
///
/// `coblet` must be a live handle; `account_id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_set_default(
    coblet: *const Coblet,
    account_id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            let id = optional_account_id(account_id)?;
            handle
                .runtime
                .block_on(handle.inner.accounts().set_default(id.as_ref()))?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Chooses the account an instance launches with; NULL `account_id` falls
/// back to the default account.
///
/// # Safety
///
/// `coblet` must be a live handle; strings must be NULL or NUL-terminated;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_assign(
    coblet: *const Coblet,
    instance: *const c_char,
    account_id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            let instance = text(instance, "instance")?;
            let id = optional_account_id(account_id)?;
            handle
                .runtime
                .block_on(handle.inner.accounts().assign(instance, id.as_ref()))?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Renews an account's tokens now and returns it as JSON in the account
/// format of `coblet_accounts_json` with a `schema` field.
///
/// # Safety
///
/// `coblet` must be a live handle; `account_id` must be NULL or
/// NUL-terminated; `out_json` must be writable; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_refresh(
    coblet: *const Coblet,
    account_id: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let (handle, id) = args(coblet, account_id)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let account = handle
                .runtime
                .block_on(handle.inner.accounts().refresh(&id))?;
            put(
                out_json,
                CobletString::new(&with_schema(account_value(&account))),
                "out_json",
            )?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Checks with Minecraft services whether the account still owns the
/// game, saves the result and returns the account as JSON in the format of
/// `coblet_accounts_json` with a `schema` field; read its `owns_game` and
/// `license`.
///
/// The current session is reused, so this costs two requests unless the
/// session needs renewing. An account that bought the game since its last
/// check becomes `ready`, one that lost it `no-license`. Offline accounts
/// are returned unchanged with a null `license`. Fails with
/// `COBLET_ERROR_KIND_AUTH` when the user has to sign in again; the last
/// license stays saved.
///
/// # Safety
///
/// `coblet` must be a live handle; `account_id` must be NULL or
/// NUL-terminated; `out_json` must be writable; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_verify_license(
    coblet: *const Coblet,
    account_id: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let (handle, id) = args(coblet, account_id)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let account = handle
                .runtime
                .block_on(handle.inner.accounts().verify_license(&id))?;
            let json = with_schema(account_value(&account));
            put(out_json, CobletString::new(&json), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Writes to `*out_owns` whether any saved account bought the game as of
/// its last license check, without network access. Prism Launcher allows
/// offline accounts only when this holds; the policy is the launcher's.
///
/// # Safety
///
/// `coblet` must be a live handle; `out_owns` must be writable; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_any_account_owns_game(
    coblet: *const Coblet,
    out_owns: *mut bool,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            if out_owns.is_null() {
                return Err(Failure::argument("out_owns", "is NULL"));
            }
            let owns = handle
                .runtime
                .block_on(handle.inner.accounts().any_owns_game())?;
            put(out_owns, owns, "out_owns")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Change to an account's skin or cape.
enum Change {
    Upload(SkinSource, SkinModel),
    Model(SkinModel),
    Reset,
    Cape(Option<String>),
}

/// Runs a skin or cape change and writes the player as JSON.
///
/// # Safety
///
/// As for the exported skin and cape functions.
#[allow(unsafe_code, reason = "reads handle and string arguments")]
unsafe fn change_appearance(
    coblet: *const Coblet,
    account_id: *const c_char,
    change: Change,
    out_json: *mut CobletString,
) -> Result<CobletStatus, Failure> {
    // SAFETY: forwarded from the caller's guarantees.
    let (handle, id) = unsafe { args(coblet, account_id) }?;
    if out_json.is_null() {
        return Err(Failure::argument("out_json", "is NULL"));
    }
    let inner = &handle.inner;
    let profile = handle.runtime.block_on(async {
        match change {
            Change::Upload(source, model) => inner.upload_skin(&id, source, model).await,
            Change::Model(model) => inner.set_skin_model(&id, model).await,
            Change::Reset => inner.reset_skin(&id).await,
            Change::Cape(cape) => inner.set_cape(&id, cape.as_deref()).await,
        }
        .map_err(Failure::from)
    })?;
    // SAFETY: checked for NULL above; the caller guarantees it is writable.
    unsafe {
        put(
            out_json,
            CobletString::new(&with_schema(player_value(&profile))),
            "out_json",
        )
    }?;
    Ok(CobletStatus::Ok)
}

/// Fetches the account's player profile (name, UUID, skins and capes)
/// from Minecraft services now, saves it and returns the updated account
/// as JSON, in the account format of `coblet_accounts_json` with a
/// `schema` field.
///
/// It sends one request with the current token, so a launcher can show the
/// snapshot from `coblet_accounts_json` at once and call this in the
/// background; `coblet_account_refresh` renews the tokens themselves.
/// Offline accounts are returned unchanged. Fails with
/// `COBLET_ERROR_KIND_AUTH` when the user has to sign in again and
/// `COBLET_ERROR_KIND_NO_PROFILE` for an account without a profile.
///
/// # Safety
///
/// `coblet` must be a live handle; `account_id` must be NULL or
/// NUL-terminated; `out_json` must be writable; `out_err` must be NULL or
/// writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_refresh_profile(
    coblet: *const Coblet,
    account_id: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let (handle, id) = args(coblet, account_id)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let account = handle.runtime.block_on(handle.inner.refresh_profile(&id))?;
            let json = with_schema(account_value(&account));
            put(out_json, CobletString::new(&json), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Downloads a skin or cape texture, such as a skin `url` from
/// `coblet_accounts_json`, as PNG bytes, for example to draw the player's
/// head. On success `*out_png` receives bytes to free with
/// `coblet_bytes_free`.
///
/// # Safety
///
/// `coblet` must be a live handle; `url` must be NULL or NUL-terminated;
/// `out_png` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_download_texture(
    coblet: *const Coblet,
    url: *const c_char,
    out_png: *mut CobletBytes,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let handle = coblet
                .as_ref()
                .ok_or_else(|| Failure::argument("coblet", "is NULL"))?;
            let url = HttpUrl::new(text(url, "url")?)
                .map_err(|error| Failure::argument("url", &error.to_string()))?;
            if out_png.is_null() {
                return Err(Failure::argument("out_png", "is NULL"));
            }
            let png = handle
                .runtime
                .block_on(handle.inner.download_texture(&url))?;
            put(out_png, CobletBytes::new(png), "out_png")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Frees bytes returned by the library; an empty value is ignored.
///
/// # Safety
///
/// `value` must come from this library and must not be freed twice.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_bytes_free(value: CobletBytes) {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { value.free() }
}

/// Uploads a PNG skin of 64x64 or 64x32 pixels, wears it and returns the
/// player as the `player` object of `coblet_accounts_json`
/// with a `schema` field.
///
/// # Safety
///
/// As `coblet_account_refresh_profile`; `png` must point to `len` readable bytes.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_upload_skin(
    coblet: *const Coblet,
    account_id: *const c_char,
    png: *const u8,
    len: usize,
    model: CobletSkinModel,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            if png.is_null() {
                return Err(Failure::argument("png", "is NULL"));
            }
            let bytes = std::slice::from_raw_parts(png, len).to_vec();
            change_appearance(
                coblet,
                account_id,
                Change::Upload(SkinSource::Png(bytes), model.into()),
                out_json,
            )
        })
    }
}

/// Wears the skin at a public URL, which the account's appearance service
/// downloads, and returns the player as the `player` object of
/// `coblet_accounts_json` with a `schema` field.
///
/// # Safety
///
/// As `coblet_account_refresh_profile`; `url` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_upload_skin_url(
    coblet: *const Coblet,
    account_id: *const c_char,
    url: *const c_char,
    model: CobletSkinModel,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let url = HttpUrl::new(text(url, "url")?)
                .map_err(|error| Failure::argument("url", &error.to_string()))?;
            change_appearance(
                coblet,
                account_id,
                Change::Upload(SkinSource::Url(url), model.into()),
                out_json,
            )
        })
    }
}

/// Switches the arm width of the worn skin and returns the player as
/// the `player` object of `coblet_accounts_json` with a `schema` field.
///
/// # Safety
///
/// As `coblet_account_refresh_profile`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_set_skin_model(
    coblet: *const Coblet,
    account_id: *const c_char,
    model: CobletSkinModel,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            change_appearance(coblet, account_id, Change::Model(model.into()), out_json)
        })
    }
}

/// Goes back to the default skin and returns the player as
/// the `player` object of `coblet_accounts_json` with a `schema` field.
///
/// # Safety
///
/// As `coblet_account_refresh_profile`.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_reset_skin(
    coblet: *const Coblet,
    account_id: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            change_appearance(coblet, account_id, Change::Reset, out_json)
        })
    }
}

/// Shows an owned cape by ID, or hides the cape when `cape_id` is NULL,
/// and returns the player as the `player` object of `coblet_accounts_json`
/// with a `schema` field.
///
/// # Safety
///
/// As `coblet_account_refresh_profile`; `cape_id` must be NULL or
/// NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_account_set_cape(
    coblet: *const Coblet,
    account_id: *const c_char,
    cape_id: *const c_char,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let cape = optional_text(cape_id, "cape_id")?.map(str::to_owned);
            change_appearance(coblet, account_id, Change::Cape(cape), out_json)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::collections::HashMap;
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::sync::Mutex;

    use coblet::runtime::{Secret, SecretKey, SecretStore as _};

    use super::{
        coblet_account_add_offline, coblet_account_assign, coblet_account_refresh_profile,
        coblet_account_remove, coblet_account_set_default, coblet_account_verify_license,
        coblet_accounts_json, coblet_any_account_owns_game, coblet_bytes_free,
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
        let status =
            unsafe { coblet_builder_build(builder, &raw mut coblet, std::ptr::null_mut()) };
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
        let status = unsafe {
            coblet_account_add_offline(coblet, steve.as_ptr(), &raw mut out, &raw mut err)
        };
        assert_eq!(status, CobletStatus::Ok);
        let account = json(&mut out);
        assert_eq!(account["schema"], 1);
        assert_eq!(account["method"], "coblet:offline");
        assert_eq!(account["state"], "ready");
        assert_eq!(account["player"]["name"], "Steve");
        assert!(account["session_expires_at"].is_null());
        assert!(account["license"].is_null() && account["license_checked_at"].is_null());
        assert_eq!(account["owns_game"], false);
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
        let status = unsafe {
            coblet_account_refresh_profile(coblet, id.as_ptr(), &raw mut out, &raw mut err)
        };
        assert_eq!(status, CobletStatus::Ok);
        let refreshed = json(&mut out);
        assert_eq!(refreshed["id"], account["id"]);
        assert_eq!(refreshed["player"]["name"], "Steve");

        let mut owns = true;
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                coblet_account_verify_license(coblet, id.as_ptr(), &raw mut out, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_any_account_owns_game(coblet, &raw mut owns, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_any_account_owns_game(coblet, std::ptr::null_mut(), &raw mut err),
                CobletStatus::InvalidArgument
            );
        }
        take_error(err);
        assert_eq!(json(&mut out)["license"], serde_json::Value::Null);
        assert!(!owns);

        let mut png = CobletBytes::empty();
        let bad_url = CString::new("not a url").unwrap();
        // SAFETY: as above.
        let status = unsafe {
            coblet_download_texture(coblet, bad_url.as_ptr(), &raw mut png, &raw mut err)
        };
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
        let status =
            unsafe { coblet_builder_set_msa_client_id(unset, empty.as_ptr(), &raw mut err) };
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
}
