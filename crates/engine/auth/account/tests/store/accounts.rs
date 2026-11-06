use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use coblet_launch_command::{Credential, LaunchAuth, OnlineSession};
use coblet_runtime::{
    AuthPrompt, BoxFuture, Extension, MemorySecretStore, Request, Response, Secret, SecretKey,
    SecretStore, TokioStorage, Transport, TransportError,
};
use coblet_types::{ExtensionId, PlayerName};

use super::AccountStore;
use crate::{
    Account, AccountId, AccountSecrets, AccountState, AccountUpdate, AuthCtx, AuthError, AuthFlow,
    AuthMethod, GameLicense, LaunchSession, LicenseCheck, LoginOptions, PlayerProfile, RefreshMode,
    Refreshed,
};

fn checked(license: GameLicense) -> LicenseCheck {
    LicenseCheck::new(license, SystemTime::UNIX_EPOCH)
}

struct NoNetwork;

impl Transport for NoNetwork {
    fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async { Err(TransportError::Timeout) })
    }
}

/// What the scripted method does on refresh.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ready,
    NoLicense,
    Revoked,
}

/// Method that counts token renewals and rotates `refresh_token` each time.
struct Scripted {
    id: ExtensionId,
    renewals: AtomicUsize,
    outcome: Outcome,
}

impl Scripted {
    fn new(outcome: Outcome) -> Arc<Self> {
        Arc::new(Self {
            id: ExtensionId::new("test:scripted").unwrap(),
            renewals: AtomicUsize::new(0),
            outcome,
        })
    }
}

fn profile() -> PlayerProfile {
    PlayerProfile::new(
        PlayerName::new("Notch").unwrap(),
        "069a79f444e94726a5befca90e38aaf5",
    )
    .unwrap()
}

fn online(profile: &PlayerProfile, token: &str) -> LaunchSession {
    LaunchSession::new(LaunchAuth::online(OnlineSession::new(
        profile.name.clone(),
        profile.uuid.clone(),
        Credential::new(token),
    )))
}

impl Extension for Scripted {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &'static str {
        "Scripted"
    }
}

impl AuthMethod for Scripted {
    fn begin<'a>(
        &'a self,
        _ctx: &'a AuthCtx,
        _options: &'a LoginOptions,
    ) -> BoxFuture<'a, Result<AuthFlow, AuthError>> {
        Box::pin(async {
            let mut update = AccountUpdate::new(AccountState::Ready).with_profile(profile());
            update.secrets.insert("refresh_token", Secret::new("r0"));
            update.license = Some(checked(GameLicense::Purchased));
            update.session_expires_at = Some(SystemTime::now() - Duration::from_secs(1));
            Ok(AuthFlow::new(
                AuthPrompt::Nothing,
                async move { Ok(update) },
            ))
        })
    }

    fn refresh<'a>(
        &'a self,
        _ctx: &'a AuthCtx,
        account: &'a Account,
        secrets: &'a AccountSecrets,
        mode: RefreshMode,
    ) -> BoxFuture<'a, Result<Refreshed, AuthError>> {
        Box::pin(async move {
            let fresh = account
                .session_expires_at
                .is_some_and(|expires| expires > SystemTime::now());
            if mode == RefreshMode::IfNeeded && fresh {
                let update = AccountUpdate::from_account(account);
                let session = online(account.profile.as_ref().unwrap(), "cached");
                return Ok(Refreshed::new(update, Some(session)));
            }
            let renewal = self.renewals.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
            secrets
                .put("refresh_token", Secret::new(format!("r{renewal}")))
                .await?;
            match self.outcome {
                Outcome::Revoked => Err(AuthError::InvalidGrant),
                Outcome::NoLicense => {
                    let mut update = AccountUpdate::new(AccountState::NoLicense);
                    update.license = Some(checked(GameLicense::Missing));
                    Ok(Refreshed::new(update, None))
                }
                Outcome::Ready => {
                    let mut update =
                        AccountUpdate::new(AccountState::Ready).with_profile(profile());
                    update.session_expires_at = Some(SystemTime::now() + Duration::from_secs(3600));
                    update.secrets.insert("access", Secret::new("a"));
                    Ok(Refreshed::new(update, Some(online(&profile(), "fresh"))))
                }
            }
        })
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    secrets: Arc<MemorySecretStore>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            secrets: Arc::new(MemorySecretStore::new()),
        }
    }

    fn store(&self) -> AccountStore {
        AccountStore::new(
            Arc::new(TokioStorage::new()),
            self.dir.path(),
            self.secrets.clone(),
            AuthCtx::new(Arc::new(NoNetwork)),
        )
    }

    fn store_with(&self, method: Arc<Scripted>) -> AccountStore {
        let mut store = self.store();
        store.register(method).unwrap();
        store
    }

    async fn secret(&self, account: &AccountId, name: &str) -> Option<String> {
        let key = SecretKey::new(format!("test:scripted/{account}/{name}")).unwrap();
        self.secrets
            .get(&key)
            .await
            .unwrap()
            .map(|secret| secret.expose().to_owned())
    }
}

