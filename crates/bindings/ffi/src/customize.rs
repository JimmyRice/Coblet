use std::ffi::{CString, c_char, c_void};
use std::path::{Component, Path};
use std::sync::Arc;

use coblet::launch::{CustomizeCtx, LaunchContext, LaunchError};
use coblet::mojang::{VersionTrait, VersionTraits};
use coblet::platform::{
    EffectiveLaunchProfile, EffectiveLibrary, EnvValue, FileSource, LibraryFile, ProfileArg,
};
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::{ExtensionId, MavenCoordinate, Sha1Digest};

use crate::guard::{guard, put};
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

/// Callback table of an extra launch step written in C.
///
/// The library owns `user_data` from the call that receives the table,
/// even when that call fails, and calls `free` once after the launch.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletStep {
    /// Step name such as `acme-start-proxy`.
    pub name: *const c_char,
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Runs the step on a worker thread. `info_json` is
    /// `{"schema":1,"data_root","game_dir","java","natives_dir"}`, with
    /// `null` for values not known yet. Returning false stops the launch;
    /// call `coblet_step_fail` first to give the reason.
    pub run: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            info_json: *const c_char,
            failure: *mut CobletStepFailure,
        ) -> bool,
    >,
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

/// Reason a step failed, given with `coblet_step_fail`.
#[derive(Debug, Default)]
pub struct CobletStepFailure {
    message: Option<String>,
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
    let owned = Owned {
        user_data: step.user_data,
        free: step.free,
    };
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(out_err, || {
            let edit = edit_mut(edit)?;
            let anchor = text(anchor, "anchor")?.to_owned();
            let name = text(step.name, "step.name")?.to_owned();
            let run = step
                .run
                .ok_or_else(|| Failure::argument("step.run", "is NULL"))?;
            edit.steps.push(PendingStep {
                anchor,
                placement: if after {
                    Placement::After
                } else {
                    Placement::Before
                },
                step: ForeignStep {
                    name,
                    run,
                    owned: Arc::new(owned),
                },
            });
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

/// Gives the reason the running step `run` callback is about to return false.
///
/// # Safety
///
/// `failure` must be the handle passed to the running `run` callback;
/// `message` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_step_fail(
    failure: *mut CobletStepFailure,
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

/// C step run as a Rust launch step.
struct ForeignStep {
    name: String,
    run: unsafe extern "C" fn(*mut c_void, *const c_char, *mut CobletStepFailure) -> bool,
    owned: Arc<Owned>,
}

impl coblet::launch::LaunchStep for ForeignStep {
    fn name(&self) -> &str {
        &self.name
    }

    #[allow(unsafe_code, reason = "calls the C callback")]
    fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
        let info = serde_json::json!({
            "schema": 1,
            "data_root": cx.layout.data_root().to_string_lossy(),
            "game_dir": cx.layout.game_directory().to_string_lossy(),
            "java": cx.java.as_ref().map(|java| java.executable.to_string_lossy().into_owned()),
            "natives_dir": cx
                .natives_directory
                .as_ref()
                .map(|dir| dir.to_string_lossy().into_owned()),
        });
        let json = c_text(&info.to_string());
        let run = self.run;
        let owned = Arc::clone(&self.owned);
        Box::pin(async move {
            let failed = |message: String| LaunchError::Step {
                step: self.name.clone(),
                message,
            };
            let (ok, failure) = tokio::task::spawn_blocking(move || {
                let mut failure = CobletStepFailure::default();
                // SAFETY: the callback and user data are valid until `free`,
                // which `owned` delays; the string and failure live for the call.
                let ok = unsafe { run(owned.user_data, json.as_ptr(), &raw mut failure) };
                (ok, failure)
            })
            .await
            .map_err(|error| failed(format!("the step stopped: {error}")))?;
            if ok {
                Ok(())
            } else {
                Err(failed(
                    failure
                        .message
                        .unwrap_or_else(|| "the step failed".to_owned()),
                ))
            }
        })
    }
}

#[cfg(test)]
#[path = "../tests/customize.rs"]
mod tests;
