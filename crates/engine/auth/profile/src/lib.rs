//! Skins and capes for coblet.
//!
//! An [`AppearanceService`] shows and changes the skins and capes of an
//! account; each auth method names the service of its accounts.
//! [`MinecraftServices`] is the built-in one for Microsoft accounts: it
//! reads the appearance, uploads skins from a file or a URL, switches
//! between the Classic and Slim models, resets the skin and shows or hides
//! capes. It also looks up any player's public skin by name.
//!
//! [`SkinLibrary`] keeps skins on disk in `data_root/skins/` with their
//! names, models and origins, for online and offline accounts alike.
#![forbid(unsafe_code)]

mod appearance;
mod error;
mod http;
mod library;
#[cfg(test)]
mod mock;
mod player;
mod png;

pub use appearance::{AppearanceCaps, AppearanceCtx, AppearanceService};
pub use error::PlayerError;
pub use library::{LibrarySkin, SkinLibrary, SkinOrigin};
pub use player::{MinecraftServices, PublicPlayer, SkinSource};
pub use png::{MAX_SKIN_BYTES, check_skin_png};
