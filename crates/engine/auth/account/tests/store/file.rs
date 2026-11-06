use std::time::{Duration, SystemTime};

use coblet_types::ExtensionId;

use super::AccountsFile;
use crate::{Account, AccountId, AccountState, GameLicense, LicenseCheck, PlayerProfile};

fn sample() -> AccountsFile {
    let profile = PlayerProfile::from_services_json(
            br#"{"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch",
                "skins": [{"id": "s", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/a", "variant": "SLIM"}],
                "capes": [{"id": "c", "state": "ACTIVE", "url": "http://textures.minecraft.net/texture/b", "alias": "Migrator"}]}"#,
        )
        .unwrap();
    let mut account = Account::new(
        AccountId::new("a1").unwrap(),
        ExtensionId::new("coblet:microsoft").unwrap(),
    );
    account.profile = Some(profile);
    account.state = AccountState::NoLicense;
    account.license = Some(LicenseCheck::new(
        GameLicense::Missing,
        SystemTime::UNIX_EPOCH + Duration::from_secs(42),
    ));
    account.session_expires_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(99));
    account.data.insert("xuid".to_owned(), "1".to_owned());
    account.secret_names.insert("refresh_token".to_owned());
    AccountsFile {
        default: Some(account.id.clone()),
        instances: [("pack".to_owned(), account.id.clone())].into(),
        accounts: vec![account],
    }
}

#[test]
fn round_trips() {
    let file = sample();
    let json = file.to_json();
    let text = String::from_utf8(json.clone()).unwrap();
    assert!(text.contains(r#""format": 1"#));
    assert!(!text.contains("token\":"));
    assert!(text.contains(r#""value": "missing""#), "{text}");
    assert_eq!(AccountsFile::parse(&json).unwrap(), file);
}

#[test]
fn reads_licenses_leniently() {
    let file = br#"{"format": 1, "default": null, "accounts": [
            {"id": "a", "method": "coblet:microsoft", "state": "ready", "profile": null, "session_expires_at": null},
            {"id": "b", "method": "coblet:microsoft", "state": "ready", "profile": null, "session_expires_at": null,
             "license": {"value": "purchased", "checked_at": 7}},
            {"id": "c", "method": "coblet:microsoft", "state": "ready", "profile": null, "session_expires_at": null,
             "license": {"value": "lifetime", "checked_at": 7}}
        ]}"#;
    let file = AccountsFile::parse(file).unwrap();
    let licenses: Vec<_> = file
        .accounts
        .iter()
        .map(|account| account.license)
        .collect();
    let bought = LicenseCheck::new(
        GameLicense::Purchased,
        SystemTime::UNIX_EPOCH + Duration::from_secs(7),
    );
    assert_eq!(licenses, [None, Some(bought), None]);
    assert!(file.accounts[1].owns_game());
    assert!(!file.accounts[0].owns_game());
}

#[test]
fn rejects_newer_or_broken_files() {
    let newer = AccountsFile::parse(br#"{"format": 2, "accounts": []}"#).unwrap_err();
    assert!(newer.contains("newer"), "{newer}");
    assert!(AccountsFile::parse(b"{}").is_err());
    assert!(AccountsFile::parse(b"not json").is_err());
    let twice = br#"{"format": 1, "default": null, "accounts": [
            {"id": "a", "method": "coblet:offline", "state": "ready", "profile": null, "session_expires_at": null},
            {"id": "a", "method": "coblet:offline", "state": "ready", "profile": null, "session_expires_at": null}
        ]}"#;
    assert!(AccountsFile::parse(twice).unwrap_err().contains("twice"));
    let state = br#"{"format": 1, "default": null, "accounts": [
            {"id": "a", "method": "coblet:offline", "state": "sleepy", "profile": null, "session_expires_at": null}
        ]}"#;
    assert!(AccountsFile::parse(state).unwrap_err().contains("sleepy"));
}
