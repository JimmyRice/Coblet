use std::path::Path;
use std::sync::Arc;

use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
use coblet_download::{Downloader, DownloaderConfig};
use coblet_launch_command::LaunchAuth;
use coblet_mojang::{RawVersionJson, TraitTable, VersionTrait, VersionTraits};
use coblet_platform::{EffectiveLaunchProfile, ProfileArg, ProfileEngine, ResolvedProfile};
use coblet_runtime::{
    BoxFuture, Extension, Request, Response, TokioSpawner, TokioStorage, Transport, TransportError,
};
use coblet_types::{Arch, ExtensionId, OsFamily, Platform, PlayerName};

use super::Launcher;
use crate::{
    CustomizeCtx, GameLayout, LaunchContext, LaunchCustomizer, LaunchError, LaunchRequest,
    LaunchServices, LaunchStep,
};

struct Offline;

impl Transport for Offline {
    fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(std::future::pending())
    }
}

/// Step that appends its name to the game arguments, to show it ran and where.
struct Mark(&'static str);

impl LaunchStep for Mark {
    fn name(&self) -> &str {
        self.0
    }

    fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            cx.request.game_args.push(self.0.to_owned());
            Ok(())
        })
    }
}

/// Customizer for applet versions: wraps the main class, adds a Java
/// agent and a step before `probe`.
struct Applet {
    id: ExtensionId,
    needs: VersionTrait,
    fail: bool,
    anchor: &'static str,
}

impl Applet {
    fn new(id: &str) -> Self {
        Self {
            id: ExtensionId::new(id).unwrap(),
            needs: VersionTrait::Applet,
            fail: false,
            anchor: "probe",
        }
    }
}

impl Extension for Applet {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &'static str {
        "Applet test"
    }
}

impl LaunchCustomizer for Applet {
    fn applies_to(&self, traits: &VersionTraits) -> bool {
        traits.contains(&self.needs)
    }

    fn customize(
        &self,
        profile: &mut EffectiveLaunchProfile,
        ctx: &CustomizeCtx,
    ) -> Result<(), LaunchError> {
        if self.fail {
            return Err(LaunchError::Step {
                step: self.id.to_string(),
                message: "agent jar is missing".to_owned(),
            });
        }
        assert!(ctx.traits.contains(&VersionTrait::Applet));
        assert_eq!(ctx.platform.os(), &OsFamily::Linux);
        profile.main_class = "acme.AppletWrapper".to_owned();
        profile
            .jvm_args
            .push(ProfileArg::always("-javaagent:/opt/acme/fix.jar"));
        ctx.add_step_before(self.anchor, Arc::new(Mark("extra")));
        Ok(())
    }
}

fn context(root: &Path, version: &str, request: LaunchRequest) -> LaunchContext {
    let storage = Arc::new(TokioStorage::new());
    let transport: Arc<dyn Transport> = Arc::new(Offline);
    let meta = MetaCache::new(Arc::clone(&transport), storage.clone(), root.join("meta"));
    let services = LaunchServices::new(
        storage.clone(),
        Arc::new(TokioSpawner::new()),
        Downloader::new(transport, storage, DownloaderConfig::default()),
        Arc::new(CatalogContext::new(meta, CachePolicy::PreferCache)),
        OsFamily::Linux,
    );
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../../fixtures/mojang/{version}.json"));
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
    let profile = ProfileEngine::builtin()
        .resolve(&resolved, &Platform::new(OsFamily::Linux, Arch::X86_64))
        .unwrap();
    LaunchContext::new(
        services,
        profile,
        GameLayout::new(root.join("data"), root.join("game")),
        request,
    )
}

fn request() -> LaunchRequest {
    LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve").unwrap()))
}

fn chosen(id: &str) -> LaunchRequest {
    request().with_customizer(ExtensionId::new(id).unwrap())
}

fn launcher(customizers: impl IntoIterator<Item = Applet>) -> Launcher {
    let mut launcher = Launcher::empty().with_step(Arc::new(Mark("probe")));
    for customizer in customizers {
        launcher.register_customizer(Arc::new(customizer)).unwrap();
    }
    launcher
}

