use std::ffi::{c_char, c_void};
use std::path::PathBuf;

use coblet::platform::{
    DetachedDraft, Draft, EnvValue, JavaMatch, JavaSource, Natives, NativesSource, ProfileArg,
    ProfileEdit, ProfileError, RuleSettings, Target,
};
use coblet::runtime::Extension;
use coblet::types::{Arch, ArchName, ExtensionId, VersionType};

use crate::context::platform_value;
use crate::customize::{CobletArgKind, Owned, c_library, c_text, trait_names};
use crate::guard::{guard, put};
use crate::types::{
    CobletError, CobletErrorKind, CobletStatus, CobletString, Failure, optional_text, text,
};

/// Callback table of a Platform Profile written in C.
///
/// It adapts launches to a group of platforms, as TOML profiles do, but
/// decides in code. The library copies `id` and `display_name` and owns
/// `user_data` from the call that receives the table, even when that call
/// fails; it calls `free` once when it no longer needs it. Callbacks may
/// run on any thread, one launch at a time per thread.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletPlatformProfile {
    /// ID such as `acme:haiku`.
    pub id: *const c_char,
    /// Name shown to users; may be NULL.
    pub display_name: *const c_char,
    /// Order among third-party profiles; lower values apply first, and
    /// TOML profiles default to 100.
    pub priority: i32,
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Reports whether the profile applies. `target_json` is
    /// `{"schema":1,"platform":{"os","arch","os_version","libc","traits":[]},
    /// "game_version","version_type","release_time","components":[{"uid","version"}],
    /// "legacy_arguments","traits":[]}`, with `null` for unknown values.
    /// When the profile cannot decide, call `coblet_match_fail` with
    /// `failure`: the profile is then skipped with a warning in the launch
    /// report, whatever the callback returns.
    pub matches: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            target_json: *const c_char,
            failure: *mut CobletMatchFailure,
        ) -> bool,
    >,
    /// Says how Mojang's rules are evaluated through `rules`, which is
    /// valid only during the call; called only when the profile matches.
    /// Calling `coblet_rules_fail` stops the launch. May be NULL.
    pub rules: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            target_json: *const c_char,
            rules: *mut CobletRules,
        ),
    >,
    /// Changes the profile through `draft`, which is valid only during the
    /// call; called only when the profile matches. Returning false stops
    /// the launch; call `coblet_draft_fail` first to give the reason. May
    /// be NULL.
    pub rewrite: Option<
        unsafe extern "C" fn(user_data: *mut c_void, draft: *mut CobletProfileDraft) -> bool,
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// Rule setting a `coblet_rules_set` call changes; the names match the
/// `[rules]` table of TOML profiles.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletRule {
    /// Mojang OS name to evaluate as: `windows`, `osx` or `linux`.
    MojangOs = 0,
    /// Architecture string to evaluate as.
    MojangArch = 1,
    /// OS version to evaluate as.
    OsVersion = 2,
    /// Value of `${arch}` in legacy native classifiers (`32` or `64`).
    NativesArch = 3,
}

/// Where the native code of libraries comes from, for `coblet_draft_handle_natives`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletNatives {
    /// Keep the natives Mojang's rules selected and vouch for them.
    Artifact = 0,
    /// The natives are not needed on this platform.
    NotNeeded = 1,
    /// Installed on the system; `location` is the absolute directory.
    System = 2,
    /// The same library with the classifier `classifier` from the Maven
    /// repository at `location`.
    Maven = 3,
}

/// How the Java runtime may be obtained.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletJavaSource {
    /// An installed Java or, failing that, a Mojang runtime.
    Any = 0,
    /// Only an installed Java; nothing is downloaded.
    SystemOnly = 1,
}

/// Which installed Java versions a launch accepts.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletJavaMatch {
    /// Only the required major version.
    Exact = 0,
    /// The required major version or a newer one the components accept.
    AtLeast = 1,
}

/// Reason the `matches` callback cannot decide, given with `coblet_match_fail`.
#[derive(Debug, Default)]
pub struct CobletMatchFailure {
    message: Option<String>,
}

