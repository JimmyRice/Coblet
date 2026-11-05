use std::path::Path;
use std::sync::{Arc, Mutex};

use coblet::catalog::{CachePolicy, CatalogContext, MetaCache};
use coblet::download::{Downloader, DownloaderConfig};
use coblet::launch::{
    CustomizeCtx, GameLayout, LaunchContext, LaunchError, LaunchRequest, LaunchServices, Launcher,
};
use coblet::launch_command::LaunchAuth;
use coblet::mojang::{RawVersionJson, TraitTable};
use coblet::platform::{EffectiveLaunchProfile, ProfileEngine, ResolvedProfile};
use coblet::runtime::{
    BoxFuture, Request, Response, TokioSpawner, TokioStorage, Transport, TransportError,
};
use coblet::types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

use super::{
    ForeignCustomizer, LaunchCustomizer, LaunchStep, LibrarySpec, ProfileEditor, StepInfo,
};
use crate::{CobletError, ErrorKind};

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
    seen: Mutex<Vec<StepInfo>>,
}

impl LaunchStep for Probe {
    fn name(&self) -> String {
        "acme-probe".to_owned()
    }

    fn run(&self, info: StepInfo) -> Result<(), CobletError> {
        self.seen.lock().unwrap().push(info);
        Ok(())
    }
}

/// Behaves like a customizer written in Python.
struct Script {
    fail: bool,
    path: &'static str,
    probe: Arc<Probe>,
    kept: Mutex<Option<Arc<ProfileEditor>>>,
}

impl Script {
    fn new() -> Self {
        Self {
            fail: false,
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

#[tokio::test(flavor = "multi_thread")]
async fn foreign_steps_run_in_the_launch() {
    let script = Arc::new(Script::new());
    let probe = Arc::clone(&script.probe);
    let mut launcher = Launcher::empty().with_step(Arc::new(Anchor));
    launcher
        .register_customizer(Arc::new(ForeignCustomizer::new(script).unwrap()))
        .unwrap();

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
    launcher.run(&mut cx).await.unwrap();
    let seen = probe.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].game_dir, "/game");
    assert_eq!(seen[0].java, None);
    assert!(
        cx.profile
            .explain()
            .contains("[customize] acme:fix: env ACME set to \"1\"")
    );
}