fn customize_lines(profile: &EffectiveLaunchProfile) -> Vec<String> {
    profile
        .explain()
        .lines()
        .filter(|line| line.starts_with("[customize]"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn a_chosen_customizer_edits_the_profile_and_adds_a_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
    launcher([Applet::new("coblet-test:x")])
        .run(&mut cx)
        .await
        .unwrap();
    assert_eq!(cx.profile.main_class, "acme.AppletWrapper");
    assert_eq!(cx.request.game_args, ["extra", "probe"]);
    assert_eq!(
        customize_lines(&cx.profile),
        [
            "[customize] coblet-test:x: main class set to `acme.AppletWrapper` (was `net.minecraft.launchwrapper.Launch`)",
            "[customize] coblet-test:x: jvm `-javaagent:/opt/acme/fix.jar` added",
        ]
    );
}

#[tokio::test]
async fn customizers_run_only_when_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), "c0.30_01c", request());
    let before = cx.profile.clone();
    launcher([Applet::new("coblet-test:x")])
        .run(&mut cx)
        .await
        .unwrap();
    assert_eq!(cx.profile, before);
    assert_eq!(cx.request.game_args, ["probe"]);
}

#[tokio::test]
async fn customizers_skip_versions_they_do_not_apply_to() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), "1.20.1", chosen("coblet-test:x"));
    let before = cx.profile.clone();
    launcher([Applet::new("coblet-test:x")])
        .run(&mut cx)
        .await
        .unwrap();
    assert_eq!(cx.profile, before);

    let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
    let before = cx.profile.clone();
    let customizer = Applet {
        needs: VersionTrait::LegacyFmlLibs,
        ..Applet::new("coblet-test:x")
    };
    launcher([customizer]).run(&mut cx).await.unwrap();
    assert_eq!(cx.profile, before);
}

#[tokio::test]
async fn a_customizer_error_stops_the_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
    let customizer = Applet {
        fail: true,
        ..Applet::new("coblet-test:x")
    };
    let error = launcher([customizer]).run(&mut cx).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "step `coblet-test:x` failed: agent jar is missing"
    );
    assert_eq!(cx.request.game_args.len(), 0);
}

#[tokio::test]
async fn an_unknown_customizer_fails_before_any_runs() {
    let dir = tempfile::tempdir().unwrap();
    let request = chosen("coblet-test:x").with_customizer(ExtensionId::new("acme:gone").unwrap());
    let mut cx = context(dir.path(), "c0.30_01c", request);
    let before = cx.profile.clone();
    let error = launcher([Applet::new("coblet-test:x")])
        .run(&mut cx)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, LaunchError::UnknownCustomizer(id) if id.as_str() == "acme:gone"),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        "no launch customizer registered as `acme:gone`"
    );
    assert_eq!(cx.profile, before);
}

#[tokio::test]
async fn an_extra_step_needs_an_existing_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), "c0.30_01c", chosen("coblet-test:x"));
    let customizer = Applet {
        anchor: "missing",
        ..Applet::new("coblet-test:x")
    };
    let error = launcher([customizer]).run(&mut cx).await.unwrap_err();
    assert!(matches!(error, LaunchError::UnknownStep(name) if name == "missing"));
}

#[test]
fn registering_the_same_id_twice_fails() {
    let mut launcher = launcher([Applet::new("coblet-test:x")]);
    assert!(
        launcher
            .register_customizer(Arc::new(Applet::new("coblet-test:x")))
            .is_err()
    );
    assert_eq!(launcher.customizers().len(), 1);
}

/// Customizer that runs `edit` on every version.
struct Edit {
    id: ExtensionId,
    edit: fn(&mut EffectiveLaunchProfile),
}

impl Edit {
    fn new(id: &str, edit: fn(&mut EffectiveLaunchProfile)) -> Arc<Self> {
        Arc::new(Self {
            id: ExtensionId::new(id).unwrap(),
            edit,
        })
    }
}

impl Extension for Edit {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &'static str {
        "Edit test"
    }
}

impl LaunchCustomizer for Edit {
    fn applies_to(&self, _traits: &VersionTraits) -> bool {
        true
    }

    fn customize(
        &self,
        profile: &mut EffectiveLaunchProfile,
        _ctx: &CustomizeCtx,
    ) -> Result<(), LaunchError> {
        (self.edit)(profile);
        Ok(())
    }
}

fn add_auth_server(profile: &mut EffectiveLaunchProfile) {
    profile.jvm_args.push(ProfileArg::always(
        "-Dauth.server=https://auth.example.com/${auth_uuid}",
    ));
}

