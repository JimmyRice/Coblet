use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
use coblet_download::{Downloader, DownloaderConfig};
use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
use coblet_mojang::{RawVersionJson, TraitTable};
use coblet_platform::{ProfileEngine, ResolvedProfile};
use coblet_runtime::{
    BoxFuture, LogLevel, LogSource, Request, Response, TokioSpawner, TokioStorage, Transport,
    TransportError,
};
use coblet_types::{Arch, OsFamily, Platform, PlayerName};

use super::Launcher;
use crate::{
    GameLayout, LaunchContext, LaunchError, LaunchHook, LaunchRequest, LaunchServices, LaunchStep,
    LogItem,
};

/// Transport for launches that need no network; it is never called.
struct Offline;

impl Transport for Offline {
    fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(std::future::pending())
    }
}

const VERSION: &str = r#"{
    "id": "t1",
    "type": "release",
    "mainClass": "net.minecraft.client.Main",
    "minecraftArguments": "--username ${auth_player_name} --accessToken ${auth_access_token} --gameDir ${game_directory}",
    "libraries": [],
    "javaVersion": { "component": "java-runtime-alpha", "majorVersion": 16 },
    "releaseTime": "2020-01-01T00:00:00+00:00",
    "time": "2020-01-01T00:00:00+00:00"
}"#;

/// A `java` that answers the version probe, records its arguments, logs,
/// writes a crash report and exits with 3.
const FAKE_JAVA: &str = r#"#!/bin/sh
if [ "$1" = "-XshowSettings:properties" ]; then
  echo 'Property settings:' >&2
  echo '    java.home = /fake/16' >&2
  echo '    java.version = 16.0.2' >&2
  echo '    os.arch = amd64' >&2
  exit 0
fi
printf '%s\n' "$@" > args.txt
echo '<log4j:Event logger="net.minecraft.client.Main" timestamp="1700000000000" level="INFO" thread="main">'
echo '  <log4j:Message><![CDATA[Setting user: Steve token secret-token-123]]></log4j:Message>'
echo '</log4j:Event>'
echo 'stderr line' >&2
mkdir -p crash-reports
echo crash > crash-reports/crash-1.txt
exit 3
"#;