/// Rule settings being changed by the `rules` callback.
#[derive(Debug)]
pub struct CobletRules {
    settings: RuleSettings,
    failure: Option<String>,
}

/// Profile under construction, handed to the `rewrite` callback.
///
/// Each `coblet_draft_*` change applies at once, with the same checks and
/// launch report lines as TOML profiles, and `coblet_draft_info_json`
/// shows it. A change that is not allowed, such as touching an
/// authentication argument, fails with `COBLET_STATUS_INVALID_ARGUMENT`
/// and changes nothing. The changes reach the launch only when `rewrite`
/// returns true.
pub struct CobletProfileDraft {
    draft: DetachedDraft,
    failure: Option<String>,
}

impl std::fmt::Debug for CobletProfileDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletProfileDraft").finish_non_exhaustive()
    }
}

/// Reads the rules handle of a callback.
///
/// # Safety
///
/// `rules` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn rules_mut<'a>(rules: *mut CobletRules) -> Result<&'a mut CobletRules, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { rules.as_mut() }.ok_or_else(|| Failure::argument("rules", "is NULL"))
}

/// Reads the draft handle of a callback.
///
/// # Safety
///
/// `draft` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn draft_mut<'a>(
    draft: *mut CobletProfileDraft,
) -> Result<&'a mut CobletProfileDraft, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { draft.as_mut() }.ok_or_else(|| Failure::argument("draft", "is NULL"))
}

/// Makes the change `body` describes on the draft of the running callback.
///
/// # Safety
///
/// As for [`draft_mut`]; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn apply(
    draft: *mut CobletProfileDraft,
    out_err: *mut *mut CobletError,
    body: impl FnOnce() -> Result<ProfileEdit, Failure>,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let draft = draft_mut(draft)?;
            let edit = body()?;
            draft.draft.draft().apply(edit).map_err(|error| {
                Failure::new(CobletErrorKind::InvalidArgument, error.to_string())
            })?;
            Ok(CobletStatus::Ok)
        })
    }
}

fn absolute(path: &str, name: &str) -> Result<PathBuf, Failure> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(Failure::argument(name, "must be an absolute path"))
    }
}

/// Sets a rule setting; NULL `value` clears what earlier profiles set.
///
/// # Safety
///
/// `rules` must be the handle of the running `rules` callback; `value`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_rules_set(
    rules: *mut CobletRules,
    rule: CobletRule,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let settings = &mut rules_mut(rules)?.settings;
            let value = optional_text(value, "value")?.map(str::to_owned);
            let slot = match rule {
                CobletRule::MojangOs => &mut settings.os_name,
                CobletRule::MojangArch => &mut settings.os_arch,
                CobletRule::OsVersion => &mut settings.os_version,
                CobletRule::NativesArch => &mut settings.natives_arch,
            };
            *slot = value;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Gives the reason the running `rules` callback cannot decide; the launch
