use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use coblet_runtime::HttpUrl;
use coblet_types::{ExtensionId, PlayerName};
use serde::{Deserialize, Serialize};

use crate::{
    Account, AccountId, AccountState, AccountUpdate, Cape, PlayerProfile, Skin, SkinModel,
};

/// Format version written to `accounts.json`.
pub(crate) const FORMAT: u32 = 1;

/// Contents of `accounts.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AccountsFile {
    pub(crate) default: Option<AccountId>,
    pub(crate) accounts: Vec<Account>,
    pub(crate) instances: BTreeMap<String, AccountId>,
}

impl AccountsFile {
    pub(crate) fn find(&self, id: &AccountId) -> Option<&Account> {
        self.accounts.iter().find(|account| &account.id == id)
    }

    pub(crate) fn find_mut(&mut self, id: &AccountId) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|account| &account.id == id)
    }

    /// Returns the account of `method` with the profile UUID `uuid`.
    pub(crate) fn find_player(&self, method: &ExtensionId, uuid: &str) -> Option<&Account> {
        self.accounts.iter().find(|account| {
            &account.method == method
                && account
                    .profile
                    .as_ref()
                    .is_some_and(|profile| profile.uuid == uuid)
        })
    }

    /// Returns the account an update from `method` belongs to: the one with
    /// the same profile UUID or, for accounts without a profile, the same
    /// `xuid` data value.
    pub(crate) fn find_same(
        &self,
        method: &ExtensionId,
        update: &AccountUpdate,
    ) -> Option<&Account> {
        if let Some(profile) = &update.profile {
            return self.find_player(method, &profile.uuid);
        }
        let xuid = update.data.get("xuid")?;
        self.accounts
            .iter()
            .find(|account| &account.method == method && account.data.get("xuid") == Some(xuid))
    }

    /// Parses a file, upgrading older formats.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let value = migrate(value)?;
        let raw: RawFile = serde_json::from_value(value).map_err(|error| error.to_string())?;
        raw.into_file()
    }

    /// Serializes the file with the current format version.
    pub(crate) fn to_json(&self) -> Vec<u8> {
        let raw = RawFile {
            format: FORMAT,
            default: self.default.as_ref().map(ToString::to_string),
            accounts: self.accounts.iter().map(RawAccount::from_account).collect(),
            instances: self
                .instances
                .iter()
                .map(|(instance, id)| (instance.clone(), id.to_string()))
                .collect(),
        };
        let mut json = serde_json::to_vec_pretty(&raw).unwrap_or_default();
        json.push(b'\n');
        json
    }
}

/// Brings a document of any known format to the current one.
fn migrate(value: serde_json::Value) -> Result<serde_json::Value, String> {
    let format = value
        .get("format")
        .and_then(serde_json::Value::as_u64)
        .ok_or("the file has no format version")?;
    match format {
        1 => Ok(value),
        newer => Err(format!(
            "format {newer} is newer than {FORMAT}, written by a later version of coblet"
        )),
    }
}

#[derive(Serialize, Deserialize)]
struct RawFile {
    format: u32,
    default: Option<String>,
    #[serde(default)]
    accounts: Vec<RawAccount>,
    #[serde(default)]
    instances: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct RawAccount {
    id: String,
    method: ExtensionId,
    state: String,
    profile: Option<RawProfile>,
    session_expires_at: Option<u64>,
    #[serde(default)]
    data: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
struct RawProfile {
    name: PlayerName,
    uuid: String,
    #[serde(default)]
    skins: Vec<RawSkin>,
    #[serde(default)]
    capes: Vec<RawCape>,
}

#[derive(Serialize, Deserialize)]
struct RawSkin {
    id: String,
    url: HttpUrl,
    model: String,
    active: bool,
}

#[derive(Serialize, Deserialize)]
struct RawCape {
    id: String,
    alias: Option<String>,
    url: HttpUrl,
    active: bool,
}

impl RawFile {
    fn into_file(self) -> Result<AccountsFile, String> {
        let id = |id: String| AccountId::new(id).map_err(|error| error.to_string());
        let accounts = self
            .accounts
            .into_iter()
            .map(RawAccount::into_account)
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = BTreeSet::new();
        if let Some(duplicate) = accounts.iter().find(|account| !seen.insert(&account.id)) {
            return Err(format!("account id `{}` appears twice", duplicate.id));
        }
        let instances = self
            .instances
            .into_iter()
            .map(|(instance, account)| Ok((instance, id(account)?)))
            .collect::<Result<_, String>>()?;
        Ok(AccountsFile {
            default: self.default.map(id).transpose()?,
            accounts,
            instances,
        })
    }
}

impl RawAccount {
    fn from_account(account: &Account) -> Self {
        Self {
            id: account.id.to_string(),
            method: account.method.clone(),
            state: account.state.as_str().to_owned(),
            profile: account.profile.as_ref().map(|profile| RawProfile {
                name: profile.name.clone(),
                uuid: profile.uuid.clone(),
                skins: profile
                    .skins
                    .iter()
                    .map(|skin| RawSkin {
                        id: skin.id.clone(),
                        url: skin.url.clone(),
                        model: skin.model.as_str().to_owned(),
                        active: skin.active,
                    })
                    .collect(),
                capes: profile
                    .capes
                    .iter()
                    .map(|cape| RawCape {
                        id: cape.id.clone(),
                        alias: cape.alias.clone(),
                        url: cape.url.clone(),
                        active: cape.active,
                    })
                    .collect(),
            }),
            session_expires_at: account.session_expires_at.map(unix_seconds),
            data: account.data.clone(),
            secrets: account.secret_names.clone(),
        }
    }

    fn into_account(self) -> Result<Account, String> {
        let id = AccountId::new(self.id).map_err(|error| error.to_string())?;
        let state = AccountState::parse(&self.state)
            .ok_or_else(|| format!("account `{id}` has an unknown state `{}`", self.state))?;
        let profile = self
            .profile
            .map(|raw| {
                let mut profile =
                    PlayerProfile::new(raw.name, &raw.uuid).map_err(|error| error.to_string())?;
                profile.skins = raw
                    .skins
                    .into_iter()
                    .map(|skin| {
                        Skin::new(
                            skin.id,
                            skin.url,
                            SkinModel::parse(&skin.model).unwrap_or_default(),
                            skin.active,
                        )
                    })
                    .collect();
                profile.capes = raw
                    .capes
                    .into_iter()
                    .map(|cape| Cape::new(cape.id, cape.alias, cape.url, cape.active))
                    .collect();
                Ok::<_, String>(profile)
            })
            .transpose()?;
        let mut account = Account::new(id, self.method);
        account.state = state;
        account.profile = profile;
        account.session_expires_at = self
            .session_expires_at
            .map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
        account.data = self.data;
        account.secret_names = self.secrets;
        Ok(account)
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use coblet_types::ExtensionId;

    use super::AccountsFile;
    use crate::{Account, AccountId, AccountState, PlayerProfile};

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
        assert_eq!(AccountsFile::parse(&json).unwrap(), file);
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
}
