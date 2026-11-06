use std::path::Path;

use super::{PlanPath, RelPath, RelPathError};

#[test]
fn rejects_escaping_paths() {
    assert_eq!(RelPath::new(""), Err(RelPathError::Empty));
    assert!(matches!(
        RelPath::new("/etc/passwd"),
        Err(RelPathError::Absolute(_))
    ));
    for path in ["a/../b", "..", "a//b", "a/", "./a"] {
        assert!(
            matches!(RelPath::new(path), Err(RelPathError::BadSegment(_))),
            "{path}"
        );
    }
    for path in [
        "C:/Windows",
        "C:foo",
        "a\\..\\b",
        "mods/a.jar:stream",
        "a\0b",
    ] {
        assert!(
            matches!(RelPath::new(path), Err(RelPathError::InvalidCharacter(_))),
            "{path}"
        );
    }
}

#[test]
fn joins_and_resolves() {
    let mods = RelPath::new("mods").unwrap();
    let jar = mods.join("sodium/sodium.jar").unwrap();
    assert_eq!(jar.as_str(), "mods/sodium/sodium.jar");
    assert_eq!(jar.file_name(), "sodium.jar");
    assert!(mods.join("../x").is_err());
    assert_eq!(
        jar.under(Path::new("/games")),
        Path::new("/games")
            .join("mods")
            .join("sodium")
            .join("sodium.jar")
    );
}

#[test]
fn serializes_plan_paths() {
    let path = PlanPath::staging(RelPath::new("client.jar").unwrap());
    assert_eq!(path.to_string(), "staging:client.jar");
    let json = serde_json::to_string(&path).unwrap();
    assert_eq!(json, r#"{"root":"staging","path":"client.jar"}"#);
    assert_eq!(serde_json::from_str::<PlanPath>(&json).unwrap(), path);
    assert!(serde_json::from_str::<PlanPath>(r#"{"root":"data","path":"../x"}"#).is_err());
}