async fn log_in(store: &AccountStore) -> Account {
    let id = ExtensionId::new("test:scripted").unwrap();
    let flow = store.begin_login(&id, &LoginOptions::new()).await.unwrap();
    store.finish_login(&id, flow).await.unwrap()
}

#[tokio::test]
async fn manages_offline_accounts() {
    let fixture = Fixture::new();
    let store = fixture.store();
    assert_eq!(store.list().await.unwrap().len(), 0);
    let steve = store
        .add_offline(PlayerName::new("Steve").unwrap())
        .await
        .unwrap();
    let again = store
        .add_offline(PlayerName::new("Steve").unwrap())
        .await
        .unwrap();
    assert_eq!(steve.id, again.id);
    let alex = store
        .add_offline(PlayerName::new("Alex").unwrap())
        .await
        .unwrap();
    assert_eq!(store.list().await.unwrap().len(), 2);
    assert_eq!(store.default_account().await.unwrap().unwrap().id, steve.id);

    store.assign("modded", Some(&alex.id)).await.unwrap();
    assert_eq!(
        store.account_for("modded").await.unwrap().unwrap().id,
        alex.id
    );
    assert_eq!(
        store.account_for("vanilla").await.unwrap().unwrap().id,
        steve.id
    );
    let session = store
        .session(&alex.id, RefreshMode::IfNeeded)
        .await
        .unwrap();
    assert!(matches!(session.auth, LaunchAuth::Offline(ref name) if name.as_str() == "Alex"));

    // A second store on the same data root sees the same file.
    let reopened = fixture.store();
    reopened.set_default(Some(&alex.id)).await.unwrap();
    assert_eq!(store.default_account().await.unwrap().unwrap().id, alex.id);

    store.remove(&alex.id).await.unwrap();
    assert!(store.default_account().await.unwrap().is_none());
    assert!(store.account_for("modded").await.unwrap().is_none());
    store.set_default(Some(&steve.id)).await.unwrap();
    assert_eq!(
        store.account_for("modded").await.unwrap().unwrap().id,
        steve.id
    );
    assert!(matches!(
        store.get(&alex.id).await,
        Err(AuthError::UnknownAccount(_))
    ));
    assert!(matches!(
        store.assign("modded", Some(&alex.id)).await,
        Err(AuthError::UnknownAccount(_))
    ));
    let text = std::fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("\"format\": 1") && text.contains("Steve"));
}

#[tokio::test]
async fn saves_login_secrets_and_relogin_updates_the_account() {
    let fixture = Fixture::new();
    let store = fixture.store_with(Scripted::new(Outcome::Ready));
    let account = log_in(&store).await;
    assert_eq!(account.state, AccountState::NeedsRefresh);
    assert!(account.secret_names.contains("refresh_token"));
    assert_eq!(
        fixture
            .secret(&account.id, "refresh_token")
            .await
            .as_deref(),
        Some("r0")
    );
    let again = log_in(&store).await;
    assert_eq!(again.id, account.id);
    assert_eq!(store.list().await.unwrap().len(), 1);
    assert!(
        !std::fs::read_to_string(store.path())
            .unwrap()
            .contains("r0")
    );

    store.remove(&account.id).await.unwrap();
    assert_eq!(fixture.secret(&account.id, "refresh_token").await, None);
}

