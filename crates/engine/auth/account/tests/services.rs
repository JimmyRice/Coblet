use crate::PlayerProfile;

#[test]
fn rejects_documents_without_a_valid_identity() {
    assert!(PlayerProfile::from_services_json(b"{}").is_err());
    assert!(PlayerProfile::from_services_json(br#"{"id": "short", "name": "Notch"}"#).is_err());
    assert!(
        PlayerProfile::from_services_json(
            br#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "bad name"}"#
        )
        .is_err()
    );
    let bare = PlayerProfile::from_services_json(
        br#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch"}"#,
    )
    .unwrap();
    assert!(bare.skins.is_empty() && bare.capes.is_empty());
}
