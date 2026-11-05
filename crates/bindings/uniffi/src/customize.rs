use std::path::{Component, Path};
use std::sync::{Arc, Mutex, PoisonError};

use coblet::launch::{CustomizeCtx, LaunchContext, LaunchError};
use coblet::mojang::VersionTraits;
use coblet::platform::{
    EffectiveLaunchProfile, EffectiveLibrary, EnvValue, FileSource, LibraryFile, ProfileArg,
};
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::{ExtensionId, MavenCoordinate, Sha1Digest};

use crate::{CobletError, ErrorKind};

/// Launch customizer written in Swift, Kotlin or Python.
///
/// It edits the launch of versions it applies to through a
/// [`ProfileEditor`]; coblet records every change in the launch report.
/// Register it with `CobletSdk.with_customizers` and choose it per launch
/// with `LaunchOptions.customizers`.
#[uniffi::export(foreign)]
pub trait LaunchCustomizer: Send + Sync {
    /// Returns the ID, such as `acme:legacy-fix`; read once at registration.
    fn id(&self) -> String;

    /// Returns the name shown to users; read once at registration.
    fn display_name(&self) -> String;

    /// Reports whether the customizer handles a version with these traits,
    /// such as `applet` or `legacy-online-services`.
    fn applies_to(&self, traits: Vec<String>) -> bool;

    /// Edits one launch; an error stops it.
    ///
    /// # Errors
    ///
    /// The error reaches the caller of the launch as a launch failure.
    fn customize(&self, editor: Arc<ProfileEditor>) -> Result<(), CobletError>;
}

/// Extra launch step written in Swift, Kotlin or Python.
///
/// It runs on a worker thread at the place a customizer registered it.
#[uniffi::export(foreign)]
pub trait LaunchStep: Send + Sync {
    /// Returns the step name, such as `acme-start-proxy`.
    fn name(&self) -> String;

    /// Runs the step; an error stops the launch.
    ///
    /// # Errors
    ///
    /// The error reaches the caller of the launch as a launch failure.
    fn run(&self, info: StepInfo) -> Result<(), CobletError>;
}

/// What an extra step can see of the launch so far.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StepInfo {
    /// Directory for shared game files.
    pub data_root: String,
    /// Working directory of the game.
    pub game_dir: String,
    /// Java executable, once `select-java` ran.
    pub java: Option<String>,
    /// Natives directory, once `prepare-natives` ran.
    pub natives_dir: Option<String>,
}

/// Library added by a customizer.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LibrarySpec {
    /// Maven coordinate, such as `com.example:fix:1.0`.
    pub name: String,
    /// Path inside the libraries directory, with `/` separators.
    pub path: String,
    /// Download address; without one the file must already be there.
    #[uniffi(default = None)]
    pub url: Option<String>,
    /// SHA-1 of the file in hex, checked when given.
    #[uniffi(default = None)]
    pub sha1: Option<String>,
    /// Size in bytes, checked when given.
    #[uniffi(default = None)]
    pub size: Option<u64>,
    /// Puts the library first on the classpath instead of last.
    #[uniffi(default = false)]
    pub first: bool,
}

#[derive(Clone, Copy)]
enum Placement {
    Before,
    After,
}

struct PendingStep {
    anchor: String,
    placement: Placement,
    step: Arc<dyn LaunchStep>,
}

struct Edit {
    profile: EffectiveLaunchProfile,
    steps: Vec<PendingStep>,
}

/// Launch profile handed to [`LaunchCustomizer::customize`].
///
/// It is valid only during that call; afterwards every method fails with
/// `InvalidState`.
#[derive(uniffi::Object)]
pub struct ProfileEditor {
    edit: Mutex<Option<Edit>>,
    os: String,
    arch: String,
    data_root: String,
    game_dir: String,
}

impl std::fmt::Debug for ProfileEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileEditor")
            .field("os", &self.os)
            .field("arch", &self.arch)
            .finish_non_exhaustive()
    }
}

fn invalid(message: impl Into<String>) -> CobletError {
    CobletError::new(ErrorKind::InvalidArgument, message)
}

fn arg_list(args: &[ProfileArg]) -> Vec<String> {
    args.iter().map(|arg| arg.value.clone()).collect()
}

fn add_arg(args: &mut Vec<ProfileArg>, value: String, first: bool) {
    let arg = ProfileArg::always(value);
    if first {
        args.insert(0, arg);
    } else {
        args.push(arg);
    }
}

fn remove_arg(args: &mut Vec<ProfileArg>, value: &str) -> bool {
    let before = args.len();
    args.retain(|arg| arg.value != value);
    args.len() != before
}

fn library_path(path: &str) -> Result<String, CobletError> {
    let normal = !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if normal {
        Ok(path.to_owned())
    } else {
        Err(invalid(format!(
            "library path `{path}` must be relative, use `/` and stay inside the libraries directory"
        )))
    }
}

