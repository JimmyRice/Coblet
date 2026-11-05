use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use coblet::auth::{AccountState, AccountUpdate, AuthFlow, PlayerProfile, callback_channel};
use coblet::runtime::{AuthPrompt, SecretKey, SecretStore as _};
use coblet::types::{ExtensionId, PlayerName};

use crate::secrets::ForeignSecrets;
use crate::{
    AccountState as State, CobletBuilder, CobletError, ErrorKind, LoginFlow, LoginInteraction,
    LoginPrompt, SecretStore, generate_secret_key,
};

fn kind(error: &CobletError) -> ErrorKind {
    let CobletError::Failed { kind, .. } = error;
    *kind
}

fn builder(dir: &tempfile::TempDir) -> Arc<CobletBuilder> {
    CobletBuilder::new(
        dir.path().to_string_lossy().into_owned(),
        "coblet-uniffi-test/1.0".to_owned(),
    )
    .unwrap()
}

#[tokio::test]
async fn offline_accounts_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = builder(&dir).build().unwrap();
    let steve = coblet
        .add_offline_account("Steve".to_owned())
        .await
        .unwrap();
    assert_eq!(steve.method, "coblet:offline");
    assert_eq!(steve.state, State::Ready);
    assert_eq!(steve.player.as_ref().unwrap().name, "Steve");
    let alex = coblet.add_offline_account("Alex".to_owned()).await.unwrap();

    assert_eq!(coblet.accounts().await.unwrap().len(), 2);
    assert_eq!(
        coblet.default_account().await.unwrap().unwrap().id,
        steve.id
    );
    coblet
        .set_default_account(Some(alex.id.clone()))
        .await
        .unwrap();
    coblet
        .assign_account("pack".to_owned(), Some(steve.id.clone()))
        .await
        .unwrap();
    let chosen = coblet.account_for("pack".to_owned()).await.unwrap();
    assert_eq!(chosen.unwrap().id, steve.id);
    let other = coblet.account_for("other".to_owned()).await.unwrap();
    assert_eq!(other.unwrap().id, alex.id);

    coblet.remove_account(steve.id.clone()).await.unwrap();
    let missing = coblet.account(steve.id).await.unwrap_err();
    assert_eq!(kind(&missing), ErrorKind::NotFound);
    let bad = coblet.add_offline_account("no spaces".to_owned()).await;
    assert_eq!(kind(&bad.unwrap_err()), ErrorKind::InvalidArgument);
}

#[tokio::test]
async fn offline_accounts_have_no_online_appearance() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = builder(&dir).build().unwrap();
    let steve = coblet
        .add_offline_account("Steve".to_owned())
        .await
        .unwrap();
    let error = coblet.appearance(steve.id).await.unwrap_err();
    assert_eq!(kind(&error), ErrorKind::Unsupported);
}

#[tokio::test]
async fn microsoft_login_needs_a_client_id() {
    let dir = tempfile::tempdir().unwrap();
    let coblet = builder(&dir).build().unwrap();
    let error = coblet
        .begin_microsoft_login(LoginInteraction::DeviceCode, None)
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::Unsupported);
    assert_eq!(
        kind(&builder(&dir).set_msa_client_id(String::new()).unwrap_err()),
        ErrorKind::InvalidArgument
    );
}

#[derive(Default)]
struct Keychain(Mutex<HashMap<String, String>>);

impl SecretStore for Keychain {
    fn get(&self, key: String) -> Result<Option<String>, CobletError> {
        Ok(self.0.lock().unwrap().get(&key).cloned())
    }

    fn put(&self, key: String, value: String) -> Result<(), CobletError> {
        if value.is_empty() {
            return Err(CobletError::new(ErrorKind::Other, "keychain is locked"));
        }
        self.0.lock().unwrap().insert(key, value);
        Ok(())
    }

    fn delete(&self, key: String) -> Result<(), CobletError> {
        self.0.lock().unwrap().remove(&key);
        Ok(())
    }
}

