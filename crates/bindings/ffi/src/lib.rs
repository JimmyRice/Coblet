//! C ABI for coblet.
//!
//! Handles (`CobletSdk`, `CobletGame`) are opaque pointers created and freed
//! in pairs; functions return a [`CobletStatus`] and report details through
//! an optional `CobletError**`; structured data travels as JSON documents
//! with a `schema` field. The header `include/coblet.h` is generated with
//! `cargo xtask gen-header`.
#![deny(unsafe_op_in_unsafe_fn)]

mod game;
mod guard;
mod sdk;
mod types;

pub use game::{
    CobletGame, CobletGameExit, CobletLaunchMode, coblet_game_free, coblet_game_kill,
    coblet_game_next_log, coblet_game_wait, coblet_launch,
};
pub use sdk::{
    CobletSdk, coblet_abi_version, coblet_error_free, coblet_sdk_free, coblet_sdk_new,
    coblet_string_free, coblet_versions_json,
};
pub use types::{COBLET_ABI_VERSION, CobletError, CobletErrorKind, CobletStatus, CobletString};