#[tokio::test]
async fn concurrent_refreshes_share_one_renewal() {
    let fixture = Fixture::new();
    let method = Scripted::new(Outcome::Ready);
    let store = Arc::new(fixture.store_with(method.clone()));
    let account = log_in(&store).await;

    let calls: Vec<_> = (0..4)
        .map(|_| {
            let store = Arc::clone(&store);
            let id = account.id.clone();
            tokio::spawn(async move { store.session(&id, RefreshMode::Force).await })
        })
        .collect();
    for call in calls {
        call.await.unwrap().unwrap();
    }
    assert_eq!(method.renewals.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .secret(&account.id, "refresh_token")
            .await
            .as_deref(),
        Some("r1")
    );
    let saved = store.get(&account.id).await.unwrap();
    assert_eq!(saved.state, AccountState::Ready);
    assert!(saved.secret_names.contains("access"));

    // A later forced refresh renews again.
    store
        .session(&account.id, RefreshMode::Force)
        .await
        .unwrap();
    assert_eq!(method.renewals.load(Ordering::SeqCst), 2);
    let cached = store
        .session(&account.id, RefreshMode::IfNeeded)
        .await
        .unwrap();
    assert_eq!(cached.access_token().unwrap().expose(), "cached");
    assert_eq!(method.renewals.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn saves_what_a_failed_refresh_says_about_the_account() {
    let fixture = Fixture::new();
    let store = fixture.store_with(Scripted::new(Outcome::Revoked));
    let account = log_in(&store).await;
    assert!(matches!(
        store.session(&account.id, RefreshMode::IfNeeded).await,
        Err(AuthError::InvalidGrant)
    ));
    let saved = store.get(&account.id).await.unwrap();
    assert_eq!(saved.state, AccountState::NeedsReauth);
    assert_eq!(
        fixture
            .secret(&account.id, "refresh_token")
            .await
            .as_deref(),
        Some("r1")
    );

    let fixture = Fixture::new();
    let store = fixture.store_with(Scripted::new(Outcome::NoLicense));
    let account = log_in(&store).await;
    assert!(matches!(
        store.session(&account.id, RefreshMode::IfNeeded).await,
        Err(AuthError::NoLicense)
    ));
    let refreshed = store.refresh(&account.id).await.unwrap();
    assert_eq!(refreshed.state, AccountState::NoLicense);
    assert!(refreshed.profile.is_none());
}

#[tokio::test]
async fn verifies_licenses_and_keeps_the_last_check() {
    let fixture = Fixture::new();
    let store = fixture.store_with(Scripted::new(Outcome::Revoked));
    let offline = store
        .add_offline(PlayerName::new("Steve").unwrap())
        .await
        .unwrap();
    assert!(!store.any_owns_game().await.unwrap());
    let account = log_in(&store).await;
    assert!(account.owns_game());
    assert!(store.any_owns_game().await.unwrap());
    // A failed check keeps the last license; only the state changes.
    assert!(matches!(
        store.verify_license(&account.id).await,
        Err(AuthError::InvalidGrant)
    ));
    let saved = store.get(&account.id).await.unwrap();
    assert_eq!(saved.state, AccountState::NeedsReauth);
    assert_eq!(saved.license, Some(checked(GameLicense::Purchased)));
    let unchanged = store.verify_license(&offline.id).await.unwrap();
    assert_eq!(unchanged.license, None);
    assert!(!unchanged.owns_game());

    let fixture = Fixture::new();
    let store = fixture.store_with(Scripted::new(Outcome::NoLicense));
    let account = log_in(&store).await;
    let checked_again = store.verify_license(&account.id).await.unwrap();
    assert_eq!(checked_again.license, Some(checked(GameLicense::Missing)));
    assert!(!store.any_owns_game().await.unwrap());
}

#[tokio::test]
async fn rechecks_are_not_merged_into_other_refreshes() {
    let fixture = Fixture::new();
    let method = Scripted::new(Outcome::Ready);
    let store = Arc::new(fixture.store_with(method.clone()));
    let account = log_in(&store).await;
    let forced = tokio::spawn({
        let store = Arc::clone(&store);
        let id = account.id.clone();
        async move { store.refresh(&id).await }
    });
    tokio::task::yield_now().await;
    store.verify_license(&account.id).await.unwrap();
    forced.await.unwrap().unwrap();
    assert_eq!(method.renewals.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reports_unknown_methods_and_broken_files() {
    let fixture = Fixture::new();
    let store = fixture.store_with(Scripted::new(Outcome::Ready));
    let account = log_in(&store).await;
    let without = fixture.store();
    assert!(matches!(
        without.session(&account.id, RefreshMode::IfNeeded).await,
        Err(AuthError::UnknownMethod(_))
    ));
    assert!(matches!(
        without
            .begin_login(
                &ExtensionId::new("coblet:microsoft").unwrap(),
                &LoginOptions::new()
            )
            .await,
        Err(AuthError::UnknownMethod(_))
    ));

    std::fs::write(store.path(), b"{\"format\": 9}").unwrap();
    assert!(matches!(
        store.list().await,
        Err(AuthError::AccountFile { .. })
    ));
}

#[tokio::test]
async fn imports_prism_accounts_without_tokens() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let prism = fixture.dir.path().join("prism-accounts.json");
    std::fs::write(
        &prism,
        br#"{"formatVersion": 3, "accounts": [
            {"type": "MSA", "active": true, "profile": {"id": "069a79f444e94726a5befca90e38aaf5", "name": "Notch"},
             "msa": {"refresh_token": "never-copied"}},
            {"type": "Offline", "profile": {"id": "5627dd98e6be3c21b8a8e92344183641", "name": "Steve"}}
        ]}"#,
    )
    .unwrap();
    let added = store.import_prism(&prism).await.unwrap();
    assert_eq!(added.len(), 2);
    assert_eq!(added[0].state, AccountState::NeedsReauth);
    assert_eq!(added[0].method.as_str(), "coblet:microsoft");
    assert!(added[0].secret_names.is_empty());
    assert_eq!(added[1].state, AccountState::Ready);
    assert_eq!(
        store.default_account().await.unwrap().unwrap().id,
        added[0].id
    );
    assert_eq!(store.import_prism(&prism).await.unwrap().len(), 0);
    assert!(
        !std::fs::read_to_string(store.path())
            .unwrap()
            .contains("never-copied")
    );
}