fn library(spec: LibrarySpec) -> Result<EffectiveLibrary, CobletError> {
    let name: MavenCoordinate = spec
        .name
        .parse()
        .map_err(|error| invalid(format!("library name: {error}")))?;
    let sha1 = spec
        .sha1
        .as_deref()
        .map(Sha1Digest::from_hex)
        .transpose()
        .map_err(|error| invalid(format!("library sha1: {error}")))?;
    let source = match spec.url {
        Some(url) => FileSource::Download {
            url,
            sha1,
            size: spec.size,
        },
        None => FileSource::Existing,
    };
    Ok(EffectiveLibrary::new(
        name,
        Some(LibraryFile::new(library_path(&spec.path)?, source)),
    ))
}

impl ProfileEditor {
    fn new(profile: EffectiveLaunchProfile, ctx: &CustomizeCtx) -> Self {
        Self {
            os: ctx.platform.os().as_str().to_owned(),
            arch: ctx.platform.arch().as_str().to_owned(),
            data_root: ctx.layout.data_root().to_string_lossy().into_owned(),
            game_dir: ctx.layout.game_directory().to_string_lossy().into_owned(),
            edit: Mutex::new(Some(Edit {
                profile,
                steps: Vec::new(),
            })),
        }
    }

    fn with<T>(&self, body: impl FnOnce(&mut Edit) -> T) -> Result<T, CobletError> {
        let mut guard = self.edit.lock().unwrap_or_else(PoisonError::into_inner);
        let edit = guard.as_mut().ok_or_else(|| {
            CobletError::new(
                ErrorKind::InvalidState,
                "the profile editor is used after customize returned",
            )
        })?;
        Ok(body(edit))
    }

    fn close(&self) -> Option<Edit> {
        self.edit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

#[uniffi::export]
impl ProfileEditor {
    /// Returns the game version ID.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn game_version(&self) -> Result<String, CobletError> {
        self.with(|edit| edit.profile.game_version.as_str().to_owned())
    }

    /// Returns the operating system, such as `macos`.
    pub fn os(&self) -> String {
        self.os.clone()
    }

    /// Returns the architecture, such as `aarch64`.
    pub fn arch(&self) -> String {
        self.arch.clone()
    }

    /// Returns the directory for shared game files.
    pub fn data_root(&self) -> String {
        self.data_root.clone()
    }

    /// Returns the working directory of the game.
    pub fn game_dir(&self) -> String {
        self.game_dir.clone()
    }

    /// Returns the version traits, such as `applet`.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn traits(&self) -> Result<Vec<String>, CobletError> {
        self.with(|edit| trait_names(&edit.profile.traits))
    }

    /// Returns the Java class with the `main` method.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn main_class(&self) -> Result<String, CobletError> {
        self.with(|edit| edit.profile.main_class.clone())
    }

    /// Replaces the main class.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn set_main_class(&self, main_class: String) -> Result<(), CobletError> {
        self.with(|edit| edit.profile.main_class = main_class)
    }

    /// Returns the JVM argument templates.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn jvm_args(&self) -> Result<Vec<String>, CobletError> {
        self.with(|edit| arg_list(&edit.profile.jvm_args))
    }

    /// Adds a JVM argument, such as `-javaagent:/path/fix.jar`, last or first.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn add_jvm_arg(&self, value: String, first: bool) -> Result<(), CobletError> {
        self.with(|edit| add_arg(&mut edit.profile.jvm_args, value, first))
    }

    /// Removes every JVM argument equal to `value`; returns whether one was found.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn remove_jvm_arg(&self, value: &str) -> Result<bool, CobletError> {
        self.with(|edit| remove_arg(&mut edit.profile.jvm_args, value))
    }

    /// Returns the game argument templates.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn game_args(&self) -> Result<Vec<String>, CobletError> {
        self.with(|edit| arg_list(&edit.profile.game_args))
    }

    /// Adds a game argument, last or first.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn add_game_arg(&self, value: String, first: bool) -> Result<(), CobletError> {
        self.with(|edit| add_arg(&mut edit.profile.game_args, value, first))
    }

    /// Removes every game argument equal to `value`; returns whether one was found.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn remove_game_arg(&self, value: &str) -> Result<bool, CobletError> {
        self.with(|edit| remove_arg(&mut edit.profile.game_args, value))
    }

    /// Returns the libraries in classpath order, natives with their classifier.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn libraries(&self) -> Result<Vec<String>, CobletError> {
        self.with(|edit| {
            edit.profile
                .libraries
                .iter()
                .filter(|library| library.is_active())
                .map(EffectiveLibrary::native_key)
                .collect()
        })
    }

    /// Adds a classpath library.
    ///
    /// # Errors
    ///
    /// Fails for a malformed name, path or SHA-1, and after `customize` returned.
    pub fn add_library(&self, spec: LibrarySpec) -> Result<(), CobletError> {
        let first = spec.first;
        let library = library(spec)?;
        self.with(|edit| {
            if first {
                edit.profile.libraries.insert(0, library);
            } else {
                edit.profile.libraries.push(library);
            }
        })
    }

    /// Removes the libraries listed as `name` by [`ProfileEditor::libraries`];
    /// returns whether one was found.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn remove_library(&self, name: &str) -> Result<bool, CobletError> {
        self.with(|edit| {
            let before = edit.profile.libraries.len();
            edit.profile
                .libraries
                .retain(|library| library.native_key() != name);
            edit.profile.libraries.len() != before
        })
    }

    /// Sets an environment variable for the game.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn set_env(&self, name: String, value: String) -> Result<(), CobletError> {
        self.with(|edit| {
            edit.profile.env.insert(name, EnvValue::Set(value));
        })
    }

    /// Removes an environment variable set by the profile; returns whether it was set.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn remove_env(&self, name: &str) -> Result<bool, CobletError> {
        self.with(|edit| edit.profile.env.remove(name).is_some())
    }

    /// Adds a directory to the native library search path.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn add_native_path(&self, path: String) -> Result<(), CobletError> {
        self.with(|edit| {
            let path = path.into();
            if !edit.profile.native_paths.contains(&path) {
                edit.profile.native_paths.push(path);
            }
        })
    }

    /// Returns the Java major version the game needs.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn java_major(&self) -> Result<u32, CobletError> {
        self.with(|edit| edit.profile.java.major)
    }

    /// Sets the Java major version the game needs.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned.
    pub fn set_java_major(&self, major: u32) -> Result<(), CobletError> {
        self.with(|edit| edit.profile.java.major = major)
    }

    /// Runs `step` right before the launch step named `anchor`, such as `pre-launch`.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned. An unknown anchor fails the launch.
    pub fn add_step_before(
        &self,
        anchor: String,
        step: Arc<dyn LaunchStep>,
    ) -> Result<(), CobletError> {
        self.with(|edit| {
            edit.steps.push(PendingStep {
                anchor,
                placement: Placement::Before,
                step,
            });
        })
    }

    /// Runs `step` right after the launch step named `anchor`.
    ///
    /// # Errors
    ///
    /// Fails after `customize` returned. An unknown anchor fails the launch.
    pub fn add_step_after(
        &self,
        anchor: String,
        step: Arc<dyn LaunchStep>,
    ) -> Result<(), CobletError> {
        self.with(|edit| {
            edit.steps.push(PendingStep {
                anchor,
                placement: Placement::After,
                step,
            });
        })
    }
}