/// stops with it once the callback returns.
///
/// # Safety
///
/// `rules` must be the handle of the running `rules` callback; `message`
/// must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_rules_fail(
    rules: *mut CobletRules,
    message: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            rules_mut(rules)?.failure = Some(text(message, "message")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// Gives the reason the running `matches` callback cannot decide; the
/// profile is skipped with a warning once the callback returns.
///
/// # Safety
///
/// `failure` must be the handle passed to the running `matches` callback;
/// `message` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_match_fail(
    failure: *mut CobletMatchFailure,
    message: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            let failure = failure
                .as_mut()
                .ok_or_else(|| Failure::argument("failure", "is NULL"))?;
            failure.message = Some(text(message, "message")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// Says whether the native code Mojang's rules select runs on this platform.
///
/// # Safety
///
/// `rules` must be the handle of the running `rules` callback; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_rules_set_native(
    rules: *mut CobletRules,
    native: bool,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            rules_mut(rules)?.settings.native = native;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Returns the profile with the changes made so far, as JSON.
///
/// The document is `{"schema":1,"game_version","libraries":[{"name",
/// "native_key","has_natives","natives_handled_by","active"}],
/// "jvm_args":[],"game_args":[],"java_major"}`. `native_key` is what
/// `coblet_draft_handle_natives` patterns match. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback;
/// `out_json` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_info_json(
    draft: *mut CobletProfileDraft,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let profile = draft_mut(draft)?.draft.profile();
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let libraries: Vec<serde_json::Value> = profile
                .libraries
                .iter()
                .map(|library| {
                    serde_json::json!({
                        "name": library.name.to_string(),
                        "native_key": library.native_key(),
                        "has_natives": library.natives != Natives::None,
                        "natives_handled_by": match &library.natives {
                            Natives::Handled { by } => Some(by.as_str()),
                            _ => None,
                        },
                        "active": library.is_active(),
                    })
                })
                .collect();
            let values = |args: &[ProfileArg]| -> Vec<String> {
                args.iter().map(|arg| arg.value.clone()).collect()
            };
            let json = serde_json::json!({
                "schema": 1,
                "game_version": profile.game_version.as_str(),
                "libraries": libraries,
                "jvm_args": values(&profile.jvm_args),
                "game_args": values(&profile.game_args),
                "java_major": profile.java.major,
            });
            put(out_json, CobletString::new(&json.to_string()), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Appends a JVM or game argument unless it is already present.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `value`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_append_arg(
    draft: *mut CobletProfileDraft,
    kind: CobletArgKind,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            let arg = ProfileArg::always(text(value, "value")?);
            Ok(match kind {
                CobletArgKind::Jvm => ProfileEdit::AppendJvmArg(arg),
                CobletArgKind::Game => ProfileEdit::AppendGameArg(arg),
            })
        })
    }
}

/// Removes JVM or game arguments matching a glob pattern, such as `-Xss*`.
///
/// A pattern that matches an authentication argument fails with
/// `COBLET_STATUS_INVALID_ARGUMENT` and removes nothing.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `pattern`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_remove_args(
    draft: *mut CobletProfileDraft,
    kind: CobletArgKind,
    pattern: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            let pattern = text(pattern, "pattern")?.to_owned();
            Ok(match kind {
                CobletArgKind::Jvm => ProfileEdit::RemoveJvmArgs(pattern),
                CobletArgKind::Game => ProfileEdit::RemoveGameArgs(pattern),
            })
        })
    }
}

/// Sets an environment variable.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; strings
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_set_env(
    draft: *mut CobletProfileDraft,
    name: *const c_char,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            Ok(ProfileEdit::SetEnv(
                text(name, "name")?.to_owned(),
                EnvValue::Set(text(value, "value")?.to_owned()),
            ))
        })
    }
}

/// Puts `count` directories in front of a path-list variable, such as
/// `LD_LIBRARY_PATH`.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `name`
/// must be NULL or NUL-terminated; `paths` must point to `count`
/// NUL-terminated strings (or be NULL when `count` is 0); `out_err` must
/// be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_prepend_env_paths(
    draft: *mut CobletProfileDraft,
    name: *const c_char,
    paths: *const *const c_char,
    count: usize,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            let name = text(name, "name")?.to_owned();
            let items = if count == 0 {
                &[][..]
            } else if paths.is_null() {
                return Err(Failure::argument("paths", "is NULL"));
            } else {
                std::slice::from_raw_parts(paths, count)
            };
            let paths = items
                .iter()
                .map(|path| text(*path, "paths").map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ProfileEdit::SetEnv(name, EnvValue::PrependPaths(paths)))
        })
    }
}

/// Adds an absolute directory with system native libraries.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `path`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_add_native_path(
    draft: *mut CobletProfileDraft,
    path: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            Ok(ProfileEdit::AddNativePath(absolute(
                text(path, "path")?,
                "path",
            )?))
        })
    }
}

