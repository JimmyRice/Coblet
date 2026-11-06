use std::sync::Arc;

use coblet_launch_command::LaunchAuth;
use coblet_runtime::{
    AuthPrompt, BoxFuture, MemorySecretStore, Request, Response, Transport, TransportError,
};

use super::OfflineMethod;
use crate::{
    Account, AccountId, AccountSecrets, AuthCtx, AuthError, AuthMethod, LoginOptions, RefreshMode,
};

struct NoNetwork;

impl Transport for NoNetwork {
    fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async { Err(TransportError::Timeout) })
    }
}

#[tokio::test]
async fn logs_in_with_the_offline_uuid() {
    let method = OfflineMethod::new();
    let ctx = AuthCtx::new(Arc::new(NoNetwork));
    let flow = method
        .begin(&ctx, &LoginOptions::new().with_hint("Steve"))
        .await
        .unwrap();
    assert_eq!(flow.prompt(), &AuthPrompt::Nothing);
    let update = flow.finish().await.unwrap();
    let profile = update.profile.clone().unwrap();
    assert_eq!(profile.uuid, "5627dd98e6be3c21b8a8e92344183641");

    let mut account = Account::new(AccountId::generate(), method.id.clone());
    let secrets = AccountSecrets::new(
        Arc::new(MemorySecretStore::new()),
        &account.method,
        &account.id,
    );
    account.profile = Some(profile);
    let refreshed = method
        .refresh(&ctx, &account, &secrets, RefreshMode::Force)
        .await
        .unwrap();
    let session = refreshed.session.unwrap();
    assert!(matches!(session.auth, LaunchAuth::Offline(ref name) if name.as_str() == "Steve"));
}

#[tokio::test]
async fn rejects_missing_or_bad_names() {
    let method = OfflineMethod::new();
    let ctx = AuthCtx::new(Arc::new(NoNetwork));
    assert!(matches!(
        method.begin(&ctx, &LoginOptions::new()).await,
        Err(AuthError::InvalidOptions(_))
    ));
    assert!(matches!(
        method
            .begin(&ctx, &LoginOptions::new().with_hint("no spaces"))
            .await,
        Err(AuthError::InvalidName(_))
    ));
}
