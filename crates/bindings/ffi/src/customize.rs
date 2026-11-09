use std::ffi::{CString, c_char, c_void};
use std::path::{Component, Path};
use std::sync::Arc;

use coblet::launch::{CustomizeCtx, LaunchError};
use coblet::mojang::{VersionTrait, VersionTraits};
use coblet::platform::{
    EffectiveLaunchProfile, EffectiveLibrary, EnvValue, FileSource, LibraryFile, ProfileArg,
};
use coblet::runtime::Extension;
use coblet::types::{ExtensionId, MavenCoordinate, Sha1Digest};

use crate::guard::{guard, put};
use crate::steps::{CobletStep, ForeignStep};
use crate::types::{CobletError, CobletStatus, CobletString, Failure, text};

/// Callback table of a launch customizer written in C.
///
/// The library copies `id` and `display_name` and owns `user_data` from
/// the call that receives the table, even when that call fails; it calls
/// `free` once when it no longer needs it. Callbacks may run on any thread,
/// one at a time per launch.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletCustomizer {
    /// ID such as `acme:legacy-fix`.
    pub id: *const c_char,
    /// Name shown to users; may be NULL.
    pub display_name: *const c_char,
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Reports whether the customizer handles a version; `traits_json` is
    /// a JSON array such as `["applet","jre-legacy"]`. NULL applies to every version.
    pub applies_to:
        Option<unsafe extern "C" fn(user_data: *mut c_void, traits_json: *const c_char) -> bool>,
    /// Edits one launch through `edit`, which is valid only during the
    /// call. Returning false stops the launch; call `coblet_edit_fail`
    /// first to give the reason.
    pub customize:
        Option<unsafe extern "C" fn(user_data: *mut c_void, edit: *mut CobletProfileEdit) -> bool>,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// Argument list a `coblet_edit_*_arg` call works on.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CobletArgKind {
    /// JVM arguments.
    Jvm = 0,
    /// Game arguments.
    Game = 1,
}

#[derive(Clone, Copy)]
enum Placement {
    Before,
    After,
}

/// `user_data` with its `free` callback, released on drop.
pub(crate) struct Owned {
    pub(crate) user_data: *mut c_void,
    pub(crate) free: Option<unsafe extern "C" fn(*mut c_void)>,
}

// SAFETY: the callback tables document that `user_data` and the callbacks
// may be used from any thread, one call at a time per launch.
#[allow(unsafe_code, reason = "C user data crosses threads")]
unsafe impl Send for Owned {}
// SAFETY: as for `Send`.
#[allow(unsafe_code, reason = "C user data crosses threads")]
unsafe impl Sync for Owned {}

impl Drop for Owned {
    #[allow(unsafe_code, reason = "calls the C free callback")]
    fn drop(&mut self) {
        if let Some(free) = self.free {
            // SAFETY: the table owner handed `user_data` to the library and
            // `free` is called exactly once, here.
            unsafe { free(self.user_data) };
        }
    }
}

struct PendingStep {
    anchor: String,
    placement: Placement,
    step: ForeignStep,
}

/// Launch profile being edited by a C customizer; valid only during its
/// `customize` callback.
pub struct CobletProfileEdit {
    profile: EffectiveLaunchProfile,
    steps: Vec<PendingStep>,
    placeholders: Vec<(String, String)>,
    failure: Option<String>,
    info: serde_json::Value,
}

impl std::fmt::Debug for CobletProfileEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletProfileEdit").finish_non_exhaustive()
    }
}

pub(crate) fn trait_names(traits: &VersionTraits) -> Vec<&str> {
    traits.iter().map(VersionTrait::as_str).collect()
}

fn args(profile: &EffectiveLaunchProfile, kind: CobletArgKind) -> &Vec<ProfileArg> {
    match kind {
        CobletArgKind::Jvm => &profile.jvm_args,
        CobletArgKind::Game => &profile.game_args,
    }
}

fn args_mut(profile: &mut EffectiveLaunchProfile, kind: CobletArgKind) -> &mut Vec<ProfileArg> {
    match kind {
        CobletArgKind::Jvm => &mut profile.jvm_args,
        CobletArgKind::Game => &mut profile.game_args,
    }
}

