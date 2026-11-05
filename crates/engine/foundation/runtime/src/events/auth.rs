use std::time::SystemTime;

use crate::HttpUrl;

/// What the user has to do to finish a login.
///
/// The engine never opens a browser or reads input itself. A login flow
/// hands this prompt to the caller, which shows it in its own UI, and also
/// publishes it as [`Event::AuthPrompt`](crate::Event::AuthPrompt).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthPrompt {
    /// Enter `user_code` at `verification_uri` on any device.
    DeviceCode {
        /// Short code the user types in.
        user_code: String,
        /// Page where the code is entered.
        verification_uri: HttpUrl,
        /// When the code stops working.
        expires_at: SystemTime,
        /// Instruction text from the identity provider, if any.
        message: Option<String>,
    },
    /// Open `url` in a browser and sign in there.
    OpenUrl {
        /// Authorization page.
        url: HttpUrl,
        /// Where the browser is sent afterwards. The flow waits for this
        /// redirect, from its loopback listener or from the caller.
        redirect_uri: String,
    },
    /// Nothing to do: the flow finishes without user interaction.
    Nothing,
}
