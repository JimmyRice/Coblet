use std::ffi::c_char;
use std::sync::Arc;

use coblet::auth::{EncryptedFileStore, EncryptionKey, MsaClientId};
use coblet::platform::{DataProfile, PlatformProfile, ProfileEngine, ProfileLayer};
use coblet::runtime::{Extension, UserAgent};
use coblet::types::{ExtensionId, Platform, PlatformNameError, PlatformTrait};

use crate::context::Coblet;
use crate::customize::{CobletCustomizer, ForeignCustomizer};
use crate::guard::{guard, put};
use crate::profile::{CobletPlatformProfile, ForeignProfile};
use crate::secrets::{CobletSecretStore, ForeignSecrets, encryption_key};
use crate::sources::{CobletUrlRewriter, ForeignRewriter, parse_kinds};
use crate::types::{CobletError, CobletErrorKind, CobletStatus, Failure, text};

/// Settings of a `Coblet` handle before it is built.
///
/// Created with `coblet_builder_new`; `coblet_builder_build` turns it into
/// a `Coblet` and frees it, and `coblet_builder_free` drops it unused.
pub struct CobletBuilder {
    data_root: String,
    user_agent: UserAgent,
    customizers: Vec<ForeignCustomizer>,
    auth_grants: Vec<ExtensionId>,
    profiles: Vec<Arc<dyn PlatformProfile>>,
    platform: Option<Platform>,
    msa_client_id: Option<MsaClientId>,
    secrets: Secrets,
    rewriters: Vec<Arc<dyn coblet::runtime::UrlRewriter>>,
    version_sources: Vec<coblet::mojang::MojangVersions>,
}

/// Where tokens are kept.
enum Secrets {
    Memory,
    Foreign(ForeignSecrets),
    File(String, EncryptionKey),
}

impl std::fmt::Debug for CobletBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletBuilder")
            .field("data_root", &self.data_root)
            .finish_non_exhaustive()
    }
}

impl CobletBuilder {
    fn add_profile(&mut self, profile: Arc<dyn PlatformProfile>) -> Result<(), Failure> {
        let taken = self
            .profiles
            .iter()
            .chain(ProfileEngine::builtin().profiles())
            .any(|known| known.id() == profile.id());
        if taken {
            return Err(Failure::argument(
                "platform profile",
                &format!("`{}` is already registered", profile.id()),
            ));
        }
        self.profiles.push(profile);
        Ok(())
    }
}

/// Reads a builder argument.
///
/// # Safety
///
/// `builder` must be NULL or a live builder handle.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn builder_mut<'a>(builder: *mut CobletBuilder) -> Result<&'a mut CobletBuilder, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { builder.as_mut() }.ok_or_else(|| Failure::argument("builder", "is NULL"))
}

/// Starts building a `Coblet` handle with the default transport.
///
/// `data_root` is the directory for shared game files; `user_agent`
/// identifies the launcher to web services, such as
/// `MyLauncher/1.0 (contact@example.com)`. On success `*out_builder`
/// receives a handle for `coblet_builder_build` or `coblet_builder_free`.
///
/// # Safety
///
/// Strings must be NULL or NUL-terminated; `out_builder` must be writable;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_new(
    data_root: *const c_char,
    user_agent: *const c_char,
    out_builder: *mut *mut CobletBuilder,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let data_root = text(data_root, "data_root")?.to_owned();
            let user_agent = UserAgent::new(text(user_agent, "user_agent")?)
                .map_err(|error| Failure::argument("user_agent", &error.to_string()))?;
            if out_builder.is_null() {
                return Err(Failure::argument("out_builder", "is NULL"));
            }
            let builder = Box::new(CobletBuilder {
                data_root,
                user_agent,
                customizers: Vec::new(),
                auth_grants: Vec::new(),
                profiles: Vec::new(),
                platform: None,
                msa_client_id: None,
                secrets: Secrets::Memory,
                rewriters: Vec::new(),
                version_sources: Vec::new(),
            });
            put(out_builder, Box::into_raw(builder), "out_builder")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a Platform Profile written as TOML, such as one for an operating