/// Says where the native code of libraries whose `native_key` matches a
/// glob pattern comes from, and marks it handled so the launch accepts it.
/// The first call that matches a library wins.
///
/// `location` is the directory for `COBLET_NATIVES_SYSTEM` and the
/// repository URL for `COBLET_NATIVES_MAVEN`; `classifier` is used only
/// for Maven. Both may be NULL otherwise.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; strings
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_handle_natives(
    draft: *mut CobletProfileDraft,
    pattern: *const c_char,
    natives: CobletNatives,
    location: *const c_char,
    classifier: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            let pattern = text(pattern, "pattern")?.to_owned();
            let source = match natives {
                CobletNatives::Artifact => NativesSource::Artifact,
                CobletNatives::NotNeeded => NativesSource::NotNeeded,
                CobletNatives::System => {
                    NativesSource::System(absolute(text(location, "location")?, "location")?)
                }
                CobletNatives::Maven => NativesSource::Maven {
                    repository: text(location, "location")?.to_owned(),
                    classifier: text(classifier, "classifier")?.to_owned(),
                },
            };
            Ok(ProfileEdit::HandleNatives { pattern, source })
        })
    }
}

/// Adds a library before the first one whose name matches the glob
/// `before` (`"*"` puts it first), or last when `before` is NULL.
///
/// `name` is a Maven coordinate; `path` is inside the libraries directory,
/// with `/` separators. Without `url` the file must already be there.
/// `sha1` (hex) may be NULL; `size` 0 means unknown.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; strings
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_add_library(
    draft: *mut CobletProfileDraft,
    name: *const c_char,
    path: *const c_char,
    url: *const c_char,
    sha1: *const c_char,
    size: u64,
    before: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            Ok(ProfileEdit::AddLibrary {
                library: Box::new(c_library(name, path, url, sha1, size)?),
                before: optional_text(before, "before")?.map(str::to_owned),
            })
        })
    }
}

/// Removes libraries whose name matches a glob pattern.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `pattern`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_remove_libraries(
    draft: *mut CobletProfileDraft,
    pattern: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            Ok(ProfileEdit::RemoveLibraries(
                text(pattern, "pattern")?.to_owned(),
            ))
        })
    }
}

/// Sets the Java major version.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_set_java_major(
    draft: *mut CobletProfileDraft,
    major: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { apply(draft, out_err, || Ok(ProfileEdit::JavaMajor(major))) }
}

/// Sets how the Java runtime may be obtained.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_set_java_source(
    draft: *mut CobletProfileDraft,
    source: CobletJavaSource,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    let source = match source {
        CobletJavaSource::Any => JavaSource::Any,
        CobletJavaSource::SystemOnly => JavaSource::SystemOnly,
    };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { apply(draft, out_err, || Ok(ProfileEdit::JavaSource(source))) }
}

/// Sets which installed Java versions are accepted.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `out_err`
/// must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_set_java_match(
    draft: *mut CobletProfileDraft,
    matching: CobletJavaMatch,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    let matching = match matching {
        CobletJavaMatch::Exact => JavaMatch::Exact,
        CobletJavaMatch::AtLeast => JavaMatch::AtLeast,
    };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe { apply(draft, out_err, || Ok(ProfileEdit::JavaMatch(matching))) }
}

/// Sets the platform key of the Mojang Java runtime, such as `linux`.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `runtime`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_set_java_runtime(
    draft: *mut CobletProfileDraft,
    runtime: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            Ok(ProfileEdit::JavaRuntime(
                text(runtime, "runtime")?.to_owned(),
            ))
        })
    }
}

/// Sets the architecture the Java runtime must be built for, such as `x86_64`.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `arch`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_set_java_arch(
    draft: *mut CobletProfileDraft,
    arch: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            let name = ArchName::new(text(arch, "arch")?)
                .map_err(|error| Failure::argument("arch", &error.to_string()))?;
            Ok(ProfileEdit::JavaArch(Arch::from(name)))
        })
    }
}

/// Raises a warning shown in the launch report.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `message`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_warn(
    draft: *mut CobletProfileDraft,
    message: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            Ok(ProfileEdit::Warn(text(message, "message")?.to_owned()))
        })
    }
}

