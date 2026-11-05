use coblet_types::PlayerName;
use serde::Deserialize;

use crate::PlayerProfile;

/// Account read from a Prism Launcher `accounts.json`, without tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrismAccount {
    /// `true` for an offline account, `false` for a Microsoft account.
    pub(crate) offline: bool,
    pub(crate) profile: PlayerProfile,
    pub(crate) active: bool,
}

#[derive(Deserialize)]
struct RawList {
    #[serde(rename = "formatVersion")]
    format_version: u64,
    #[serde(default)]
    accounts: Vec<RawAccount>,
}

#[derive(Deserialize)]
struct RawAccount {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    active: bool,
    profile: Option<RawProfile>,
}

#[derive(Deserialize)]
struct RawProfile {
    id: String,
    name: String,
}

/// Reads the accounts of a Prism account list (format 3).
///
/// Only the account type, player name, UUID and the active flag are read.
/// Entries of other types or without a valid profile are skipped.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<PrismAccount>, String> {
    let list: RawList = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if list.format_version != 3 {
        return Err(format!(
            "prism account list format {} is not supported, only 3 is",
            list.format_version
        ));
    }
    Ok(list
        .accounts
        .into_iter()
        .filter_map(|account| {
            let offline = match account.kind.as_str() {
                "MSA" => false,
                "Offline" => true,
                _ => return None,
            };
            let raw = account.profile?;
            let name = PlayerName::new(raw.name).ok()?;
            let profile = PlayerProfile::new(name, &raw.id).ok()?;
            Some(PrismAccount {
                offline,
                profile,
                active: account.active,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
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
}
