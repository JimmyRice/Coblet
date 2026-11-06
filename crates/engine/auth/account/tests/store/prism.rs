use super::parse;

#[test]
fn reads_names_and_types_only() {
    let accounts = parse(
            br#"{"formatVersion": 3, "accounts": [
                {"type": "MSA", "active": true, "profile": {"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch", "skin": {"id": "x"}},
                 "msa": {"token": "secret"}, "utoken": {"token": "secret"}},
                {"type": "Offline", "profile": {"id": "5627dd98e6be3c21b8a8e92344183641", "name": "Steve"}},
                {"type": "Mojang", "profile": {"id": "069a79f444e94726a5befca90e38aaf5", "name": "Old"}},
                {"type": "MSA"},
                {"type": "MSA", "profile": {"id": "", "name": ""}}
            ]}"#,
        )
        .unwrap();
    assert_eq!(accounts.len(), 2);
    assert!(!accounts[0].offline && accounts[0].active);
    assert_eq!(accounts[0].profile.name.as_str(), "Notch");
    assert!(accounts[1].offline && !accounts[1].active);
    assert!(parse(br#"{"formatVersion": 2, "accounts": []}"#).is_err());
}
