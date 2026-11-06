use std::collections::BTreeMap;

use crate::{Argument, Condition, RawVersionJson, RuleContext, evaluate};

pub(crate) fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../../../fixtures/mojang/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

fn fixture(name: &str) -> RawVersionJson {
    RawVersionJson::from_slice(&fixture_bytes(&format!("{name}.json"))).unwrap()
}

const FIXTURES: [&str; 9] = [
    "26.3",
    "1.21.11",
    "1.20.1",
    "1.13.2",
    "1.12.2",
    "1.5.2",
    "b1.7.3",
    "a1.2.6",
    "c0.30_01c",
];

#[test]
fn parses_and_round_trips_every_fixture() {
    for name in FIXTURES {
        let raw = fixture(name);
        assert_eq!(raw.id.as_str(), name);
        assert!(raw.main_class.is_some(), "{name}");
        assert!(raw.downloads.client.is_some(), "{name}");
        assert!(raw.asset_index.is_some(), "{name}");
        assert!(raw.java_version.is_some(), "{name}");
        assert!(
            raw.arguments.is_some() != raw.minecraft_arguments.is_some(),
            "{name} has exactly one argument style"
        );
        let json = serde_json::to_vec(&raw).unwrap();
        assert_eq!(RawVersionJson::from_slice(&json).unwrap(), raw, "{name}");
    }
}

#[test]
fn mapping_downloads_are_optional() {
    let modern = fixture("26.3");
    assert!(modern.downloads.client_mappings.is_none());
    assert!(modern.downloads.server_mappings.is_none());
    assert_eq!(
        modern.java_version.unwrap().component,
        "java-runtime-epsilon"
    );
    let obfuscated = fixture("1.20.1");
    assert!(obfuscated.downloads.client_mappings.is_some());
}

#[test]
fn reads_legacy_arguments_and_natives() {
    let beta = fixture("b1.7.3");
    assert_eq!(
        beta.main_class.as_deref(),
        Some("net.minecraft.launchwrapper.Launch")
    );
    assert_eq!(beta.assets.as_deref(), Some("pre-1.6"));
    assert_eq!(beta.tweak_classes(), Vec::<&str>::new());
    let natives = beta
        .libraries
        .iter()
        .find(|library| !library.natives.is_empty())
        .unwrap();
    assert_eq!(natives.natives["osx"], "natives-osx");
    assert_eq!(natives.extract.as_ref().unwrap().exclude, ["META-INF/"]);
    let classic = fixture("c0.30_01c");
    assert_eq!(
        classic.tweak_classes(),
        ["net.minecraft.launchwrapper.AlphaVanillaTweaker"]
    );
}

#[test]
fn evaluates_fixture_rules_per_platform() {
    let raw = fixture("1.20.1");
    let arguments = raw.arguments.as_ref().unwrap();
    let mac = RuleContext::new("osx", "arm64").with_os_version("15.5");
    let linux = RuleContext::new("linux", "x86_64");
    let start_on_first_thread = arguments
        .jvm
        .iter()
        .find_map(|argument| match argument {
            Argument::Conditional { rules, value }
                if value.as_slice() == ["-XstartOnFirstThread"] =>
            {
                Some(rules)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(evaluate(start_on_first_thread, &mac), Condition::Always);
    assert_eq!(evaluate(start_on_first_thread, &linux), Condition::Never);

    let demo = arguments
        .game
        .iter()
        .find_map(|argument| match argument {
            Argument::Conditional { rules, value } if value.as_slice() == ["--demo"] => Some(rules),
            _ => None,
        })
        .unwrap();
    let condition = evaluate(demo, &linux);
    assert!(!condition.is_allowed_by_default());
    assert!(condition.allows(&BTreeMap::from([("is_demo_user".to_owned(), true)])));

    let macos_natives: Vec<&str> = raw
        .libraries
        .iter()
        .filter(|library| evaluate(&library.rules, &mac).is_allowed_by_default())
        .filter_map(|library| library.name.classifier())
        .collect();
    assert!(
        macos_natives.contains(&"natives-macos-arm64"),
        "{macos_natives:?}"
    );
    assert!(
        !macos_natives
            .iter()
            .any(|c| c.contains("linux") || c.contains("windows"))
    );
}

#[test]
fn keeps_unknown_fields() {
    let raw = RawVersionJson::from_slice(
        br#"{"id":"x","libraries":[{"name":"a:b:1","acme":1}],"_comment":"kept"}"#,
    )
    .unwrap();
    assert_eq!(raw.extra["_comment"], "kept");
    assert_eq!(raw.libraries[0].extra["acme"], 1);
}