fn values(args: &[ProfileArg]) -> Vec<&str> {
    args.iter().map(|arg| arg.value.as_str()).collect()
}

fn library_path(path: &str) -> Result<&str, Failure> {
    let normal = !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if normal {
        Ok(path)
    } else {
        Err(Failure::argument(
            "path",
            "must be relative, use `/` and stay inside the libraries directory",
        ))
    }
}

/// Reads a library from C arguments: a Maven coordinate, a path inside the
/// libraries directory, and an optional URL, SHA-1 (hex) and size (0 for
/// unknown).
///
/// # Safety
///
/// Strings must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "reads C string arguments")]
pub(crate) unsafe fn c_library(
    name: *const c_char,
    path: *const c_char,
    url: *const c_char,
    sha1: *const c_char,
    size: u64,
) -> Result<EffectiveLibrary, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    let (name, path, url, sha1) = unsafe {
        (
            text(name, "name")?,
            text(path, "path")?,
            crate::types::optional_text(url, "url")?,
            crate::types::optional_text(sha1, "sha1")?,
        )
    };
    let name: MavenCoordinate =
        name.parse()
            .map_err(|error: coblet::types::MavenCoordinateError| {
                Failure::argument("name", &error.to_string())
            })?;
    let path = library_path(path)?;
    let sha1 = sha1
        .map(Sha1Digest::from_hex)
        .transpose()
        .map_err(|error| Failure::argument("sha1", &error.to_string()))?;
    let source = match url {
        Some(url) => FileSource::Download {
            url: url.to_owned(),
            sha1,
            size: (size > 0).then_some(size),
        },
        None => FileSource::Existing,
    };
    Ok(EffectiveLibrary::new(
        name,
        Some(LibraryFile::new(path, source)),
    ))
}

/// Reads the edit handle of a callback.
///
/// # Safety
///
/// `edit` must be NULL or the handle passed to the running callback.
#[allow(unsafe_code, reason = "reads a handle argument")]
unsafe fn edit_mut<'a>(edit: *mut CobletProfileEdit) -> Result<&'a mut CobletProfileEdit, Failure> {
    // SAFETY: forwarded from the caller's guarantee.
    unsafe { edit.as_mut() }.ok_or_else(|| Failure::argument("edit", "is NULL"))
}

/// Returns what the customizer can see as JSON.
///
/// The document is `{"schema":1,"game_version","os","arch","data_root",
/// "game_dir","traits":[],"main_class","jvm_args":[],"game_args":[],
/// "libraries":[],"java_major"}`; `libraries` lists active libraries in
/// classpath order, natives with their classifier. On success `*out_json`
/// receives a string to free with `coblet_string_free`.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback;
/// `out_json` must be writable; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_info_json(
    edit: *mut CobletProfileEdit,
    out_json: *mut CobletString,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            if out_json.is_null() {
                return Err(Failure::argument("out_json", "is NULL"));
            }
            let profile = &edit.profile;
            let libraries: Vec<String> = profile
                .libraries
                .iter()
                .filter(|library| library.is_active())
                .map(EffectiveLibrary::native_key)
                .collect();
            let mut info = edit.info.clone();
            if let Some(map) = info.as_object_mut() {
                map.insert("traits".into(), trait_names(&profile.traits).into());
                map.insert("main_class".into(), profile.main_class.clone().into());
                map.insert("jvm_args".into(), values(&profile.jvm_args).into());
                map.insert("game_args".into(), values(&profile.game_args).into());
                map.insert("libraries".into(), libraries.into());
                map.insert("java_major".into(), profile.java.major.into());
            }
            put(out_json, CobletString::new(&info.to_string()), "out_json")?;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Replaces the main class.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback;
