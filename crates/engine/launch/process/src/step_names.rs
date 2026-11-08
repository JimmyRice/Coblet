//! Names of the built-in launch steps, in the order [`Launcher::new`](crate::Launcher::new) runs them.
//!
//! A step that replaces one of them must leave in the
//! [`LaunchContext`](crate::LaunchContext) what later steps read:
//!
//! | Step | Leaves |
//! |---|---|
//! | [`SELECT_JAVA`] | `java` |
//! | [`VERIFY_FILES`] | the game files on disk; `game_assets` for old asset indexes |
//! | [`PREPARE_NATIVES`] | `natives_directory` |
//! | [`CLIENT_JAR`] | `client_jar` |
//! | [`LEGACY_FML_LIBS`] | nothing |
//! | [`LOGGING_CONFIG`] | the log configuration in `jvm_args` |
//! | [`PRE_LAUNCH`] | nothing |
//! | [`RESOLVE_PLACEHOLDERS`] | values in `placeholders` |
//! | [`BUILD_COMMAND`] | `command` |
//! | [`SPAWN`] | `session` |

/// Uses the configured Java after checking it, or finds or installs one that fits.
pub const SELECT_JAVA: &str = "select-java";
/// Downloads missing or damaged game files and prepares assets.
pub const VERIFY_FILES: &str = "verify-files";
/// Extracts native jars into the natives directory.
pub const PREPARE_NATIVES: &str = "prepare-natives";
/// Chooses the client jar.
pub const CLIENT_JAR: &str = "client-jar";
/// Places the libraries old FML downloads at start.
pub const LEGACY_FML_LIBS: &str = "legacy-fml-libs";
/// Adds the version's log4j configuration.
pub const LOGGING_CONFIG: &str = "logging-config";
/// Runs the pre-launch command.
pub const PRE_LAUNCH: &str = "pre-launch";
/// Asks the placeholder sources for placeholders nothing else set.
pub const RESOLVE_PLACEHOLDERS: &str = "resolve-placeholders";
/// Builds the launch command.
pub const BUILD_COMMAND: &str = "build-command";
/// Starts the game and monitors it.
pub const SPAWN: &str = "spawn";

/// Every built-in step, in order.
pub const BUILTIN: &[&str] = &[
    SELECT_JAVA,
    VERIFY_FILES,
    PREPARE_NATIVES,
    CLIENT_JAR,
    LEGACY_FML_LIBS,
    LOGGING_CONFIG,
    PRE_LAUNCH,
    RESOLVE_PLACEHOLDERS,
    BUILD_COMMAND,
    SPAWN,
];
