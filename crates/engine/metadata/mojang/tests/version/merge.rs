use super::InheritError;
use crate::RawVersionJson;

fn parse(json: &str) -> RawVersionJson {
    RawVersionJson::from_slice(json.as_bytes()).unwrap()
}

#[test]
fn merges_a_loader_patch_onto_its_parent() {
    let parent = parse(
        r#"{"id":"1.20.1","type":"release","mainClass":"net.minecraft.client.main.Main",
            "arguments":{"game":["--username","${auth_player_name}"],"jvm":["-cp","${classpath}"]},
            "libraries":[{"name":"com.mojang:brigadier:1.1.8"}],
            "javaVersion":{"component":"java-runtime-gamma","majorVersion":17},"assets":"5"}"#,
    );
    let child = parse(
        r#"{"id":"fabric-loader-0.16.14-1.20.1","inheritsFrom":"1.20.1",
            "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
            "arguments":{"game":[],"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},
            "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.14","url":"https://maven.fabricmc.net/"}]}"#,
    );
    let merged = child
        .resolve_inheritance(|id| (id.as_str() == "1.20.1").then(|| parent.clone()))
        .unwrap();
    assert_eq!(merged.id.as_str(), "fabric-loader-0.16.14-1.20.1");
    assert_eq!(merged.inherits_from, None);
    assert_eq!(
        merged.main_class.as_deref(),
        Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
    );
    let names: Vec<String> = merged
        .libraries
        .iter()
        .map(|library| library.name.to_string())
        .collect();
    assert_eq!(
        names,
        [
            "net.fabricmc:fabric-loader:0.16.14",
            "com.mojang:brigadier:1.1.8"
        ]
    );
    let arguments = merged.arguments.unwrap();
    assert_eq!(arguments.jvm.len(), 3);
    assert_eq!(arguments.game.len(), 2);
    assert_eq!(merged.java_version.unwrap().major_version, 17);
    assert_eq!(merged.assets.as_deref(), Some("5"));
}

#[test]
fn reports_missing_parents_and_cycles() {
    let a = parse(r#"{"id":"a","inheritsFrom":"b"}"#);
    let b = parse(r#"{"id":"b","inheritsFrom":"a"}"#);
    assert!(matches!(
        a.clone().resolve_inheritance(|_| None),
        Err(InheritError::Missing { .. })
    ));
    assert!(matches!(
        a.resolve_inheritance(|_| Some(b.clone())),
        Err(InheritError::Cycle(_))
    ));
}