/// Refuses to launch this combination; the launch fails as unsupported
/// with this reason.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `reason`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_refuse(
    draft: *mut CobletProfileDraft,
    reason: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        apply(draft, out_err, || {
            Ok(ProfileEdit::Refuse(text(reason, "reason")?.to_owned()))
        })
    }
}

/// Gives the reason the running `rewrite` callback is about to return false.
///
/// # Safety
///
/// `draft` must be the handle of the running `rewrite` callback; `message`
/// must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_draft_fail(
    draft: *mut CobletProfileDraft,
    message: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            draft_mut(draft)?.failure = Some(text(message, "message")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

type MatchesFn = unsafe extern "C" fn(*mut c_void, *const c_char, *mut CobletMatchFailure) -> bool;
type RulesFn = unsafe extern "C" fn(*mut c_void, *const c_char, *mut CobletRules);
type RewriteFn = unsafe extern "C" fn(*mut c_void, *mut CobletProfileDraft) -> bool;

/// C profile registered with the Rust profile engine.
pub(crate) struct ForeignProfile {
    id: ExtensionId,
    display_name: String,
    priority: i32,
    matches: MatchesFn,
    rules: Option<RulesFn>,
    rewrite: Option<RewriteFn>,
    owned: Owned,
}

impl std::fmt::Debug for ForeignProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForeignProfile")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ForeignProfile {
    /// Takes ownership of the table's user data and checks the table.
    ///
    /// # Safety
    ///
    /// Strings in the table must be NULL or NUL-terminated; the callbacks
    /// must stay valid until `free` runs.
    #[allow(unsafe_code, reason = "reads the callback table")]
    pub(crate) unsafe fn new(table: &CobletPlatformProfile) -> Result<Self, Failure> {
        let owned = Owned {
            user_data: table.user_data,
            free: table.free,
        };
        // SAFETY: forwarded from the caller's guarantee.
        let id = unsafe { text(table.id, "platform profile id") }?;
        let id = ExtensionId::new(id)
            .map_err(|error| Failure::argument("platform profile id", &error.to_string()))?;
        // SAFETY: forwarded from the caller's guarantee.
        let display_name = unsafe { optional_text(table.display_name, "display_name") }?
            .unwrap_or(id.as_str())
            .to_owned();
        let matches = table
            .matches
            .ok_or_else(|| Failure::argument("platform profile matches", "is NULL"))?;
        Ok(Self {
            id,
            display_name,
            priority: table.priority,
            matches,
            rules: table.rules,
            rewrite: table.rewrite,
            owned,
        })
    }
}

impl ForeignProfile {
    fn failed(&self, message: String) -> ProfileError {
        ProfileError::Failed {
            profile: self.id.clone(),
            message,
        }
    }
}

impl Extension for ForeignProfile {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }
}

fn target_json(target: &Target<'_>) -> std::ffi::CString {
    let profile = target.profile;
    let components: Vec<serde_json::Value> = profile
        .components
        .iter()
        .map(|component| serde_json::json!({"uid": component.uid, "version": component.version}))
        .collect();
    let json = serde_json::json!({
        "schema": 1,
        "platform": platform_value(target.platform),
        "game_version": profile.game_version.as_str(),
        "version_type": profile.version_type.as_ref().map(VersionType::as_str),
        "release_time": profile.release_time.map(|time| time.to_string()),
        "components": components,
        "legacy_arguments": profile.legacy_arguments,
        "traits": trait_names(&profile.traits),
    });
    c_text(&json.to_string())
}

impl coblet::platform::PlatformProfile for ForeignProfile {
    fn priority(&self) -> i32 {
        self.priority
    }

    fn matches(&self, target: &Target<'_>) -> bool {
        self.try_matches(target).unwrap_or(false)
    }