fn drop_username(profile: &mut EffectiveLaunchProfile) {
    profile.game_args.retain(|arg| arg.value != "--username");
}

fn edit_launcher(edits: &[Arc<Edit>]) -> Launcher {
    let mut launcher = Launcher::empty().with_step(Arc::new(Mark("probe")));
    for edit in edits {
        launcher.register_customizer(edit.clone()).unwrap();
    }
    launcher
}

#[tokio::test]
async fn customizers_may_not_change_auth_arguments_by_default() {
    let dir = tempfile::tempdir().unwrap();
    for (edit, argument) in [
        (
            add_auth_server as fn(&mut EffectiveLaunchProfile),
            "-Dauth.server=https://auth.example.com/${auth_uuid}",
        ),
        (drop_username, "--username"),
    ] {
        let mut cx = context(dir.path(), "1.20.1", chosen("acme:auth"));
        let before = cx.profile.clone();
        let error = edit_launcher(&[Edit::new("acme:auth", edit)])
            .run(&mut cx)
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                LaunchError::AuthArgument { customizer, argument: found }
                    if customizer.as_str() == "acme:auth" && found == argument
            ),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "launch customizer `acme:auth` may not add or remove authentication argument `{argument}` unless the launcher allows it"
            )
        );
        assert_eq!(cx.profile, before);
        assert_eq!(cx.request.game_args.len(), 0);
    }
}

#[tokio::test]
async fn a_granted_customizer_changes_auth_arguments_and_shows_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), "1.20.1", chosen("acme:auth"));
    let mut launcher = edit_launcher(&[Edit::new("acme:auth", add_auth_server)]);
    launcher
        .allow_auth_changes(ExtensionId::new("acme:auth").unwrap())
        .unwrap();
    launcher.run(&mut cx).await.unwrap();
    assert_eq!(
        customize_lines(&cx.profile),
        ["[customize] acme:auth: jvm `-Dauth.server=https://auth.example.com/${auth_uuid}` added"]
    );
    assert_eq!(cx.request.game_args, ["probe"]);
}

#[tokio::test]
async fn a_grant_covers_only_its_own_customizer() {
    let dir = tempfile::tempdir().unwrap();
    let request = chosen("acme:trusted").with_customizer(ExtensionId::new("acme:other").unwrap());
    let mut cx = context(dir.path(), "1.20.1", request);
    let mut launcher = edit_launcher(&[
        Edit::new("acme:trusted", add_auth_server),
        Edit::new("acme:other", drop_username),
    ]);
    launcher
        .allow_auth_changes(ExtensionId::new("acme:trusted").unwrap())
        .unwrap();
    let error = launcher.run(&mut cx).await.unwrap_err();
    assert!(
        matches!(&error, LaunchError::AuthArgument { customizer, .. } if customizer.as_str() == "acme:other"),
        "{error}"
    );
}

#[tokio::test]
async fn other_changes_and_moved_auth_arguments_need_no_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), "1.20.1", chosen("acme:plain"));
    let edit = Edit::new("acme:plain", |profile| {
        profile.game_args.reverse();
        profile
            .jvm_args
            .push(ProfileArg::always("-Dacme.flag=true"));
    });
    edit_launcher(&[edit]).run(&mut cx).await.unwrap();
    assert_eq!(
        customize_lines(&cx.profile),
        [
            "[customize] acme:plain: jvm `-Dacme.flag=true` added",
            "[customize] acme:plain: game arguments reordered",
        ]
    );
}

#[test]
fn only_registered_customizers_can_be_granted() {
    let mut launcher = edit_launcher(&[Edit::new("acme:auth", add_auth_server)]);
    let error = launcher
        .allow_auth_changes(ExtensionId::new("acme:gone").unwrap())
        .unwrap_err();
    assert!(matches!(error, LaunchError::UnknownCustomizer(id) if id.as_str() == "acme:gone"));
    assert!(!launcher.allows_auth_changes(&ExtensionId::new("acme:gone").unwrap()));
    launcher
        .allow_auth_changes(ExtensionId::new("acme:auth").unwrap())
        .unwrap();
    assert!(launcher.allows_auth_changes(&ExtensionId::new("acme:auth").unwrap()));
}
