use super::{ExtensionId, ExtensionIdError};

#[test]
fn splits_namespace_and_name() {
    let id = ExtensionId::new("acme:loaders/my-loader.v2").unwrap();
    assert_eq!(id.namespace(), "acme");
    assert_eq!(id.name(), "loaders/my-loader.v2");
    assert_eq!(id.to_string(), "acme:loaders/my-loader.v2");
}

#[test]
fn rejects_malformed_ids() {
    assert_eq!(
        ExtensionId::new("fabric"),
        Err(ExtensionIdError::MissingNamespace("fabric".to_owned()))
    );
    assert_eq!(
        ExtensionId::new(":fabric"),
        Err(ExtensionIdError::EmptyPart(":fabric".to_owned()))
    );
    assert_eq!(
        ExtensionId::new("coblet:"),
        Err(ExtensionIdError::EmptyPart("coblet:".to_owned()))
    );
    assert_eq!(
        ExtensionId::new("Coblet:fabric"),
        Err(ExtensionIdError::InvalidCharacter {
            id: "Coblet:fabric".to_owned(),
            character: 'C'
        })
    );
    assert!(matches!(
        ExtensionId::new("a/b:c"),
        Err(ExtensionIdError::InvalidCharacter { character: '/', .. })
    ));
    assert!(matches!(
        ExtensionId::new("coblet:a:b"),
        Err(ExtensionIdError::InvalidCharacter { character: ':', .. })
    ));
    assert_eq!(
        ExtensionId::new(format!("x:{}", "a".repeat(127))),
        Err(ExtensionIdError::TooLong(129))
    );
}

#[test]
fn round_trips_through_serde() {
    let id = ExtensionId::new("coblet:forge").unwrap();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""coblet:forge""#);
    assert_eq!(serde_json::from_str::<ExtensionId>(&json).unwrap(), id);
    assert!(serde_json::from_str::<ExtensionId>(r#""forge""#).is_err());
}
