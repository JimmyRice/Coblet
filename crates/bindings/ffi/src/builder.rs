use std::ffi::c_char;
use std::sync::Arc;

use coblet::platform::{DataProfile, PlatformProfile, ProfileEngine, ProfileLayer};
use coblet::runtime::{Extension, UserAgent};
use coblet::types::{Platform, PlatformNameError, PlatformTrait};

use crate::context::Coblet;
use crate::customize::{CobletCustomizer, ForeignCustomizer};
use crate::guard::{guard, put};
use crate::types::{CobletError, CobletErrorKind, CobletStatus, Failure, text};

/// Settings of a `Coblet` handle before it is built.
///
/// Created with `coblet_builder_new`; `coblet_builder_build` turns it into
/// a `Coblet` and frees it, and `coblet_builder_free` drops it unused.
pub struct CobletBuilder {
    data_root: String,
    user_agent: UserAgent,
    customizers: Vec<ForeignCustomizer>,
    profiles: Vec<Arc<dyn PlatformProfile>>,
    platform: Option<Platform>,
}

impl std::fmt::Debug for CobletBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletBuilder")
            .field("data_root", &self.data_root)
            .finish_non_exhaustive()
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
                profiles: Vec::new(),
                platform: None,
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
pub unsafe extern "C" fn coblet_builder_add_platform_profile(
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
            let taken = builder
                .profiles
                .iter()
                .chain(ProfileEngine::builtin().profiles())
                .any(|known| known.id() == profile.id());
            if taken {
                return Err(Failure::argument(
                    "toml",
                    &format!(
                        "declares profile `{}`, which is already registered",
                        profile.id()
                    ),
                ));
            }
            builder.profiles.push(Arc::new(profile));
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
                profiles,
                platform,
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
