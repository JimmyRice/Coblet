use std::ffi::OsString;
use std::sync::Arc;

use coblet_types::{MiB, PlayerName};

use crate::{
    AssetsLocation, CommandError, Credential, FeatureCondition, JavaExecutable, LaunchAuth,
    LaunchCommand, LaunchCommandBuilder, MainClass, Memory, OnlineSession, PlaceholderResolver,
    QuickPlay, Ready, Resolution, TemplateArg,
};

fn base(auth: LaunchAuth) -> LaunchCommandBuilder<Ready> {
    LaunchCommand::builder()
        .java(JavaExecutable::new("/jdk/bin/java"))
        .main_class(MainClass::new("net.minecraft.client.main.Main"))
        .auth(auth)
        .game_directory("/game")
        .classpath_separator(":")
}

fn steve() -> PlayerName {
    PlayerName::new("Steve").unwrap()
}

fn args(command: &LaunchCommand) -> Vec<String> {
    command
        .args()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn offline_values() {
    let command = base(LaunchAuth::offline(steve()))
        .assets(AssetsLocation::new("/assets", "pre-1.6"))
        .natives_directory("/natives")
        .game_args([
            "${auth_player_name}",
            "${auth_session}",
            "${auth_access_token}",
            "${user_type}",
            "${user_properties}",
            "${game_assets}",
            "${natives_directory}",
            "${launcher_name}",
        ])
        .build()
        .unwrap();
    assert_eq!(
        args(&command)[1..],
        [
            "Steve",
            "-",
            "0",
            "legacy",
            "{}",
            "/assets/virtual/pre-1.6",
            "/natives",
            "coblet"
        ]
    );
    assert_eq!(command.cwd(), std::path::Path::new("/game"));
}

#[test]
fn memory_comes_first_and_classpath_joins() {
    let command = base(LaunchAuth::offline(steve()))
        .classpath(["/a.jar", "/b.jar"])
        .jvm_args(["-cp", "${classpath}"])
        .memory(Memory::new(MiB(256), MiB(1024)).unwrap())
        .build()
        .unwrap();
    assert_eq!(
        args(&command),
        [
            "-Xms256M",
            "-Xmx1024M",
            "-cp",
            "/a.jar:/b.jar",
            "net.minecraft.client.main.Main"
        ]
    );
}

#[test]
fn features_follow_auth_resolution_and_quick_play() {
    let demo = TemplateArg::when(
        "--demo",
        FeatureCondition::when([("is_demo_user".to_owned(), true)]),
    );
    let size = TemplateArg::when(
        "--width=${resolution_width}",
        FeatureCondition::when([("has_custom_resolution".to_owned(), true)]),
    );
    let world = TemplateArg::when(
        "${quickPlaySingleplayer}",
        FeatureCondition::when([("is_quick_play_singleplayer".to_owned(), true)]),
    );
    let offline = base(LaunchAuth::offline(steve()))
        .game_args([demo.clone(), size.clone(), world.clone()])
        .build()
        .unwrap();
    assert_eq!(args(&offline), ["net.minecraft.client.main.Main"]);

    let demo_run = base(LaunchAuth::demo(steve()))
        .game_args([demo, size, world])
        .resolution(Resolution {
            width: 854,
            height: 480,
        })
        .quick_play(QuickPlay::Singleplayer("New World".to_owned()))
        .build()
        .unwrap();
    assert_eq!(args(&demo_run)[1..], ["--demo", "--width=854", "New World"]);
}

#[test]
fn explicit_feature_overrides_derived_one() {
    let demo = TemplateArg::when(
        "--demo",
        FeatureCondition::when([("is_demo_user".to_owned(), true)]),
    );
    let command = base(LaunchAuth::demo(steve()))
        .game_args([demo])
        .feature("is_demo_user", false)
        .build()
        .unwrap();
    assert_eq!(args(&command).len(), 1);
}

#[test]
fn online_secrets_are_redacted() {
    let mut session = OnlineSession::new(
        steve(),
        "0123456789abcdef0123456789abcdef",
        Credential::new("eyJ.secret.token"),
    );
    session.xuid = Some(Credential::new("2535400000000000"));
    let command = base(LaunchAuth::online(session))
        .game_args([
            "--accessToken",
            "${auth_access_token}",
            "--session",
            "${auth_session}",
            "--xuid",
            "${auth_xuid}",
            "--userType",
            "${user_type}",
        ])
        .build()
        .unwrap();
    let shown = command.to_string();
    let debug = format!("{command:?}");
    for text in [&shown, &debug] {
        assert!(!text.contains("eyJ.secret.token"), "{text}");
        assert!(!text.contains("2535400000000000"), "{text}");
    }
    assert!(shown.contains("--accessToken '<redacted>'"));
    assert!(shown.contains("--session 'token:<redacted>:0123456789abcdef0123456789abcdef'"));
    assert!(shown.contains("--userType msa"));
    let exposed = command.expose_secrets().to_string();
    assert!(exposed.contains("eyJ.secret.token"));
    assert_eq!(args(&command)[2], "eyJ.secret.token");
}

#[test]
fn display_quotes_spaces() {
    let command = base(LaunchAuth::offline(steve()))
        .game_args(["--gameDir", "/Users/a b/.minecraft", "it's"])
        .build()
        .unwrap();
    assert_eq!(
        command.to_string(),
        "/jdk/bin/java net.minecraft.client.main.Main --gameDir '/Users/a b/.minecraft' 'it'\\''s'"
    );
}

#[test]
fn argfile_holds_jvm_args_and_main_class() {
    let long = "x".repeat(100);
    let builder = || {
        base(LaunchAuth::offline(steve()))
            .classpath([format!("/{long}.jar"), "/c \"q\".jar".to_owned()])
            .jvm_args(["-cp", "${classpath}"])
            .game_args(["--version", "1.20.1"])
    };
    let short = builder().argfile("/tmp/args.txt", 10_000).build().unwrap();
    assert!(short.argfile().is_none());

    let command = builder().argfile("/tmp/args.txt", 50).build().unwrap();
    assert_eq!(args(&command), ["@/tmp/args.txt", "--version", "1.20.1"]);
    let file = command.argfile().unwrap();
    assert_eq!(file.path, std::path::Path::new("/tmp/args.txt"));
    assert_eq!(
        file.content,
        format!("\"-cp\"\n\"/{long}.jar:/c \\\"q\\\".jar\"\n\"net.minecraft.client.main.Main\"\n")
    );
}

struct Brand;

impl PlaceholderResolver for Brand {
    fn resolve(&self, name: &str) -> Option<OsString> {
        (name == "brand").then(|| "acme".into())
    }
}

#[test]
fn resolvers_and_unknown_placeholders() {
    let error = base(LaunchAuth::offline(steve()))
        .game_args(["${brand}"])
        .build()
        .unwrap_err();
    assert_eq!(error, CommandError::UnknownPlaceholder("brand".to_owned()));
    let command = base(LaunchAuth::offline(steve()))
        .game_args(["${brand}"])
        .placeholder_resolver(Arc::new(Brand))
        .build()
        .unwrap();
    assert_eq!(args(&command)[1], "acme");
}

#[test]
fn game_directory_is_required() {
    let error = LaunchCommand::builder()
        .java(JavaExecutable::new("java"))
        .main_class(MainClass::new("Main"))
        .auth(LaunchAuth::offline(steve()))
        .build()
        .unwrap_err();
    assert_eq!(error, CommandError::Missing("game directory"));
}