/// system or architecture the built-in profiles do not cover.
///
/// The text is checked now: a malformed profile, or one whose `id` is
/// already registered, fails with `COBLET_STATUS_INVALID_ARGUMENT`. The
/// format is the one of coblet's built-in profiles; it can also be
/// generated at run time, for example after probing system library paths.
///
/// # Safety
///
/// `builder` must be a live builder handle; `toml` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_platform_profile_toml(
    builder: *mut CobletBuilder,
    toml: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            let profile = DataProfile::from_toml(text(toml, "toml")?, ProfileLayer::ThirdParty)
                .map_err(|error| Failure::argument("toml", &error.to_string()))?;
            builder.add_profile(Arc::new(profile))?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a Platform Profile written in C, for adaptations that need code,
/// such as probing the machine.
///
/// The library owns the table's user data from this call on, even when it
/// fails, and calls its `free` once: on failure before returning, otherwise
/// when the builder or the built `Coblet` is freed. A NULL `matches`, a
/// malformed ID or one that is already registered fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings in the table must be
/// NULL or NUL-terminated and its callbacks must stay valid until its
/// `free` runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_platform_profile(
    builder: *mut CobletBuilder,
    profile: CobletPlatformProfile,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees; the adapter owns the
    // user data from here on.
    let adapter = unsafe { ForeignProfile::new(&profile) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            builder.add_profile(Arc::new(adapter?))?;
            Ok(CobletStatus::Ok)
        })
    }
}

fn platform_from_json(json: &str) -> Result<Platform, Failure> {
    let bad = |problem: &str| Failure::argument("platform_json", problem);
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| bad(&format!("is not JSON: {error}")))?;
    let field = |name: &str| -> Result<Option<&str>, Failure> {
        match value.get(name) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(bad(&format!("has a non-string `{name}`"))),
        }
    };
    let name = |error: PlatformNameError| bad(&error.to_string());
    let os = field("os")?.ok_or_else(|| bad("has no `os`"))?;
    let arch = field("arch")?.ok_or_else(|| bad("has no `arch`"))?;
    let mut platform = Platform::new(os.parse().map_err(name)?, arch.parse().map_err(name)?);
    if let Some(version) = field("os_version")? {
        platform = platform.with_os_version(version);
    }
    if let Some(libc) = field("libc")? {
        platform = platform.with_libc(libc.parse().map_err(name)?);
    }
    if let Some(traits) = value.get("traits") {
        let traits = traits
            .as_array()
            .ok_or_else(|| bad("has a `traits` that is not an array"))?;
        for item in traits {
            let item = item
                .as_str()
                .ok_or_else(|| bad("has a trait that is not a string"))?;
            platform = platform.with_trait(PlatformTrait::new(item).map_err(name)?);
        }
    }
    Ok(platform)
}

/// Describes the platform instead of detecting it from the build target,
/// for example to give the OS version or platform traits that profiles
/// match on.
///
/// `platform_json` is `{"os","arch","os_version","libc","traits":[]}`;
/// `os` and `arch` are required, the rest may be absent or `null`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `platform_json` must be NULL
/// or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_set_platform(
    builder: *mut CobletBuilder,
    platform_json: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            builder.platform = Some(platform_from_json(text(platform_json, "platform_json")?)?);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a launch customizer; it runs only for launches that name its ID in