    #[allow(unsafe_code, reason = "calls the C callback")]
    fn try_matches(&self, target: &Target<'_>) -> Result<bool, ProfileError> {
        let json = target_json(target);
        let mut failure = CobletMatchFailure::default();
        // SAFETY: the callback and user data are valid until `free`; the
        // string and the failure handle live for the call.
        let matches =
            unsafe { (self.matches)(self.owned.user_data, json.as_ptr(), &raw mut failure) };
        match failure.message {
            Some(message) => Err(self.failed(message)),
            None => Ok(matches),
        }
    }

    fn rules(&self, target: &Target<'_>, settings: &mut RuleSettings) {
        // Only reached by callers that ignore errors; the engine uses try_rules.
        let _ = self.try_rules(target, settings);
    }

    #[allow(unsafe_code, reason = "calls the C callback")]
    fn try_rules(
        &self,
        target: &Target<'_>,
        settings: &mut RuleSettings,
    ) -> Result<(), ProfileError> {
        let Some(rules) = self.rules else {
            return Ok(());
        };
        let json = target_json(target);
        let mut handle = CobletRules {
            settings: settings.clone(),
            failure: None,
        };
        // SAFETY: the callback and user data are valid until `free`; the
        // string and the handle live for the call.
        unsafe { rules(self.owned.user_data, json.as_ptr(), &raw mut handle) };
        if let Some(message) = handle.failure {
            return Err(self.failed(message));
        }
        *settings = handle.settings;
        Ok(())
    }

    #[allow(unsafe_code, reason = "calls the C callback")]
    fn rewrite(&self, draft: &mut Draft<'_>) -> Result<(), ProfileError> {
        let Some(rewrite) = self.rewrite else {
            return Ok(());
        };
        let mut handle = CobletProfileDraft {
            draft: draft.detach(),
            failure: None,
        };
        // SAFETY: the callback and user data are valid until `free`; the
        // handle lives for the call.
        let ok = unsafe { rewrite(self.owned.user_data, &raw mut handle) };
        if !ok {
            return Err(self.failed(
                handle
                    .failure
                    .unwrap_or_else(|| "the profile failed".to_owned()),
            ));
        }
        draft.attach(handle.draft);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use coblet::mojang::{RawVersionJson, TraitTable};
    use coblet::platform::{
        EffectiveLaunchProfile, JavaSource, ProfileEngine, ProfileError, ResolvedProfile,
    };
    use coblet::types::{Arch, ArchName, OsFamily, Platform};

    use super::{
        CobletJavaSource, CobletMatchFailure, CobletNatives, CobletPlatformProfile,
        CobletProfileDraft, CobletRule, CobletRules, ForeignProfile, coblet_draft_add_native_path,
        coblet_draft_append_arg, coblet_draft_fail, coblet_draft_handle_natives,
        coblet_draft_info_json, coblet_draft_set_java_source, coblet_match_fail, coblet_rules_fail,
        coblet_rules_set,
    };
    use crate::builder::{
        coblet_builder_add_platform_profile, coblet_builder_free, coblet_builder_new,
    };
    use crate::customize::CobletArgKind;
    use crate::types::{CobletStatus, CobletString};

    /// State behind `user_data`, as a C program would keep it.
    #[derive(Default, Clone, Copy, PartialEq, Eq)]
    enum Fail {
        #[default]
        Nowhere,
        Matches,
        Rules,
        Rewrite,
    }

    #[derive(Default)]
    struct State {
        fail: Fail,
        frees: AtomicUsize,
    }

    fn state(user_data: *mut c_void) -> &'static State {
        // SAFETY: every test passes a pointer to a `State` that outlives the profile.
        unsafe { &*user_data.cast::<State>() }
    }

