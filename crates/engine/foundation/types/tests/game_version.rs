use super::{GameVersionId, GameVersionIdError, VersionType};

#[test]
fn accepts_ids_from_every_era() {
    for id in [
        "rd-132211",
        "a1.0.4",
        "b1.7.3",
        "1.14 Pre-Release 1",
        "1.21.11",
        "26.1",
    ] {
        assert_eq!(GameVersionId::new(id).unwrap().as_str(), id);
    }
}

#[test]
fn rejects_ids_that_cannot_come_from_a_manifest() {
    assert_eq!(GameVersionId::new(""), Err(GameVersionIdError::Empty));
    assert!(matches!(
        GameVersionId::new("1.20 "),
        Err(GameVersionIdError::SurroundingWhitespace(_))
    ));
    assert!(matches!(
        GameVersionId::new("1.20\n"),
        Err(GameVersionIdError::SurroundingWhitespace(_))
    ));
    assert!(matches!(
        GameVersionId::new("1.\u{7}20"),
        Err(GameVersionIdError::ControlCharacter(_))
    ));
    assert_eq!(
        GameVersionId::new("1".repeat(129)),
        Err(GameVersionIdError::TooLong(129))
    );
}

#[test]
fn maps_known_types_and_keeps_unknown_ones() {
    let parsed: Vec<VersionType> = [
        "release",
        "snapshot",
        "old_beta",
        "old_alpha",
        "experiment",
        "april_fools",
    ]
    .into_iter()
    .map(VersionType::from)
    .collect();
    assert_eq!(
        parsed,
        [
            VersionType::Release,
            VersionType::Snapshot,
            VersionType::OldBeta,
            VersionType::OldAlpha,
            VersionType::Experiment,
            VersionType::Other("april_fools".to_owned()),
        ]
    );
    assert_eq!(
        VersionType::from("release".to_owned()),
        VersionType::Release
    );
}

#[test]
fn serializes_with_manifest_spelling() {
    let json = serde_json::to_string(&[VersionType::OldAlpha, VersionType::Other("x".to_owned())])
        .unwrap();
    assert_eq!(json, r#"["old_alpha","x"]"#);
    let back: Vec<VersionType> = serde_json::from_str(r#"["snapshot","pending"]"#).unwrap();
    assert_eq!(
        back,
        [
            VersionType::Snapshot,
            VersionType::Other("pending".to_owned())
        ]
    );
    let id: GameVersionId = serde_json::from_str(r#""1.20.1""#).unwrap();
    assert_eq!(serde_json::to_string(&id).unwrap(), r#""1.20.1""#);
}