#[tokio::test]
async fn foreign_secret_stores_back_coblet_secrets() {
    let keychain = Arc::new(Keychain::default());
    let store = ForeignSecrets(keychain.clone());
    let key = SecretKey::new("coblet:microsoft/a/refresh_token").unwrap();
    store
        .put(&key, coblet::runtime::Secret::new("token"))
        .await
        .unwrap();
    assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "token");
    assert_eq!(keychain.0.lock().unwrap().len(), 1);
    let error = store
        .put(&key, coblet::runtime::Secret::new(""))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("keychain is locked"), "{error}");
    store.delete(&key).await.unwrap();
    assert!(store.get(&key).await.unwrap().is_none());

    let dir = tempfile::tempdir().unwrap();
    let builder = builder(&dir);
    builder.set_secret_store(keychain).unwrap();
    builder.build().unwrap();
}

#[test]
fn encrypted_secret_files_need_a_32_byte_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = generate_secret_key().unwrap();
    assert_eq!(key.len(), 32);
    let builder = builder(&dir);
    let error = builder
        .set_encrypted_secret_file("secrets.bin".to_owned(), vec![0; 16])
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    let path = dir.path().join("secrets.bin");
    builder
        .set_encrypted_secret_file(path.to_string_lossy().into_owned(), key)
        .unwrap();
    builder.build().unwrap();
}

fn facade(dir: &tempfile::TempDir) -> coblet::Coblet {
    coblet::Coblet::builder()
        .data_root(dir.path())
        .user_agent(coblet::runtime::UserAgent::new("coblet-uniffi-test/1.0").unwrap())
        .build()
        .unwrap()
}

fn offline() -> ExtensionId {
    ExtensionId::new("coblet:offline").unwrap()
}

fn notch() -> AccountUpdate {
    let profile = PlayerProfile::new(
        PlayerName::new("Notch").unwrap(),
        "069a79f444e94726a5befca90e38aaf5",
    )
    .unwrap();
    AccountUpdate::new(AccountState::Ready).with_profile(profile)
}

#[tokio::test]
async fn login_flows_take_a_redirect_and_finish_once() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, receiver) = callback_channel();
    let prompt = AuthPrompt::OpenUrl {
        url: coblet::runtime::HttpUrl::new("https://login.example/authorize").unwrap(),
        redirect_uri: "myapp://auth".to_owned(),
    };
    let flow = AuthFlow::new(prompt, async move {
        let uri = receiver.recv().await?;
        assert_eq!(uri, "myapp://auth?code=c&state=s");
        Ok(notch())
    })
    .with_callback(sender);
    let flow = LoginFlow::new(facade(&dir), offline(), flow);
    assert!(
        matches!(flow.prompt(), LoginPrompt::OpenUrl { ref redirect_uri, .. } if redirect_uri == "myapp://auth")
    );
    assert!(flow.accepts_redirect());
    flow.submit_redirect("myapp://auth?code=c&state=s".to_owned())
        .unwrap();
    assert!(!flow.accepts_redirect());
    assert_eq!(
        kind(
            &flow
                .submit_redirect("myapp://again".to_owned())
                .unwrap_err()
        ),
        ErrorKind::InvalidState
    );

    let account = flow.finish().await.unwrap();
    assert_eq!(account.player.unwrap().name, "Notch");
    assert_eq!(
        kind(&flow.finish().await.unwrap_err()),
        ErrorKind::InvalidState
    );
}

#[tokio::test]
async fn cancelled_login_flows_fail_with_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let flow = AuthFlow::new(AuthPrompt::Nothing, std::future::pending());
    let flow = Arc::new(LoginFlow::new(facade(&dir), offline(), flow));
    assert_eq!(
        kind(&flow.submit_redirect("x://y".to_owned()).unwrap_err()),
        ErrorKind::InvalidState
    );
    let waiting = tokio::spawn({
        let flow = Arc::clone(&flow);
        async move { flow.finish().await }
    });
    tokio::task::yield_now().await;
    flow.cancel();
    let error = waiting.await.unwrap().unwrap_err();
    assert_eq!(kind(&error), ErrorKind::Cancelled);
}
