//! Skins and capes for coblet.
//!
//! [`Player`] talks to Minecraft services with the access token of an
//! online session: it reads the appearance, uploads skins from a file or a
//! URL, switches between the Classic and Slim models, resets the skin and
//! shows or hides capes. It also looks up any player's public skin by name.
//!
//! [`SkinLibrary`] keeps skins on disk in `data_root/skins/` with their
//! names, models and origins, for online and offline accounts alike.
#![forbid(unsafe_code)]

mod error;
mod http;
mod library;
#[cfg(test)]
mod mock;
mod player;
mod png;

pub use error::PlayerError;
pub use library::{LibrarySkin, SkinLibrary, SkinOrigin};
pub use player::{Player, PublicPlayer, SkinSource};
pub use png::{MAX_SKIN_BYTES, check_skin_png};
