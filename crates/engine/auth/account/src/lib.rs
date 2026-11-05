//! Accounts and logins for coblet.
//!
//! An [`Account`] holds the non-sensitive part of a login: the player
//! [`PlayerProfile`], its [`AccountState`] and data its [`AuthMethod`] keeps
//! between refreshes. Tokens live in a
//! [`SecretStore`](coblet_runtime::SecretStore).
//!
//! Auth methods are extensions. [`OfflineMethod`] (`coblet:offline`) is
//! always available; the `msa` feature adds Microsoft login. A login starts
//! with [`AuthMethod::begin`], which returns an [`AuthFlow`] whose
//! [`AuthPrompt`](coblet_runtime::AuthPrompt) the caller shows in its own
//! UI. The engine never opens a browser or reads input itself.
//!
//! ```
//! use std::sync::Arc;
//!
//! use coblet_auth::{AuthCtx, AuthMethod, LoginOptions, OfflineMethod};
//! # use coblet_runtime::{BoxFuture, Request, Response, Transport, TransportError};
//! # struct NoNetwork;
//! # impl Transport for NoNetwork {
//! #     fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
//! #         Box::pin(async { Err(TransportError::Timeout) })
//! #     }
//! # }
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let ctx = AuthCtx::new(Arc::new(NoNetwork));
//! let flow = OfflineMethod::new()
//!     .begin(&ctx, &LoginOptions::new().with_hint("Steve"))
//!     .await?;
//! let update = flow.finish().await?;
//! assert_eq!(update.profile.unwrap().name.as_str(), "Steve");
//! # Ok::<(), coblet_auth::AuthError>(())
//! # }).unwrap();
//! ```
#![forbid(unsafe_code)]

mod account;
mod error;
mod flow;
mod method;
mod offline;
mod secrets;
mod services;
mod session;

pub use account::{
    Account, AccountId, AccountIdError, AccountState, Cape, PlayerProfile, ProfileUuidError, Skin,
    SkinModel,
};
pub use error::AuthError;
pub use flow::{AuthFlow, CallbackReceiver, CallbackSender, callback_channel};
pub use method::{
    AccountUpdate, AuthCtx, AuthMethod, Credentials, Interaction, LoginOptions, Redirect,
    RefreshMode, Refreshed,
};
pub use offline::{OFFLINE_METHOD, OfflineMethod};
pub use secrets::AccountSecrets;
pub use services::ProfileJsonError;
pub use session::LaunchSession;
