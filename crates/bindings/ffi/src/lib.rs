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
//! (`CobletSecretStore`). Other ways of signing in are callback tables
//! (`CobletAuthMethod`) that work through a `CobletAuthCtx` handle and
//! start with `coblet_login_begin_with`; skins and capes elsewhere come
//! from callback tables (`CobletAppearanceService`) or TOML and JSON
//! definitions (`coblet_builder_add_appearance_service_toml`). Mirrors are prefix rules
//! (`coblet_builder_add_mirror`) or callback tables (`CobletUrlRewriter`),
//! and game versions come from Mojang's manifest, other manifests
//! (`coblet_builder_add_version_manifest`) and callback tables
//! (`CobletVersionSource`, `coblet_builder_add_version_source`) that fetch
//! through a `CobletSourceCtx` handle; `coblet_builder_remove_version_source`
//! drops one, Mojang's included. Mod Loaders are callback tables
//! (`CobletModLoader`) that fetch through a `CobletLoaderCtx` handle, or
//! TOML and JSON definitions (`coblet_builder_add_loader_toml`), and get
//! versions from more places with `coblet_builder_add_loader_version_source`
//! and its TOML and JSON forms; `coblet_loader_*` lists their
//! versions and recipes, `coblet_plan_launch_json` resolves a version with
//! one and `coblet_launch_loader` starts it. Java providers and locators
//! are callback tables (`CobletJavaProvider`, `CobletJavaLocator`) that
//! answer through a `CobletJavaCtx` handle, or TOML and JSON provider
//! definitions (`coblet_builder_add_java_provider_toml`); built-in ones are
//! removed with `coblet_builder_remove_java_provider`, and `coblet_java_*`
//! lists installs, providers and packages. Version trait sources are
//! callback tables (`CobletTraitSource`) that answer through a
//! `CobletTraitCtx` handle, or TOML and JSON tables
//! (`coblet_builder_add_trait_table_toml`); `coblet_version_traits_json`
//! reports a version's traits and where each came from. Launch steps are
//! callback tables (`CobletStep`) that write launch state back through a
//! `CobletStepCtx` handle; `coblet_builder_add_step`,
//! `coblet_builder_replace_step` and `coblet_builder_remove_step` change
//! the step list every launch runs, and `coblet_launch_steps_json` lists
//! it. `${name}` placeholders get values from the builder
//! (`coblet_builder_add_placeholder`), customizers and steps, and from
//! callback tables (`CobletPlaceholderSource`) that answer through a
//! `CobletPlaceholderCtx` handle. Events (launch steps, file transfers, game
//! starts, logs and exits) are read with `coblet_events_next` as fixed
//! layout `CobletEvent` structs. The header `include/coblet.h` is generated with
//! `cargo xtask gen-header`.
#![deny(unsafe_op_in_unsafe_fn)]

mod accounts;
mod appearance;
mod auth_methods;
mod builder;
mod context;
mod customize;
mod events;
mod game;
mod guard;
mod http;
mod java;
mod loaders;
mod login;
mod placeholders;
mod profile;
mod secrets;
mod sources;
mod steps;
#[cfg(test)]
mod testing;
mod traits;
mod types;
mod version_sources;