/// `main_class` must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_set_main_class(
    edit: *mut CobletProfileEdit,
    main_class: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            text(main_class, "main_class")?.clone_into(&mut edit.profile.main_class);
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds an argument, such as `-javaagent:/path/fix.jar`, last or first.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; `value`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_add_arg(
    edit: *mut CobletProfileEdit,
    kind: CobletArgKind,
    value: *const c_char,
    first: bool,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let arg = ProfileArg::always(text(value, "value")?);
            let list = args_mut(&mut edit.profile, kind);
            if first {
                list.insert(0, arg);
            } else {
                list.push(arg);
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Removes every argument equal to `value`; `*out_removed` tells whether
/// one was found.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; `value`
/// must be NULL or NUL-terminated; `out_removed` must be NULL or writable;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_remove_arg(
    edit: *mut CobletProfileEdit,
    kind: CobletArgKind,
    value: *const c_char,
    out_removed: *mut bool,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let value = text(value, "value")?;
            let before = args(&edit.profile, kind).len();
            let list = args_mut(&mut edit.profile, kind);
            list.retain(|arg| arg.value != value);
            let removed = list.len() != before;
            if !out_removed.is_null() {
                put(out_removed, removed, "out_removed")?;
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a classpath library, last or first.
///
/// `name` is a Maven coordinate such as `com.example:fix:1.0`; `path` is
/// inside the libraries directory, with `/` separators. Without `url` the
/// file must already be there. `sha1` (hex) may be NULL; `size` 0 means
/// unknown.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; strings
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_add_library(
    edit: *mut CobletProfileEdit,
    name: *const c_char,
    path: *const c_char,
    url: *const c_char,
    sha1: *const c_char,
    size: u64,
    first: bool,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let library = c_library(name, path, url, sha1, size)?;
            if first {
                edit.profile.libraries.insert(0, library);
            } else {
                edit.profile.libraries.push(library);
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Removes the libraries listed as `name` in `coblet_edit_info_json`;
/// `*out_removed` tells whether one was found.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; `name`
/// must be NULL or NUL-terminated; `out_removed` must be NULL or writable;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_remove_library(
    edit: *mut CobletProfileEdit,
    name: *const c_char,
    out_removed: *mut bool,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let name = text(name, "name")?;
            let before = edit.profile.libraries.len();
            edit.profile
                .libraries
                .retain(|library| library.native_key() != name);
            let removed = edit.profile.libraries.len() != before;
            if !out_removed.is_null() {
                put(out_removed, removed, "out_removed")?;
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Sets an environment variable for the game; a NULL `value` removes one
/// the profile sets.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; strings
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_set_env(
    edit: *mut CobletProfileEdit,
    name: *const c_char,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let name = text(name, "name")?;
            match crate::types::optional_text(value, "value")? {
                Some(value) => {
                    edit.profile
                        .env
                        .insert(name.to_owned(), EnvValue::Set(value.to_owned()));
                }
                None => {
                    edit.profile.env.remove(name);
                }
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Adds a directory to the native library search path.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; `path`
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_add_native_path(
    edit: *mut CobletProfileEdit,
    path: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let path = text(path, "path")?.into();
            if !edit.profile.native_paths.contains(&path) {
                edit.profile.native_paths.push(path);
            }
            Ok(CobletStatus::Ok)
        })
    }
}

/// Sets the Java major version the game needs.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback;
/// `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_set_java_major(
    edit: *mut CobletProfileEdit,
    major: u32,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            edit_mut(edit)?.profile.java.major = major;
            Ok(CobletStatus::Ok)
        })
    }
}

/// Runs `step` right before or after the launch step named `anchor`, such
/// as `pre-launch`; an unknown anchor fails the launch.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; strings
/// must be NULL or NUL-terminated; the step's callbacks must stay valid
/// until its `free` runs; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_add_step(
    edit: *mut CobletProfileEdit,
    anchor: *const c_char,
    after: bool,
    step: CobletStep,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: the caller guarantees the table; the adapter owns the user
    // data from here on.
    let adapter = unsafe { ForeignStep::new(&step) };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let anchor = text(anchor, "anchor")?.to_owned();
            edit.steps.push(PendingStep {
                anchor,
                placement: if after {
                    Placement::After
                } else {
                    Placement::Before
                },
                step: adapter?,
            });
            Ok(CobletStatus::Ok)
        })
    }
}

