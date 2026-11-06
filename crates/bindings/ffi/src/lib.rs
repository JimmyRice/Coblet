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
//! through a `CobletProfileEdit` handle. Accounts, Microsoft logins
//! (`CobletLogin`), skins and capes are managed with `coblet_account_*`
//! and `coblet_login_*`; a secret store written in C is a callback table
//! (`CobletSecretStore`). The header `include/coblet.h` is generated with
//! `cargo xtask gen-header`.
#![deny(unsafe_op_in_unsafe_fn)]

mod accounts;
mod builder;
mod context;
mod customize;
mod game;
mod guard;
mod login;
mod profile;
mod secrets;
mod types;

pub use accounts::{
    CobletSkinModel, coblet_account_add_offline, coblet_account_assign, coblet_account_refresh,
    coblet_account_refresh_profile, coblet_account_remove, coblet_account_reset_skin,
    coblet_account_set_cape, coblet_account_set_default, coblet_account_set_skin_model,
    coblet_account_upload_skin, coblet_account_upload_skin_url, coblet_accounts_json,
    coblet_bytes_free, coblet_download_texture,
};
pub use builder::{
    CobletBuilder, coblet_builder_add_customizer, coblet_builder_add_platform_profile,
    coblet_builder_add_platform_profile_toml, coblet_builder_allow_auth_changes,
    coblet_builder_build, coblet_builder_free, coblet_builder_new,
    coblet_builder_set_encrypted_secret_file, coblet_builder_set_msa_client_id,
    coblet_builder_set_platform, coblet_builder_set_secret_store,
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
    coblet_launch_account, coblet_launch_customized,
};
pub use login::{
    CobletLogin, CobletLoginInteraction, coblet_login_begin, coblet_login_cancel,
    coblet_login_finish, coblet_login_free, coblet_login_prompt_json, coblet_login_submit_redirect,
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
pub use secrets::{
    COBLET_SECRET_KEY_LEN, CobletSecretCall, CobletSecretStore, coblet_generate_secret_key,
    coblet_secret_call_fail, coblet_secret_call_set_value,
};
pub use types::{
    COBLET_ABI_VERSION, CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString,
};