/// `coblet_launch_customized`.
///
/// The library owns the table's user data from this call on, even when it
/// fails, and calls its `free` once: on failure before returning, otherwise
/// when the builder or the built `Coblet` is freed.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings in the table must be
/// NULL or NUL-terminated and its callbacks must stay valid until its
/// `free` runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_customizer(
    builder: *mut CobletBuilder,
    customizer: CobletCustomizer,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees; the adapter owns the
    // user data from here on.
    let adapter = unsafe { ForeignCustomizer::new(&customizer) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            let adapter = adapter?;
            if builder
                .customizers
                .iter()
                .any(|known| known.id() == adapter.id())
            {
                return Err(Failure::argument(
                    "customizer id",
                    &format!("`{}` is already registered", adapter.id()),
                ));
            }
            builder.customizers.push(adapter);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Lets the added customizer `id` add and remove authentication
/// arguments, such as `--accessToken` or `${auth_uuid}`, for example one
/// that points the game at a third-party login server.
///
/// Customizers cannot do this by default: the launch fails with
/// `COBLET_ERROR_KIND_LAUNCH` when one tries. Grant it only to customizers
/// the launcher trusts with the account; their changes still appear in
/// `coblet_game_report`. A malformed `id`, or one no customizer added with
/// `coblet_builder_add_customizer` has, fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_allow_auth_changes(
    builder: *mut CobletBuilder,
    id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            let id = ExtensionId::new(text(id, "customizer id")?)
                .map_err(|error| Failure::argument("customizer id", &error.to_string()))?;
            if !builder.customizers.iter().any(|known| known.id() == &id) {
                return Err(Failure::argument(
                    "customizer id",
                    &format!("`{id}` was not added; add the customizer first"),
                ));
            }
            if !builder.auth_grants.contains(&id) {
                builder.auth_grants.push(id);
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Sends requests whose address starts with `from` to the same path under
/// `to`, for a regional mirror or a proxy.
///
/// `kinds` is NULL or a JSON array of kind names, such as
/// `["library", "metadata"]`, that limits the rule; NULL or an empty array
/// applies it to every kind. Kind names are `library`, `natives`, `asset`,
/// `client_jar`, `server_jar`, `runtime`, `content`, `pack`, `loader` and
/// `metadata`. Rules apply to game files, libraries, assets, Java runtimes
/// and metadata such as the version manifest, in the order they were
/// added, together with rewriters from `coblet_builder_add_url_rewriter`.
/// Hashes are still checked, so a mirror cannot change content that is
/// listed with a hash. Account and skin requests are not rewritten. A
/// prefix that is not an HTTP or HTTPS address, or malformed `kinds`,
/// fails with `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated (`kinds` may be NULL); `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_mirror(
    builder: *mut CobletBuilder,
    from: *const c_char,
    to: *const c_char,
    kinds: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            let kinds = if kinds.is_null() {
                None
            } else {
                Some(text(kinds, "kinds")?)
            };
            let mut rule =
                coblet::runtime::PrefixRewriter::new(text(from, "from")?, text(to, "to")?)
                    .map_err(|error| Failure::argument("mirror", &error.to_string()))?;
            if let Some(kinds) = parse_kinds(kinds)? {
                rule = rule.for_kinds(kinds);
            }
            builder.rewriters.push(Arc::new(rule));
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a rewriter written in C, run in order with the mirror rules.
///
/// The library owns the table's `user_data` from this call on, even when
/// it fails. A table without a `rewrite` callback fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; the table's callbacks must stay
/// valid until its `free` runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_url_rewriter(
    builder: *mut CobletBuilder,
    rewriter: CobletUrlRewriter,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // The adapter owns the user data from here on.
    let adapter = ForeignRewriter::new(&rewriter);
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder_mut(builder)?;
            builder.rewriters.push(Arc::new(adapter?));
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a game version source that reads a manifest in the format of
/// Mojang's `version_manifest_v2.json` from `url`, such as a list of
/// experimental versions. It comes after the official manifest; when both
/// list an ID, the official entry wins. A malformed or already used `id`,
/// or an address that is not HTTP or HTTPS, fails with
/// `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `builder` must be a live builder handle; strings must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_add_version_manifest(
    builder: *mut CobletBuilder,
    id: *const c_char,
    url: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            let id = ExtensionId::new(text(id, "source id")?)
                .map_err(|error| Failure::argument("source id", &error.to_string()))?;
            let url = coblet::runtime::HttpUrl::new(text(url, "url")?)
                .map_err(|error| Failure::argument("url", &error.to_string()))?;
            let taken = id.as_str() == "coblet:mojang"
                || builder
                    .version_sources
                    .iter()
                    .any(|source| source.id() == &id);
            if taken {
                return Err(Failure::argument(
                    "source id",
                    &format!("`{id}` is already added"),
                ));
            }
            builder
                .version_sources
                .push(coblet::mojang::MojangVersions::other(id, url));
            Ok(CobletStatus::Ok)
        })
    }
}

/// Enables Microsoft login with the launcher's Azure application Client
/// ID, which Minecraft services must have approved. Without one,
/// `coblet_login_begin` fails with `COBLET_ERROR_KIND_UNSUPPORTED`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `client_id` must be NULL or
/// NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_set_msa_client_id(
    builder: *mut CobletBuilder,
    client_id: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            let client_id = MsaClientId::new(text(client_id, "client_id")?)
                .map_err(|error| Failure::argument("client_id", &error.to_string()))?;
            builder.msa_client_id = Some(client_id);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Keeps refresh tokens and other secrets in a store written in C, such
