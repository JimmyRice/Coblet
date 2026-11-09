use std::path::{Component, Path};
use std::sync::{Arc, Mutex, PoisonError};

use coblet::launch::{CustomizeCtx, LaunchError};
use coblet::mojang::VersionTraits;
use coblet::platform::{
    EffectiveLaunchProfile, EffectiveLibrary, EnvValue, FileSource, LibraryFile, ProfileArg,
};
use coblet::runtime::Extension;
use coblet::types::{ExtensionId, MavenCoordinate, Sha1Digest};

use crate::steps::ForeignStep;
use crate::{CobletError, ErrorKind, LaunchStep};

/// Launch customizer written in Swift, Kotlin or Python.
///
/// It edits the launch of versions it applies to through a
/// [`ProfileEditor`]; coblet records every change in the launch report.
/// Register it with `CobletBuilder.add_customizer` and choose it per launch
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
    placeholders: Vec<(String, String)>,
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

pub(crate) fn library(spec: LibrarySpec) -> Result<EffectiveLibrary, CobletError> {
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
                placeholders: Vec::new(),
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

    /// Sets `${name}` to `value` in this launch, replacing the value of
    /// `LaunchOptions.placeholders` or of an earlier customizer.
    ///
    /// # Errors
    ///
    /// Fails for a name coblet fills itself, such as `auth_access_token`,
    /// an empty name or one with `$`, `{` or `}`, and after `customize`
    /// returned.
    pub fn set_placeholder(&self, name: String, value: String) -> Result<(), CobletError> {
        crate::placeholders::placeholder_name(&name)?;
        self.with(|edit| edit.placeholders.push((name, value)))
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

pub(crate) fn trait_names(traits: &VersionTraits) -> Vec<String> {
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
        for (name, value) in edit.placeholders {
            ctx.set_placeholder(name, value)?;
        }
        for pending in edit.steps {
            let step = Arc::new(ForeignStep::new(pending.step));
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
    use std::path::Path;
    use std::sync::{Arc, Mutex};

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

    use super::{ForeignCustomizer, LaunchCustomizer, LibrarySpec, ProfileEditor};
    use crate::{CobletError, ErrorKind};
    use crate::{LaunchStep, StepContext};

    fn platform() -> Platform {
        Platform::new(OsFamily::Linux, Arch::X86_64)
    }

    fn profile(version: &str) -> EffectiveLaunchProfile {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../../fixtures/mojang/{version}.json"));
        let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let traits = TraitTable::builtin().for_version(&raw);
        let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
        ProfileEngine::builtin()
            .resolve(&resolved, &platform())
            .unwrap()
    }

    /// Records what the step saw.
    #[derive(Default)]
    struct Probe {
        seen: Mutex<Vec<(String, Option<String>)>>,
    }

    impl LaunchStep for Probe {
        fn name(&self) -> String {
            "acme-probe".to_owned()
        }

        fn run(&self, cx: Arc<StepContext>) -> Result<(), CobletError> {
            self.seen.lock().unwrap().push((cx.game_dir()?, cx.java()?));
            Ok(())
        }
    }

    /// Behaves like a customizer written in Python.
    struct Script {
        fail: bool,
        auth: bool,
        path: &'static str,
        probe: Arc<Probe>,
        kept: Mutex<Option<Arc<ProfileEditor>>>,
    }

    impl Script {
        fn new() -> Self {
            Self {
                fail: false,
                auth: false,
                path: "acme/fix/1.0/fix-1.0.jar",
                probe: Arc::new(Probe::default()),
                kept: Mutex::new(None),
            }
        }
    }

    impl LaunchCustomizer for Script {
        fn id(&self) -> String {
            "acme:fix".to_owned()
        }

        fn display_name(&self) -> String {
            "Fix".to_owned()
        }

        fn applies_to(&self, traits: Vec<String>) -> bool {
            traits.iter().any(|value| value == "applet")
        }

        fn customize(&self, editor: Arc<ProfileEditor>) -> Result<(), CobletError> {
            if self.fail {
                return Err(CobletError::new(ErrorKind::Other, "agent jar is missing"));
            }
            if self.auth {
                return editor.add_game_arg("--accessToken".to_owned(), false);
            }
            assert_eq!(editor.os(), "linux");
            assert!(editor.traits()?.contains(&"applet".to_owned()));
            assert_eq!(editor.main_class()?, "net.minecraft.launchwrapper.Launch");
            editor.set_main_class("acme.Wrapper".to_owned())?;
            editor.add_jvm_arg("-javaagent:/opt/fix.jar".to_owned(), true)?;
            editor.add_library(LibrarySpec {
                name: "acme:fix:1.0".to_owned(),
                path: self.path.to_owned(),
                url: None,
                sha1: None,
                size: None,
                first: true,
            })?;
            assert!(editor.remove_library("org.ow2.asm:asm-all:4.1")?);
            editor.set_env("ACME".to_owned(), "1".to_owned())?;
            editor.add_step_before("probe".to_owned(), self.probe.clone())?;
            *self.kept.lock().unwrap() = Some(editor);
            Ok(())
        }
    }

    fn ctx() -> CustomizeCtx {
        let profile = profile("c0.30_01c");
        CustomizeCtx::new(
            platform(),
            GameLayout::new("/data", "/game"),
            profile.traits,
        )
    }

    fn adapter(script: Script) -> ForeignCustomizer {
        ForeignCustomizer::new(Arc::new(script)).unwrap()
    }

    fn run(
        customizer: &ForeignCustomizer,
        profile: &mut EffectiveLaunchProfile,
        ctx: &CustomizeCtx,
    ) -> Result<(), LaunchError> {
        coblet::launch::LaunchCustomizer::customize(customizer, profile, ctx)
    }

    #[test]
    fn edits_reach_the_profile_and_the_editor_closes() {
        let script = Arc::new(Script::new());
        let customizer = ForeignCustomizer::new(script.clone()).unwrap();
        let ctx = ctx();
        let mut profile = profile("c0.30_01c");
        assert!(coblet::launch::LaunchCustomizer::applies_to(
            &customizer,
            &profile.traits
        ));
        run(&customizer, &mut profile, &ctx).unwrap();
        assert_eq!(profile.main_class, "acme.Wrapper");
        assert_eq!(profile.jvm_args[0].value, "-javaagent:/opt/fix.jar");
        assert_eq!(profile.libraries[0].name.to_string(), "acme:fix:1.0");
        assert!(
            profile
                .libraries
                .iter()
                .all(|library| library.name.to_string() != "org.ow2.asm:asm-all:4.1")
        );
        assert_eq!(ctx.extra_steps(), ["acme-probe"]);

        let kept = script.kept.lock().unwrap().clone().unwrap();
        let error = kept.main_class().unwrap_err();
        assert!(
            matches!(
                &error,
                CobletError::Failed {
                    kind: ErrorKind::InvalidState,
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_failure_becomes_a_launch_error() {
        let customizer = adapter(Script {
            fail: true,
            ..Script::new()
        });
        let mut profile = profile("c0.30_01c");
        let before = profile.clone();
        let error = run(&customizer, &mut profile, &ctx()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "step `acme:fix` failed: agent jar is missing"
        );
        assert_eq!(profile, before);
    }

    #[test]
    fn library_paths_must_stay_inside_the_libraries_directory() {
        for path in ["../escape.jar", "/abs/fix.jar", "a\\b.jar", ""] {
            let customizer = adapter(Script {
                path,
                ..Script::new()
            });
            let error = run(&customizer, &mut profile("c0.30_01c"), &ctx()).unwrap_err();
            assert!(
                error.to_string().contains("library path"),
                "{path}: {error}"
            );
        }
    }

    struct BadId;

    impl LaunchCustomizer for BadId {
        fn id(&self) -> String {
            "No Namespace".to_owned()
        }

        fn display_name(&self) -> String {
            String::new()
        }

        fn applies_to(&self, _traits: Vec<String>) -> bool {
            false
        }

        fn customize(&self, _editor: Arc<ProfileEditor>) -> Result<(), CobletError> {
            Ok(())
        }
    }

    #[test]
    fn ids_are_checked_at_registration() {
        let error = ForeignCustomizer::new(Arc::new(BadId)).err().unwrap();
        assert!(error.to_string().starts_with("customizer id:"), "{error}");
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

    async fn launch(script: Arc<Script>, grant: bool) -> Result<LaunchContext, LaunchError> {
        let mut launcher = Launcher::empty().with_step(Arc::new(Anchor));
        launcher
            .register_customizer(Arc::new(ForeignCustomizer::new(script).unwrap()))
            .unwrap();
        if grant {
            launcher
                .allow_auth_changes(ExtensionId::new("acme:fix").unwrap())
                .unwrap();
        }

        let dir = std::env::temp_dir().join(format!("coblet-uniffi-{}", std::process::id()));
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
            profile("c0.30_01c"),
            GameLayout::new("/data", "/game"),
            request,
        );
        launcher.run(&mut cx).await?;
        Ok(cx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn foreign_steps_run_in_the_launch() {
        let script = Arc::new(Script::new());
        let probe = Arc::clone(&script.probe);
        let cx = launch(script, false).await.unwrap();
        let seen = probe.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], ("/game".to_owned(), None));
        assert!(
            cx.profile
                .explain()
                .contains("[customize] acme:fix: env ACME set to \"1\"")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn foreign_customizers_need_a_grant_to_change_auth_arguments() {
        let script = || {
            Arc::new(Script {
                auth: true,
                ..Script::new()
            })
        };
        let error = launch(script(), false).await.unwrap_err();
        assert!(
            matches!(
                &error,
                LaunchError::AuthArgument { customizer, argument }
                    if customizer.as_str() == "acme:fix" && argument == "--accessToken"
            ),
            "{error}"
        );
        let cx = launch(script(), true).await.unwrap();
        assert!(
            cx.profile
                .explain()
                .contains("[customize] acme:fix: game `--accessToken` added")
        );
    }
}