fn write_script(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn context(root: &Path, auth: LaunchAuth) -> LaunchContext {
    let storage = Arc::new(TokioStorage::new());
    let transport: Arc<dyn coblet_runtime::Transport> = Arc::new(Offline);
    let meta = MetaCache::new(Arc::clone(&transport), storage.clone(), root.join("meta"));
    let services = LaunchServices::new(
        storage.clone(),
        Arc::new(TokioSpawner::new()),
        Downloader::new(transport, storage, DownloaderConfig::default()),
        Arc::new(CatalogContext::new(meta, CachePolicy::PreferCache)),
        OsFamily::Linux,
    );
    let raw = RawVersionJson::from_slice(VERSION.as_bytes()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
    let platform = Platform::new(OsFamily::Linux, Arch::X86_64);
    let profile = ProfileEngine::builtin()
        .resolve(&resolved, &platform)
        .unwrap();
    let java = root.join("jdk/bin/java");
    write_script(&java, FAKE_JAVA);
    let request = LaunchRequest::new(auth).with_java(&java);
    LaunchContext::new(
        services,
        profile,
        GameLayout::new(root.join("data"), root.join("game")),
        request,
    )
}

fn online() -> LaunchAuth {
    LaunchAuth::online(OnlineSession::new(
        PlayerName::new("Steve").unwrap(),
        "0123456789abcdef0123456789abcdef",
        Credential::new("secret-token-123"),
    ))
}

#[tokio::test]
async fn launches_streams_redacted_logs_and_reports_the_crash() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(dir.path(), online());
    let marker = dir.path().join("post-exit.txt");
    write_script(
        &dir.path().join("post.sh"),
        &format!(
            "#!/bin/sh\necho \"$COBLET_GAME_DIR\" > '{}'\n",
            marker.display()
        ),
    );
    cx.request.post_exit = Some(LaunchHook::new(
        dir.path().join("post.sh"),
        Vec::<String>::new(),
    ));
    let session = Launcher::new().launch(cx).await.unwrap();
    assert!(!session.command().to_string().contains("secret-token-123"));
    let mut logs = session.logs();
    let exit = session.wait().await.unwrap();

    assert_eq!(exit.exit.code, Some(3));
    assert!(!exit.killed);
    assert!(exit.suspected_crash);
    assert_eq!(exit.crash_reports.len(), 1);

    let mut lines = Vec::new();
    while let Some(item) = logs.next().await {
        if let LogItem::Line(line) = item {
            lines.push(line);
        }
    }
    let event = lines
        .iter()
        .find(|line| line.logger.as_deref() == Some("net.minecraft.client.Main"))
        .unwrap();
    assert_eq!(event.message, "Setting user: Steve token <redacted>");
    assert_eq!(event.level, LogLevel::Info);
    assert!(
        lines
            .iter()
            .any(|line| line.source == LogSource::Stderr && line.message == "stderr line")
    );
    assert!(
        lines
            .iter()
            .any(|line| line.source == LogSource::Launcher && line.message.contains("exit code 3"))
    );
    assert!(
        lines
            .iter()
            .all(|line| !line.raw.contains("secret-token-123"))
    );

    let args = std::fs::read_to_string(dir.path().join("game/args.txt")).unwrap();
    assert!(
        args.contains(
            "net.minecraft.client.Main\n--username\nSteve\n--accessToken\nsecret-token-123\n"
        ),
        "{args}"
    );
    assert!(args.contains("-Djava.library.path="));
    let post = std::fs::read_to_string(marker).unwrap();
    assert_eq!(post.trim(), dir.path().join("game").to_string_lossy());
}

#[tokio::test]
async fn rejects_a_configured_java_of_the_wrong_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(
        dir.path(),
        LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
    );
    cx.profile.java.major = 17;
    let error = Launcher::new().launch(cx).await.unwrap_err();
    assert!(
        matches!(
            error,
            LaunchError::JavaMismatch {
                found_major: 16,
                major: 17,
                ..
            }
        ),
        "{error}"
    );
}

#[tokio::test]
async fn a_failing_pre_launch_command_stops_the_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(
        dir.path(),
        LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
    );
    write_script(&dir.path().join("pre.sh"), "#!/bin/sh\nexit 1\n");
    cx.request.pre_launch = Some(LaunchHook::new(
        dir.path().join("pre.sh"),
        Vec::<String>::new(),
    ));
    let error = Launcher::new().launch(cx).await.unwrap_err();
    assert!(
        matches!(error, LaunchError::PreLaunch(exit) if exit.code == Some(1)),
        "{error}"
    );
    assert!(!dir.path().join("game/args.txt").exists());
}

struct Mark;

impl LaunchStep for Mark {
    fn name(&self) -> &'static str {
        "mark"
    }

    fn run<'a>(&'a self, cx: &'a mut LaunchContext) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            cx.request.game_args.push("--marked".to_owned());
            Ok(())
        })
    }
}

#[tokio::test]
async fn steps_can_be_inserted_replaced_and_removed() {
    let mut launcher = Launcher::new();
    assert_eq!(
        launcher.names(),
        [
            "select-java",
            "verify-files",
            "prepare-natives",
            "client-jar",
            "legacy-fml-libs",
            "logging-config",
            "pre-launch",
            "build-command",
            "spawn"
        ]
    );
    launcher
        .insert_before("build-command", Arc::new(Mark))
        .unwrap();
    launcher.remove("spawn").unwrap();
    assert!(matches!(
        launcher.remove("spawn"),
        Err(LaunchError::UnknownStep(_))
    ));
    let dir = tempfile::tempdir().unwrap();
    let mut cx = context(
        dir.path(),
        LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
    );
    launcher.run(&mut cx).await.unwrap();
    let command = cx.command.unwrap();
    assert_eq!(command.args().last().unwrap(), "--marked");
    assert!(cx.session.is_none());
}