fn trait_names(traits: &VersionTraits) -> Vec<String> {
    traits
        .iter()
        .map(|value| value.as_str().to_owned())
        .collect()
}

/// Registers a foreign customizer with the Rust launcher.
pub(crate) struct ForeignCustomizer {
    id: ExtensionId,
    display_name: String,
    inner: Arc<dyn LaunchCustomizer>,
}

impl ForeignCustomizer {
    pub(crate) fn new(inner: Arc<dyn LaunchCustomizer>) -> Result<Self, CobletError> {
        let id = ExtensionId::new(inner.id())
            .map_err(|error| invalid(format!("customizer id: {error}")))?;
        Ok(Self {
            id,
            display_name: inner.display_name(),
            inner,
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

impl coblet::launch::LaunchCustomizer for ForeignCustomizer {
    fn applies_to(&self, traits: &VersionTraits) -> bool {
        self.inner.applies_to(trait_names(traits))
    }

    fn customize(
        &self,
        profile: &mut EffectiveLaunchProfile,
        ctx: &CustomizeCtx,
    ) -> Result<(), LaunchError> {
        let editor = Arc::new(ProfileEditor::new(profile.clone(), ctx));
        let result = self.inner.customize(Arc::clone(&editor));
        let edit = editor.close();
        result.map_err(|error| LaunchError::Step {
            step: self.id.to_string(),
            message: error.to_string(),
        })?;
        let Some(edit) = edit else {
            return Ok(());
        };
        *profile = edit.profile;
        for pending in edit.steps {
            let step = Arc::new(ForeignStep {
                name: pending.step.name(),
                inner: pending.step,
            });
            match pending.placement {
                Placement::Before => ctx.add_step_before(pending.anchor, step),
                Placement::After => ctx.add_step_after(pending.anchor, step),
            }
        }
        Ok(())
    }
}

/// Runs a foreign step as a Rust launch step.
struct ForeignStep {
    name: String,
    inner: Arc<dyn LaunchStep>,
}

impl coblet::launch::LaunchStep for ForeignStep {
    fn name(&self) -> &str {
        &self.name
    }

    fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
        let info = StepInfo {
            data_root: cx.layout.data_root().to_string_lossy().into_owned(),
            game_dir: cx.layout.game_directory().to_string_lossy().into_owned(),
            java: cx
                .java
                .as_ref()
                .map(|java| java.executable.to_string_lossy().into_owned()),
            natives_dir: cx
                .natives_directory
                .as_ref()
                .map(|dir| dir.to_string_lossy().into_owned()),
        };
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let failed = |message: String| LaunchError::Step {
                step: self.name.clone(),
                message,
            };
            tokio::task::spawn_blocking(move || inner.run(info))
                .await
                .map_err(|error| failed(format!("the step stopped: {error}")))?
                .map_err(|error| failed(error.to_string()))
        })
    }
}

#[cfg(test)]
#[path = "customize_tests.rs"]
mod tests;
