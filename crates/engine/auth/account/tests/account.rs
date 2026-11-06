use std::time::{Duration, SystemTime};

use coblet_types::{ExtensionId, PlayerName};

use super::{Account, AccountId, AccountState, PlayerProfile, SkinModel};

#[test]
fn normalizes_profile_uuids() {
    let name = PlayerName::new("Steve").unwrap();
    let profile = PlayerProfile::new(name.clone(), "069A79F4-44E9-4726-A5BE-FCA90E38AAF5").unwrap();
    assert_eq!(profile.uuid, "069a79f444e94726a5befca90e38aaf5");
    assert!(PlayerProfile::new(name.clone(), "069a79f4").is_err());
    assert!(PlayerProfile::new(name, "z69a79f444e94726a5befca90e38aaf5").is_err());
}

#[test]
fn reports_expired_sessions_as_needing_refresh() {
    let mut account = Account::new(
        AccountId::new("a").unwrap(),
        ExtensionId::new("coblet:microsoft").unwrap(),
    );
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    assert_eq!(account.state_at(now), AccountState::Ready);
    account.session_expires_at = Some(now);
    assert_eq!(account.state_at(now), AccountState::NeedsRefresh);
    account.state = AccountState::NoLicense;
    assert_eq!(account.state_at(now), AccountState::NoLicense);
}

#[test]
fn round_trips_names() {
    for state in [
        AccountState::Ready,
        AccountState::NeedsRefresh,
        AccountState::NeedsReauth,
        AccountState::NoLicense,
        AccountState::NoProfile,
    ] {
        assert_eq!(AccountState::parse(state.as_str()), Some(state));
    }
    assert_eq!(SkinModel::parse("SLIM"), Some(SkinModel::Slim));
    assert_eq!(SkinModel::parse("wide"), None);
}