/// as the system keychain. Replaces an earlier
/// `coblet_builder_set_encrypted_secret_file`. Without either, secrets
/// live in memory and every Microsoft account needs a new login after a
/// restart.
///
/// The library owns the table's user data from this call on, even when it
/// fails.
///
/// # Safety
///
/// `builder` must be a live builder handle; `store` must point to a
/// callback table whose callbacks and user data may be used from any
/// thread until `free`; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_set_secret_store(
    builder: *mut CobletBuilder,
    store: *const CobletSecretStore,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let table = store
                .as_ref()
                .ok_or_else(|| Failure::argument("store", "is NULL"))?;
            let store = ForeignSecrets::new(table)?;
            builder_mut(builder)?.secrets = Secrets::Foreign(store);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Keeps secrets in one file encrypted with a key of
/// `COBLET_SECRET_KEY_LEN` bytes, such as one from
/// `coblet_generate_secret_key`. Keep the key somewhere safer than next to
/// the file, such as a system keychain. Replaces an earlier
/// `coblet_builder_set_secret_store`.
///
/// # Safety
///
/// `builder` must be a live builder handle; `path` must be NULL or
/// NUL-terminated; `key` must point to `key_len` readable bytes; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_set_encrypted_secret_file(
    builder: *mut CobletBuilder,
    path: *const c_char,
    key: *const u8,
    key_len: usize,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let builder = builder_mut(builder)?;
            let path = text(path, "path")?.to_owned();
            let key = encryption_key(key, key_len)?;
            builder.secrets = Secrets::File(path, key);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Builds the `Coblet` handle and frees the builder, whether or not it succeeds.
///
/// On success `*out_coblet` receives a handle to free with `coblet_free`.
///
/// # Safety
///
/// `builder` must be a live builder handle that is not used afterwards;
/// `out_coblet` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_build(
    builder: *mut CobletBuilder,
    out_coblet: *mut *mut Coblet,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller hands over a live handle from `coblet_builder_new`.
    let builder = (!builder.is_null()).then(|| unsafe { Box::from_raw(builder) });
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, move || {
            let builder = builder.ok_or_else(|| Failure::argument("builder", "is NULL"))?;
            if out_coblet.is_null() {
                return Err(Failure::argument("out_coblet", "is NULL"));
            }
            let CobletBuilder {
                data_root,
                user_agent,
                customizers,
                auth_grants,
                profiles,
                platform,
                msa_client_id,
                secrets,
                rewriters,
                version_sources,
            } = *builder;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("coblet")
                .build()
                .map_err(|error| Failure::new(CobletErrorKind::Other, error.to_string()))?;
            let mut options = coblet::Coblet::builder()
                .data_root(data_root)
                .user_agent(user_agent);
            if let Some(platform) = platform {
                options = options.platform(platform);
            }
            for profile in profiles {
                options = options.platform_profile(profile);
            }
            for customizer in customizers {
                options = options.customizer(Arc::new(customizer));
            }
            for id in auth_grants {
                options = options.allow_auth_changes(id);
            }
            if let Some(client_id) = msa_client_id {
                options = options.msa_client_id(client_id);
            }
            for rewriter in rewriters {
                options = options.url_rewriter(rewriter);
            }
            for source in version_sources {
                options = options.version_source(Arc::new(source));
            }
            match secrets {
                Secrets::Memory => {}
                Secrets::Foreign(store) => options = options.secret_store(Arc::new(store)),
                Secrets::File(path, key) => {
                    let storage = Arc::new(coblet::runtime::TokioStorage::new());
                    let store = EncryptedFileStore::new(storage, path, key);
                    options = options.secret_store(Arc::new(store));
                }
            }
            let inner = runtime.block_on(async { options.build() })?;
            let handle = Box::into_raw(Box::new(Coblet { runtime, inner }));
            put(out_coblet, handle, "out_coblet").inspect_err(|_| {
                // SAFETY: the handle was created above and never handed out.
                drop(Box::from_raw(handle));
            })?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Frees a builder without building it; NULL is ignored. Customizers added
/// to it get their `free` called.
///
/// # Safety
///
/// `builder` must be NULL or a live builder handle that is not used afterwards.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_builder_free(builder: *mut CobletBuilder) {
    if !builder.is_null() {
        // SAFETY: the handle came from `Box::into_raw` in `coblet_builder_new`.
        drop(unsafe { Box::from_raw(builder) });
    }
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