/// Sets `${name}` to `value` in this launch, replacing the value of an
/// earlier customizer. A name coblet fills itself, such as
/// `auth_access_token`, an empty name or one with `$`, `{` or `}` fails
/// with `COBLET_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback; strings
/// must be NULL or NUL-terminated; `out_err` must be NULL or writable.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_set_placeholder(
    edit: *mut CobletProfileEdit,
    name: *const c_char,
    value: *const c_char,
    out_err: *mut *mut CobletError,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let name = crate::placeholders::check_name(text(name, "name")?)?;
            edit.placeholders
                .push((name, text(value, "value")?.to_owned()));
            Ok(CobletStatus::Ok)
        })
    }
}

/// Gives the reason the running `customize` callback is about to return false.
///
/// # Safety
///
/// `edit` must be the handle of the running `customize` callback;
/// `message` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_edit_fail(
    edit: *mut CobletProfileEdit,
    message: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            edit_mut(edit)?.failure = Some(text(message, "message")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

/// C customizer registered with the Rust launcher.
pub(crate) struct ForeignCustomizer {
    id: ExtensionId,
    display_name: String,
    applies_to: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> bool>,
    customize: unsafe extern "C" fn(*mut c_void, *mut CobletProfileEdit) -> bool,
    owned: Owned,
}

impl ForeignCustomizer {
    /// Takes ownership of the table's user data and checks the table.
    ///
    /// # Safety
    ///
    /// Strings in the table must be NULL or NUL-terminated; the callbacks
    /// must stay valid until `free` runs.
    #[allow(unsafe_code, reason = "reads the callback table")]
    pub(crate) unsafe fn new(table: &CobletCustomizer) -> Result<Self, Failure> {
        let owned = Owned {
            user_data: table.user_data,
            free: table.free,
        };
        // SAFETY: forwarded from the caller's guarantee.
        let id = unsafe { text(table.id, "customizer id") }?;
        let id = ExtensionId::new(id)
            .map_err(|error| Failure::argument("customizer id", &error.to_string()))?;
        // SAFETY: forwarded from the caller's guarantee.
        let display_name =
            unsafe { crate::types::optional_text(table.display_name, "display_name") }?
                .unwrap_or(id.as_str())
                .to_owned();
        let customize = table
            .customize
            .ok_or_else(|| Failure::argument("customizer customize", "is NULL"))?;
        Ok(Self {
            id,
            display_name,
            applies_to: table.applies_to,
            customize,
            owned,
        })
    }
}

impl Extension for ForeignCustomizer {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }
}

pub(crate) fn c_text(value: &str) -> CString {
    CString::new(value.replace('\0', "\u{fffd}")).unwrap_or_default()
}

impl coblet::launch::LaunchCustomizer for ForeignCustomizer {
    #[allow(unsafe_code, reason = "calls the C callback")]
    fn applies_to(&self, traits: &VersionTraits) -> bool {
        let Some(applies_to) = self.applies_to else {
            return true;
        };
        let json = c_text(&serde_json::Value::from(trait_names(traits)).to_string());
        // SAFETY: the callback and user data are valid until `free`, and
        // the string lives for the call.
        unsafe { applies_to(self.owned.user_data, json.as_ptr()) }
    }

