//! C ABI for coblet.
//!
//! Handles (`Coblet`, `CobletGame`) are opaque pointers created and freed
//! in pairs; a `CobletBuilder` adds Platform Profiles and launch
//! customizers before the `Coblet` is built; functions return a [`CobletStatus`] and report details through
//! an optional `CobletError**`; structured data travels as JSON documents
//! with a `schema` field. Platform Profiles written in C are callback
//! tables (`CobletPlatformProfile`) that change profiles through a
//! `CobletProfileDraft` handle; launch customizers and extra steps are
//! callback tables (`CobletCustomizer`, `CobletStep`) that edit a launch
//! through a `CobletProfileEdit` handle. The header `include/coblet.h` is generated with
//! `cargo xtask gen-header`.
#![deny(unsafe_op_in_unsafe_fn)]

mod builder;
mod context;
mod customize;
mod game;
mod guard;
mod profile;
mod types;

pub use builder::{
    CobletBuilder, coblet_builder_add_customizer, coblet_builder_add_platform_profile,
    coblet_builder_add_platform_profile_toml, coblet_builder_build, coblet_builder_free,
    coblet_builder_new, coblet_builder_set_platform,
};
pub use context::{
    Coblet, coblet_abi_version, coblet_error_free, coblet_free, coblet_new, coblet_platform_json,
    coblet_string_free, coblet_versions_json,
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
pub use profile::{
    CobletJavaSource, CobletMatchFailure, CobletNatives, CobletPlatformProfile, CobletProfileDraft,
    CobletRule, CobletRules, coblet_draft_add_library, coblet_draft_add_native_path,
    coblet_draft_append_arg, coblet_draft_fail, coblet_draft_handle_natives,
    coblet_draft_info_json, coblet_draft_prepend_env_paths, coblet_draft_refuse,
    coblet_draft_remove_args, coblet_draft_remove_libraries, coblet_draft_set_env,
    coblet_draft_set_java_arch, coblet_draft_set_java_major, coblet_draft_set_java_runtime,
    coblet_draft_set_java_source, coblet_draft_warn, coblet_match_fail, coblet_rules_fail,
    coblet_rules_set, coblet_rules_set_native,
};
pub use types::{COBLET_ABI_VERSION, CobletError, CobletErrorKind, CobletStatus, CobletString};
