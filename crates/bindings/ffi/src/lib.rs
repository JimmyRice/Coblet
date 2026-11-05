//! C ABI for coblet.
//!
//! Handles (`Coblet`, `CobletGame`) are opaque pointers created and freed
//! in pairs; functions return a [`CobletStatus`] and report details through
//! an optional `CobletError**`; structured data travels as JSON documents
//! with a `schema` field. Launch customizers and extra steps are callback
//! tables (`CobletCustomizer`, `CobletStep`) that edit a launch through a
//! `CobletProfileEdit` handle. The header `include/coblet.h` is generated with
//! `cargo xtask gen-header`.
#![deny(unsafe_op_in_unsafe_fn)]

mod context;
mod customize;
mod game;
mod guard;
mod types;

pub use context::{
    Coblet, coblet_abi_version, coblet_error_free, coblet_free, coblet_new,
    coblet_new_with_customizers, coblet_string_free, coblet_versions_json,
};
pub use customize::{
    CobletArgKind, CobletCustomizer, CobletProfileEdit, CobletStep, CobletStepFailure,
    coblet_edit_add_arg, coblet_edit_add_library, coblet_edit_add_native_path,
    coblet_edit_add_step, coblet_edit_fail, coblet_edit_info_json, coblet_edit_remove_arg,
    coblet_edit_remove_library, coblet_edit_set_env, coblet_edit_set_java_major,
    coblet_edit_set_main_class, coblet_step_fail,
};
pub use game::{
    CobletGame, CobletGameExit, CobletLaunchMode, coblet_game_free, coblet_game_kill,
    coblet_game_next_log, coblet_game_report, coblet_game_wait, coblet_launch,
    coblet_launch_customized,
};
pub use types::{COBLET_ABI_VERSION, CobletError, CobletErrorKind, CobletStatus, CobletString};