    #[allow(unsafe_code, reason = "calls the C callback")]
    fn customize(
        &self,
        profile: &mut EffectiveLaunchProfile,
        ctx: &CustomizeCtx,
    ) -> Result<(), LaunchError> {
        let mut edit = CobletProfileEdit {
            profile: profile.clone(),
            steps: Vec::new(),
            placeholders: Vec::new(),
            failure: None,
            info: serde_json::json!({
                "schema": 1,
                "game_version": profile.game_version.as_str(),
                "os": ctx.platform.os().as_str(),
                "arch": ctx.platform.arch().as_str(),
                "data_root": ctx.layout.data_root().to_string_lossy(),
                "game_dir": ctx.layout.game_directory().to_string_lossy(),
            }),
        };
        // SAFETY: the callback and user data are valid until `free`; the
        // edit handle lives for the call.
        let ok = unsafe { (self.customize)(self.owned.user_data, &raw mut edit) };
        if !ok {
            return Err(LaunchError::Step {
                step: self.id.to_string(),
                message: edit
                    .failure
                    .unwrap_or_else(|| "the customizer failed".to_owned()),
            });
        }
        *profile = edit.profile;
        for (name, value) in edit.placeholders {
            ctx.set_placeholder(name, value)?;
        }
        for pending in edit.steps {
            let step = Arc::new(pending.step);
            match pending.placement {
                Placement::Before => ctx.add_step_before(pending.anchor, step),
                Placement::After => ctx.add_step_after(pending.anchor, step),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use coblet::catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet::download::{Downloader, DownloaderConfig};
    use coblet::launch::{
        CustomizeCtx, GameLayout, LaunchContext, LaunchError, LaunchRequest, LaunchServices,
        Launcher,
    };
    use coblet::launch_command::LaunchAuth;
    use coblet::mojang::{RawVersionJson, TraitTable};
    use coblet::platform::{EffectiveLaunchProfile, ProfileEngine, ResolvedProfile};
    use coblet::runtime::{
        BoxFuture, Request, Response, TokioSpawner, TokioStorage, Transport, TransportError,
    };
    use coblet::types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

    use super::{
        CobletArgKind, CobletCustomizer, CobletProfileEdit, ForeignCustomizer, coblet_edit_add_arg,
        coblet_edit_add_library, coblet_edit_add_step, coblet_edit_fail, coblet_edit_info_json,
        coblet_edit_remove_library, coblet_edit_set_env, coblet_edit_set_main_class,
        coblet_edit_set_placeholder,
    };
    use crate::builder::{
        coblet_builder_add_customizer, coblet_builder_allow_auth_changes, coblet_builder_build,
        coblet_builder_free, coblet_builder_new,
    };
    use crate::steps::{CobletStep, CobletStepCtx, coblet_step_ctx_fail};
    use crate::types::{CobletStatus, CobletString};

    /// State behind `user_data`, as a C program would keep it.
    #[derive(Default)]
    struct State {
        fail: bool,
        auth: bool,
        frees: AtomicUsize,
        step_runs: AtomicUsize,
        step_frees: AtomicUsize,
        last_info: std::sync::Mutex<String>,
    }

    fn state(user_data: *mut c_void) -> &'static State {
        // SAFETY: every test passes a pointer to a `State` that outlives the launch.
        unsafe { &*user_data.cast::<State>() }
    }

    unsafe extern "C" fn applies_to(_user_data: *mut c_void, traits_json: *const c_char) -> bool {
        // SAFETY: the library passes a NUL-terminated string.
        let traits = unsafe { CStr::from_ptr(traits_json) }.to_str().unwrap();
        traits.contains("\"applet\"")
    }

    unsafe extern "C" fn run_step(
        user_data: *mut c_void,
        ctx: *mut CobletStepCtx,
        info_json: *const c_char,
    ) -> bool {
        let state = state(user_data);
        state.step_runs.fetch_add(1, Ordering::SeqCst);
        // SAFETY: the library passes a NUL-terminated string.
        let info = unsafe { CStr::from_ptr(info_json) }.to_str().unwrap();
        info.clone_into(&mut state.last_info.lock().unwrap());
        if state.fail {
            let message = CString::new("proxy did not start").unwrap();
            // SAFETY: the context handle of this call.
            unsafe { coblet_step_ctx_fail(ctx, message.as_ptr()) };
            return false;
        }
        true
    }

    unsafe extern "C" fn free_step(user_data: *mut c_void) {
        state(user_data).step_frees.fetch_add(1, Ordering::SeqCst);
    }

    /// Sets one placeholder and tries one coblet fills itself.
    ///
    /// # Safety
    ///
    /// `edit` must be the handle of the running callback.
    unsafe fn set_placeholders(edit: *mut CobletProfileEdit) {
        let null = std::ptr::null_mut();
        // SAFETY: forwarded from the caller's guarantee; the strings are static.
        unsafe {
            assert_eq!(
                coblet_edit_set_placeholder(edit, c"acme_user".as_ptr(), c"Steve".as_ptr(), null),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_edit_set_placeholder(
                    edit,
                    c"auth_access_token".as_ptr(),
                    c"x".as_ptr(),
                    null
                ),
                CobletStatus::InvalidArgument
            );
        }
    }

    unsafe extern "C" fn customize(user_data: *mut c_void, edit: *mut CobletProfileEdit) -> bool {
        let state = state(user_data);
        let null = std::ptr::null_mut();
        let text = |value: &str| CString::new(value).unwrap();
        // SAFETY: `edit` is the handle of this call; strings live for each call.
        unsafe {
            if state.fail {
                let message = text("agent jar is missing");
                coblet_edit_fail(edit, message.as_ptr());
                return false;
            }
            if state.auth {
                let token = text("--accessToken");
                assert_eq!(
                    coblet_edit_add_arg(edit, CobletArgKind::Game, token.as_ptr(), true, null),
                    CobletStatus::Ok
                );
                return true;
            }
            let mut json = CobletString::empty();
            assert_eq!(
                coblet_edit_info_json(edit, &raw mut json, null),
                CobletStatus::Ok
            );
            let info = CStr::from_ptr(json.ptr).to_str().unwrap().to_owned();
            crate::context::coblet_string_free(json);
            assert!(
                info.contains("\"main_class\":\"net.minecraft.launchwrapper.Launch\""),
                "{info}"
            );
            assert!(info.contains("\"os\":\"linux\""), "{info}");

            let main = text("acme.Wrapper");
            assert_eq!(
                coblet_edit_set_main_class(edit, main.as_ptr(), null),
                CobletStatus::Ok
            );
            let agent = text("-javaagent:/opt/fix.jar");
            assert_eq!(
                coblet_edit_add_arg(edit, CobletArgKind::Jvm, agent.as_ptr(), true, null),
                CobletStatus::Ok
            );
            let (name, path) = (text("acme:fix:1.0"), text("acme/fix/1.0/fix-1.0.jar"));
            assert_eq!(
                coblet_edit_add_library(
                    edit,
                    name.as_ptr(),
                    path.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    true,
                    null
                ),
                CobletStatus::Ok
            );
            let escape = text("../escape.jar");
            assert_eq!(
                coblet_edit_add_library(
                    edit,
                    name.as_ptr(),
                    escape.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    false,
                    null
                ),
                CobletStatus::InvalidArgument
            );
            let asm = text("org.ow2.asm:asm-all:4.1");
            let mut removed = false;
            assert_eq!(
                coblet_edit_remove_library(edit, asm.as_ptr(), &raw mut removed, null),
                CobletStatus::Ok
            );
            assert!(removed);
            set_placeholders(edit);
            let (env, one) = (text("ACME"), text("1"));
            assert_eq!(
                coblet_edit_set_env(edit, env.as_ptr(), one.as_ptr(), null),
                CobletStatus::Ok
            );
            let (anchor, step_name) = (text("probe"), text("acme-probe"));
            let step = CobletStep {
                name: step_name.as_ptr(),
                user_data,
                run: Some(run_step),
                free: Some(free_step),
            };
            assert_eq!(
                coblet_edit_add_step(edit, anchor.as_ptr(), false, step, null),
                CobletStatus::Ok
            );
        }
        true
    }

    unsafe extern "C" fn free_customizer(user_data: *mut c_void) {
        state(user_data).frees.fetch_add(1, Ordering::SeqCst);
    }

    fn table(state: &State, id: &CStr) -> CobletCustomizer {
        CobletCustomizer {
            id: id.as_ptr(),
            display_name: std::ptr::null(),
            user_data: std::ptr::from_ref(state).cast_mut().cast(),
            applies_to: Some(applies_to),
            customize: Some(customize),
            free: Some(free_customizer),
        }
    }

    fn platform() -> Platform {
        Platform::new(OsFamily::Linux, Arch::X86_64)
    }

    fn profile() -> EffectiveLaunchProfile {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/mojang/c0.30_01c.json");
        let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
        ProfileEngine::builtin()
            .resolve(&resolved, &platform())
            .unwrap()
    }

    fn adapter(state: &State) -> ForeignCustomizer {
        let id = CString::new("acme:fix").unwrap();
        // SAFETY: the table is valid and `state` outlives the adapter.
        unsafe { ForeignCustomizer::new(&table(state, &id)) }.unwrap()
    }

    fn customize_with(
        customizer: &ForeignCustomizer,
        profile: &mut EffectiveLaunchProfile,
    ) -> Result<CustomizeCtx, LaunchError> {
        let ctx = CustomizeCtx::new(
            platform(),
            GameLayout::new("/data", "/game"),
            profile.traits.clone(),
        );
        coblet::launch::LaunchCustomizer::customize(customizer, profile, &ctx)?;
        Ok(ctx)
    }

    #[test]
    fn edits_reach_the_profile_and_free_runs_once() {
        let state = State::default();
        let customizer = adapter(&state);
        let mut profile = profile();
        assert!(coblet::launch::LaunchCustomizer::applies_to(
            &customizer,
            &profile.traits
        ));
        let ctx = customize_with(&customizer, &mut profile).unwrap();
        assert_eq!(profile.main_class, "acme.Wrapper");
        assert_eq!(profile.jvm_args[0].value, "-javaagent:/opt/fix.jar");
        assert_eq!(profile.libraries[0].name.to_string(), "acme:fix:1.0");
        assert_eq!(ctx.extra_steps(), ["acme-probe"]);
        let shown = format!("{ctx:?}");
        assert!(shown.contains("placeholders: [\"acme_user\"]"), "{shown}");
        drop(ctx);
        assert_eq!(state.step_frees.load(Ordering::SeqCst), 1);
        assert_eq!(state.frees.load(Ordering::SeqCst), 0);
        drop(customizer);
        assert_eq!(state.frees.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_false_return_becomes_a_launch_error() {
        let state = State {
            fail: true,
            ..State::default()
        };
        let customizer = adapter(&state);
        let mut profile = profile();
        let before = profile.clone();
        let error = customize_with(&customizer, &mut profile).unwrap_err();
        assert_eq!(
            error.to_string(),
            "step `acme:fix` failed: agent jar is missing"
        );
        assert_eq!(profile, before);
    }

    #[test]
    fn rejected_tables_are_still_freed() {
        let state = State::default();
        let good = CString::new("acme:fix").unwrap();
        let bad = CString::new("No Namespace").unwrap();
        let root = CString::new("/tmp/coblet-ffi-customizers").unwrap();
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
            let status = coblet_builder_add_customizer(builder, table(&state, &good), &raw mut err);
            assert_eq!(status, CobletStatus::Ok);
            let status = coblet_builder_add_customizer(builder, table(&state, &bad), &raw mut err);
            assert_eq!(status, CobletStatus::InvalidArgument);
            let message = CStr::from_ptr((*err).message.ptr).to_str().unwrap();
            assert!(message.starts_with("customizer id"), "{message}");
            crate::context::coblet_error_free(err);
            assert_eq!(state.frees.load(Ordering::SeqCst), 1);
            coblet_builder_free(builder);
        }
        assert_eq!(state.frees.load(Ordering::SeqCst), 2);
    }

    struct Offline;

    impl Transport for Offline {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(std::future::pending())
        }
    }

    struct Anchor;

    impl coblet::launch::LaunchStep for Anchor {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn run<'a>(&'a self, _cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn launch(state: &State, grant: bool) -> Result<LaunchContext, LaunchError> {
        let mut launcher = Launcher::empty().with_step(Arc::new(Anchor));
        launcher
            .register_customizer(Arc::new(adapter(state)))
            .unwrap();
        if grant {
            launcher
                .allow_auth_changes(ExtensionId::new("acme:fix").unwrap())
                .unwrap();
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = std::env::temp_dir().join(format!("coblet-ffi-{}", std::process::id()));
        let storage = Arc::new(TokioStorage::new());
        let transport: Arc<dyn Transport> = Arc::new(Offline);
        let meta = MetaCache::new(Arc::clone(&transport), storage.clone(), dir.join("meta"));
        let services = LaunchServices::new(
            storage.clone(),
            Arc::new(TokioSpawner::new()),
            Downloader::new(transport, storage, DownloaderConfig::default()),
            Arc::new(CatalogContext::new(meta, CachePolicy::PreferCache)),
            OsFamily::Linux,
        );
        let request = LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
            .with_customizer(ExtensionId::new("acme:fix").unwrap());
        let mut cx = LaunchContext::new(
            services,
            profile(),
            GameLayout::new("/data", "/game"),
            request,
        );
        runtime.block_on(launcher.run(&mut cx))?;
        Ok(cx)
    }

    #[test]
    fn c_steps_run_in_the_launch() {
        let state = State::default();
        let cx = launch(&state, false).unwrap();
        assert_eq!(state.step_runs.load(Ordering::SeqCst), 1);
        let info = state.last_info.lock().unwrap().clone();
        assert!(info.contains("\"game_dir\":\"/game\""), "{info}");
        assert!(info.contains("\"java\":null"), "{info}");
        assert!(
            cx.profile
                .explain()
                .contains("[customize] acme:fix: env ACME set to \"1\"")
        );
        assert_eq!(state.step_frees.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn c_customizers_need_a_grant_to_change_auth_arguments() {
        let state = State {
            auth: true,
            ..State::default()
        };
        let error = launch(&state, false).unwrap_err();
        assert!(
            matches!(
                &error,
                LaunchError::AuthArgument { customizer, argument }
                    if customizer.as_str() == "acme:fix" && argument == "--accessToken"
            ),
            "{error}"
        );
        let cx = launch(&state, true).unwrap();
        assert!(
            cx.profile
                .explain()
                .contains("[customize] acme:fix: game `--accessToken` added")
        );
    }

    /// Calls `coblet_builder_allow_auth_changes` and returns the status with
    /// the error message, if any.
    fn allow(
        builder: *mut crate::builder::CobletBuilder,
        id: &str,
    ) -> (CobletStatus, Option<String>) {
        let id = CString::new(id).unwrap();
        let mut err = std::ptr::null_mut();
        // SAFETY: a live builder, a valid string and out-pointer; the error is freed once.
        unsafe {
            let status = coblet_builder_allow_auth_changes(builder, id.as_ptr(), &raw mut err);
            let message = (!err.is_null()).then(|| {
                let text = CStr::from_ptr((*err).message.ptr)
                    .to_str()
                    .unwrap()
                    .to_owned();
                crate::context::coblet_error_free(err);
                text
            });
            (status, message)
        }
    }

    #[test]
    fn auth_grants_name_added_customizers_and_reach_the_launcher() {
        let state = State::default();
        let id = CString::new("acme:fix").unwrap();
        let root = CString::new("/tmp/coblet-ffi-grants").unwrap();
        let agent = CString::new("coblet-ffi-test/1.0").unwrap();
        for grant in [false, true] {
            let mut builder = std::ptr::null_mut();
            let mut coblet = std::ptr::null_mut();
            let null = std::ptr::null_mut();
            // SAFETY: valid strings, tables and out-pointers; the handles are freed once.
            unsafe {
                let status =
                    coblet_builder_new(root.as_ptr(), agent.as_ptr(), &raw mut builder, null);
                assert_eq!(status, CobletStatus::Ok);
                let (status, message) = allow(builder, "acme:fix");
                assert_eq!(status, CobletStatus::InvalidArgument);
                assert_eq!(
                    message.as_deref(),
                    Some("customizer id `acme:fix` was not added; add the customizer first")
                );
                let status = coblet_builder_add_customizer(builder, table(&state, &id), null);
                assert_eq!(status, CobletStatus::Ok);
                let (status, message) = allow(builder, "No Namespace");
                assert_eq!(status, CobletStatus::InvalidArgument);
                assert!(message.unwrap().starts_with("customizer id"));
                if grant {
                    assert_eq!(allow(builder, "acme:fix"), (CobletStatus::Ok, None));
                    assert_eq!(allow(builder, "acme:fix"), (CobletStatus::Ok, None));
                }
                let status = coblet_builder_build(builder, &raw mut coblet, null);
                assert_eq!(status, CobletStatus::Ok);
                let allowed = (*coblet)
                    .inner
                    .launcher()
                    .allows_auth_changes(&ExtensionId::new("acme:fix").unwrap());
                assert_eq!(allowed, grant);
                crate::context::coblet_free(coblet);
            }
        }
        assert_eq!(state.frees.load(Ordering::SeqCst), 2);
    }
}