    fn json(ptr: *const c_char) -> serde_json::Value {
        // SAFETY: the library passes a NUL-terminated string.
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    unsafe extern "C" fn matches(
        user_data: *mut c_void,
        target_json: *const c_char,
        failure: *mut CobletMatchFailure,
    ) -> bool {
        if state(user_data).fail == Fail::Matches {
            let message = CString::new("cannot read /proc/cpuinfo").unwrap();
            // SAFETY: the failure handle of this call and a valid string.
            unsafe { coblet_match_fail(failure, message.as_ptr()) };
            return true;
        }
        let target = json(target_json);
        target["platform"]["arch"] == "loongarch64"
            && target["components"]
                .as_array()
                .unwrap()
                .iter()
                .any(|component| component["uid"] == "org.lwjgl3")
    }

    unsafe extern "C" fn rules(
        user_data: *mut c_void,
        _target: *const c_char,
        rules: *mut CobletRules,
    ) {
        if state(user_data).fail == Fail::Rules {
            let message = CString::new("unknown kernel").unwrap();
            // SAFETY: the handle of this call and a valid string.
            unsafe { coblet_rules_fail(rules, message.as_ptr()) };
            return;
        }
        let linux = CString::new("linux").unwrap();
        // SAFETY: the handle of this call and a valid string.
        let status = unsafe {
            coblet_rules_set(
                rules,
                CobletRule::MojangOs,
                linux.as_ptr(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, CobletStatus::Ok);
    }

    unsafe extern "C" fn rewrite(user_data: *mut c_void, draft: *mut CobletProfileDraft) -> bool {
        let null = std::ptr::null_mut();
        let text = |value: &str| CString::new(value).unwrap();
        // SAFETY: `draft` is the handle of this call; strings live for each call.
        unsafe {
            if state(user_data).fail == Fail::Rewrite {
                let message = text("no GPU driver found");
                coblet_draft_fail(draft, message.as_ptr());
                return false;
            }
            let relative = text("lib");
            assert_eq!(
                coblet_draft_add_native_path(draft, relative.as_ptr(), null),
                CobletStatus::InvalidArgument
            );
            // A rejected change fails at the call; the profile can go on.
            let token = text("--accessToken");
            assert_eq!(
                coblet_draft_append_arg(draft, CobletArgKind::Game, token.as_ptr(), null),
                CobletStatus::InvalidArgument
            );
            let mut out = CobletString::empty();
            assert_eq!(
                coblet_draft_info_json(draft, &raw mut out, null),
                CobletStatus::Ok
            );
            let info = json(out.ptr);
            crate::context::coblet_string_free(out);
            let system = text("/usr/lib/lwjgl3");
            for library in info["libraries"].as_array().unwrap() {
                if library["has_natives"] != true {
                    continue;
                }
                let key = library["native_key"].as_str().unwrap();
                let natives = if key.ends_with(":natives-linux") {
                    CobletNatives::System
                } else {
                    CobletNatives::NotNeeded
                };
                let pattern = text(key);
                let status = coblet_draft_handle_natives(
                    draft,
                    pattern.as_ptr(),
                    natives,
                    system.as_ptr(),
                    std::ptr::null(),
                    null,
                );
                assert_eq!(status, CobletStatus::Ok);
            }
            let arg = text("-Dorg.lwjgl.librarypath=/usr/lib/lwjgl3");
            assert_eq!(
                coblet_draft_append_arg(draft, CobletArgKind::Jvm, arg.as_ptr(), null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_draft_set_java_source(draft, CobletJavaSource::SystemOnly, null),
                CobletStatus::Ok
            );
            // Reads show the changes made so far.
            let mut out = CobletString::empty();
            assert_eq!(
                coblet_draft_info_json(draft, &raw mut out, null),
                CobletStatus::Ok
            );
            let info = json(out.ptr);
            crate::context::coblet_string_free(out);
            assert!(
                info["jvm_args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "-Dorg.lwjgl.librarypath=/usr/lib/lwjgl3")
            );
        }
        true
    }

    unsafe extern "C" fn free(user_data: *mut c_void) {
        state(user_data).frees.fetch_add(1, Ordering::SeqCst);
    }

    fn table(state: &State, id: &CStr) -> CobletPlatformProfile {
        CobletPlatformProfile {
            id: id.as_ptr(),
            display_name: std::ptr::null(),
            priority: 100,
            user_data: std::ptr::from_ref(state).cast_mut().cast(),
            matches: Some(matches),
            rules: Some(rules),
            rewrite: Some(rewrite),
            free: Some(free),
        }
    }

    fn resolved() -> ResolvedProfile {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/mojang/1.20.1.json");
        let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        ResolvedProfile::from_version(raw, traits).unwrap()
    }

    fn resolve(state: &State) -> Result<EffectiveLaunchProfile, ProfileError> {
        let id = CString::new("acme:loongarch").unwrap();
        // SAFETY: the table is valid and `state` outlives the engine.
        let profile = unsafe { ForeignProfile::new(&table(state, &id)) }.unwrap();
        let mut engine = ProfileEngine::builtin();
        engine.register(std::sync::Arc::new(profile)).unwrap();
        let loongarch = Platform::new(
            OsFamily::Linux,
            Arch::from(ArchName::new("loongarch64").unwrap()),
        );
        engine.resolve(&resolved(), &loongarch)
    }

    #[test]
    fn a_c_profile_adapts_a_new_architecture() {
        let state = State::default();
        let profile = resolve(&state).unwrap();
        assert_eq!(profile.java.source, JavaSource::SystemOnly);
        assert_eq!(
            profile.native_paths,
            [std::path::PathBuf::from("/usr/lib/lwjgl3")]
        );
        let explain = profile.explain();
        assert!(
            explain.contains("[evaluate] acme:loongarch: rules evaluate as os `linux`"),
            "{explain}"
        );
        assert!(explain.contains("natives from system path `/usr/lib/lwjgl3`"));
        assert!(explain.contains(
            "[rewrite] acme:loongarch: jvm `-Dorg.lwjgl.librarypath=/usr/lib/lwjgl3` appended"
        ));
        assert_eq!(state.frees.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failures_after_matching_stop_the_launch() {
        let failing = |fail| State {
            fail,
            ..State::default()
        };
        let error = resolve(&failing(Fail::Rewrite)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "platform profile acme:loongarch failed: no GPU driver found"
        );
        let error = resolve(&failing(Fail::Rules)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "platform profile acme:loongarch failed: unknown kernel"
        );
    }

    #[test]
    fn a_profile_that_cannot_match_is_skipped() {
        let state = State {
            fail: Fail::Matches,
            ..State::default()
        };
        let error = resolve(&state).unwrap_err();
        let ProfileError::Unsupported { explain, .. } = &error else {
            panic!("{error}");
        };
        assert!(
        explain.contains(
            "[match] acme:loongarch: warning `skipped because matching failed: cannot read /proc/cpuinfo`"
        ),
        "{explain}"
    );
    }

    #[test]
    fn tables_are_freed_once_even_when_rejected() {
        let state = State::default();
        let good = CString::new("acme:loongarch").unwrap();
        let root = CString::new("/tmp/coblet-ffi-profiles").unwrap();
        let agent = CString::new("coblet-ffi-test/1.0").unwrap();
        let mut builder = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        // SAFETY: valid strings, tables and out-pointers; the builder is freed once.
        unsafe {
            let status = coblet_builder_new(
                root.as_ptr(),
                agent.as_ptr(),
                &raw mut builder,
                &raw mut err,
            );
            assert_eq!(status, CobletStatus::Ok);
            let status =
                coblet_builder_add_platform_profile(builder, table(&state, &good), &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let status =
                coblet_builder_add_platform_profile(builder, table(&state, &good), &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            let message = CStr::from_ptr((*err).message.ptr).to_str().unwrap();
            assert!(message.contains("already registered"), "{message}");
            crate::context::coblet_error_free(err);
            let mut no_matches = table(&state, &good);
            no_matches.matches = None;
            let status =
                coblet_builder_add_platform_profile(builder, no_matches, std::ptr::null_mut());
            assert_eq!(status, CobletStatus::InvalidArgument);
            assert_eq!(state.frees.load(Ordering::SeqCst), 2);
            coblet_builder_free(builder);
        }
        assert_eq!(state.frees.load(Ordering::SeqCst), 3);
    }
}