pub use accounts::{
    CobletSkinModel, coblet_account_add_offline, coblet_account_assign, coblet_account_refresh,
    coblet_account_refresh_profile, coblet_account_remove, coblet_account_reset_skin,
    coblet_account_set_cape, coblet_account_set_default, coblet_account_set_skin_model,
    coblet_account_upload_skin, coblet_account_upload_skin_url, coblet_account_verify_license,
    coblet_accounts_json, coblet_any_account_owns_game, coblet_bytes_free, coblet_download_texture,
};
pub use appearance::{
    COBLET_APPEARANCE_CAN_CHOOSE_CAPE, COBLET_APPEARANCE_CAN_HIDE_CAPE,
    COBLET_APPEARANCE_CAN_RESET_SKIN, COBLET_APPEARANCE_CAN_SET_MODEL,
    COBLET_APPEARANCE_CAN_UPLOAD_PNG, COBLET_APPEARANCE_CAN_UPLOAD_URL, COBLET_APPEARANCE_REPLACE,
    CobletAppearanceCtx, CobletAppearanceService, coblet_appearance_capabilities_json,
    coblet_appearance_ctx_access_token, coblet_appearance_ctx_fail, coblet_appearance_ctx_png,
    coblet_appearance_ctx_send, coblet_appearance_ctx_set_result, coblet_appearance_services_json,
    coblet_builder_add_appearance_service, coblet_builder_add_appearance_service_json,
    coblet_builder_add_appearance_service_toml, coblet_builder_remove_appearance_service,
    coblet_builder_set_appearance_for,
};
pub use auth_methods::{
    COBLET_AUTH_REPLACE, CobletAuthCtx, CobletAuthMethod, coblet_auth_ctx_fail,
    coblet_auth_ctx_is_cancelled, coblet_auth_ctx_put_secret, coblet_auth_ctx_secret,
    coblet_auth_ctx_send, coblet_auth_ctx_set_result, coblet_auth_ctx_show_prompt,
    coblet_auth_ctx_wait_redirect, coblet_auth_methods_json, coblet_builder_add_auth_method,
};
pub use builder::{
    COBLET_MIRROR_TRUST_METADATA, CobletBuilder, coblet_builder_add_customizer,
    coblet_builder_add_mirror, coblet_builder_add_platform_profile,
    coblet_builder_add_platform_profile_toml, coblet_builder_add_url_rewriter,
    coblet_builder_add_version_manifest, coblet_builder_allow_auth_changes, coblet_builder_build,
    coblet_builder_free, coblet_builder_new, coblet_builder_set_encrypted_secret_file,
    coblet_builder_set_mirror_fallback, coblet_builder_set_msa_client_id,
    coblet_builder_set_platform, coblet_builder_set_secret_store,
};
pub use context::{
    Coblet, coblet_abi_version, coblet_error_free, coblet_free, coblet_new, coblet_platform_json,
    coblet_string_free, coblet_versions_json,
};
pub use customize::{
    CobletArgKind, CobletCustomizer, CobletProfileEdit, coblet_edit_add_arg,
    coblet_edit_add_library, coblet_edit_add_native_path, coblet_edit_add_step, coblet_edit_fail,
    coblet_edit_info_json, coblet_edit_remove_arg, coblet_edit_remove_library, coblet_edit_set_env,
    coblet_edit_set_java_major, coblet_edit_set_main_class, coblet_edit_set_placeholder,
};
pub use events::{
    CobletEvent, CobletEventKind, CobletEvents, CobletTransferKind, coblet_event_clear,
    coblet_events_free, coblet_events_next, coblet_events_subscribe,
};
pub use game::{
    COBLET_OPERATION_ID_LEN, CobletGame, CobletGameExit, CobletLaunchMode, coblet_game_free,
    coblet_game_kill, coblet_game_next_log, coblet_game_operation_id, coblet_game_report,
    coblet_game_wait, coblet_generate_operation_id, coblet_launch, coblet_launch_account,
    coblet_launch_customized, coblet_launch_loader,
};
pub use java::{
    COBLET_JAVA_REPLACE, CobletJavaCtx, CobletJavaLocator, CobletJavaProvider,
    coblet_builder_add_java_directory, coblet_builder_add_java_executable,
    coblet_builder_add_java_locator, coblet_builder_add_java_provider,
    coblet_builder_add_java_provider_json, coblet_builder_add_java_provider_toml,
    coblet_builder_remove_java_locator, coblet_builder_remove_java_provider, coblet_install_java,
    coblet_java_ctx_fail, coblet_java_ctx_fetch, coblet_java_ctx_set_result,
    coblet_java_installs_json, coblet_java_locators_json, coblet_java_packages_json,
    coblet_java_providers_json,
};
pub use loaders::{
    COBLET_LOADER_REPLACE, CobletLoaderCtx, CobletModLoader, coblet_builder_add_loader,
    coblet_builder_add_loader_json, coblet_builder_add_loader_toml, coblet_loader_ctx_fail,
    coblet_loader_ctx_fetch, coblet_loader_ctx_set_result, coblet_loader_recipe_json,
    coblet_loader_versions_json, coblet_loaders_json, coblet_plan_launch_json,
};
pub use login::{
    CobletLogin, CobletLoginInteraction, coblet_login_begin, coblet_login_begin_with,
    coblet_login_cancel, coblet_login_finish, coblet_login_free, coblet_login_prompt_json,
    coblet_login_submit_redirect,
};
pub use placeholders::{
    COBLET_PLACEHOLDER_REPLACE, CobletPlaceholderCtx, CobletPlaceholderSource,
    CobletUnknownPlaceholders, coblet_builder_add_placeholder,
    coblet_builder_add_placeholder_source, coblet_builder_remove_placeholder_source,
    coblet_builder_set_unknown_placeholders, coblet_placeholder_ctx_fail,
    coblet_placeholder_ctx_set_result, coblet_placeholder_sources_json,
};
pub use profile::{
    CobletJavaMatch, CobletJavaSource, CobletMatchFailure, CobletNatives, CobletPlatformProfile,
    CobletProfileDraft, CobletRule, CobletRules, coblet_draft_add_library,
    coblet_draft_add_native_path, coblet_draft_append_arg, coblet_draft_fail,
    coblet_draft_handle_natives, coblet_draft_info_json, coblet_draft_prepend_env_paths,
    coblet_draft_refuse, coblet_draft_remove_args, coblet_draft_remove_libraries,
    coblet_draft_set_env, coblet_draft_set_java_arch, coblet_draft_set_java_major,
    coblet_draft_set_java_match, coblet_draft_set_java_runtime, coblet_draft_set_java_source,
    coblet_draft_warn, coblet_match_fail, coblet_rules_fail, coblet_rules_set,
    coblet_rules_set_native,
};
pub use secrets::{
    COBLET_SECRET_KEY_LEN, CobletSecretCall, CobletSecretStore, coblet_generate_secret_key,
    coblet_secret_call_fail, coblet_secret_call_set_value,
};
pub use sources::{CobletRewrite, CobletUrlRewriter, coblet_rewrite_set_url};
pub use steps::{
    CobletStep, CobletStepCtx, coblet_builder_add_step, coblet_builder_remove_step,
    coblet_builder_replace_step, coblet_launch_steps_json, coblet_step_ctx_add_jvm_arg,
    coblet_step_ctx_fail, coblet_step_ctx_set_client_jar, coblet_step_ctx_set_game_assets,
    coblet_step_ctx_set_java, coblet_step_ctx_set_natives_dir, coblet_step_ctx_set_placeholder,
};
pub use traits::{
    COBLET_TRAITS_REPLACE, CobletTraitCtx, CobletTraitSource, coblet_builder_add_trait_source,
    coblet_builder_add_trait_table_json, coblet_builder_add_trait_table_toml,
    coblet_builder_remove_trait_source, coblet_builtin_version_traits_toml, coblet_trait_ctx_fail,
    coblet_trait_ctx_fetch, coblet_trait_ctx_set_result, coblet_trait_sources_json,
    coblet_version_traits_json,
};
pub use types::{
    COBLET_ABI_VERSION, CobletBytes, CobletError, CobletErrorKind, CobletStatus, CobletString,
};
pub use version_sources::{
    COBLET_SOURCE_REPLACE, CobletSourceCtx, CobletVersionSource,
    coblet_builder_add_loader_version_source, coblet_builder_add_loader_version_source_json,
    coblet_builder_add_loader_version_source_toml, coblet_builder_add_version_source,
    coblet_builder_remove_loader_version_source, coblet_builder_remove_version_source,
    coblet_loader_version_sources_json, coblet_source_ctx_fail, coblet_source_ctx_fetch,
    coblet_source_ctx_set_result, coblet_version_sources_json,
};
