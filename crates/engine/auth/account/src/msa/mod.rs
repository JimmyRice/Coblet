//! Microsoft login: OAuth (device code or PKCE), Xbox Live, XSTS and
//! Minecraft services.

mod config;
mod http;
#[cfg(feature = "loopback")]
mod loopback;
#[cfg(not(feature = "loopback"))]
#[path = "no_loopback.rs"]
mod loopback;
mod method;
mod minecraft;
#[cfg(test)]
#[path = "../../tests/msa/mock.rs"]
mod mock;
mod oauth;
mod random;
mod xbox;

pub use config::{DEFAULT_SCOPES, MsaClientId, MsaClientIdError, MsaConfig};
pub use method::{MICROSOFT_METHOD, MicrosoftMethod};
