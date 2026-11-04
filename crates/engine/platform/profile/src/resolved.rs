use coblet_catalog::Timestamp;
use coblet_mojang::{
    Argument, AssetIndexRef, Download, JavaVersion, Library, LoggingConfig, RawVersionJson,
    VersionTraits,
};
use coblet_types::{GameVersionId, VersionType};

use crate::ProfileError;

/// Component UID of the game itself.
pub const MINECRAFT_UID: &str = "net.minecraft";
/// Component UID of LWJGL 2.
pub const LWJGL2_UID: &str = "org.lwjgl";
/// Component UID of LWJGL 3.
pub const LWJGL3_UID: &str = "org.lwjgl3";

/// Component of the Instance stack, identified by UID and version.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ComponentRef {
    /// Component UID, such as `net.minecraft` or `org.lwjgl3`.
    pub uid: String,
    /// Component version.
    pub version: String,
}

/// Platform-neutral launch description: the version JSON merged with the
/// Instance's components, rules still unevaluated.
///
/// It is the input of [`ProfileEngine::resolve`](crate::ProfileEngine::resolve).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedProfile {
    /// Game version.
    pub game_version: GameVersionId,
    /// Release channel.
    pub version_type: Option<VersionType>,
    /// Release time.
    pub release_time: Option<Timestamp>,
    /// Components in stack order.
    pub components: Vec<ComponentRef>,
    /// Java class with the `main` method.
    pub main_class: String,
    /// Libraries in classpath order, with their rules.
    pub libraries: Vec<Library>,
    /// JVM arguments with their rules.
    pub jvm_args: Vec<Argument>,
    /// Game arguments with their rules.
    pub game_args: Vec<Argument>,
    /// Whether the arguments came from the pre-1.13 `minecraftArguments` string,
    /// which carries no JVM arguments.
    pub legacy_arguments: bool,
    /// The client jar.
    pub client: Option<Download>,
    /// Asset index to use.
    pub asset_index: Option<AssetIndexRef>,
    /// Asset index ID.
    pub assets: String,
    /// Java runtime the version was built for.
    pub java: Option<JavaVersion>,
    /// Log configuration.
    pub logging: Option<LoggingConfig>,
    /// Version traits.
    pub traits: VersionTraits,
}

fn lwjgl_component(libraries: &[Library]) -> Option<ComponentRef> {
    libraries.iter().find_map(|library| {
        let name = &library.name;
        match (name.group(), name.artifact()) {
            ("org.lwjgl.lwjgl", "lwjgl") => Some(ComponentRef {
                uid: LWJGL2_UID.to_owned(),
                version: name.version().to_owned(),
            }),
            ("org.lwjgl", "lwjgl") => Some(ComponentRef {
                uid: LWJGL3_UID.to_owned(),
                version: name.version().to_owned(),
            }),
            _ => None,
        }
    })
}

impl ResolvedProfile {
    /// Builds the profile of a vanilla version from its merged version JSON.
    ///
    /// Components are derived from the file: `net.minecraft`, plus
    /// `org.lwjgl` (LWJGL 2) or `org.lwjgl3` (LWJGL 3) from the libraries.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::Incomplete`] when the file still inherits from
    /// another version or has no main class.
    pub fn from_version(raw: RawVersionJson, traits: VersionTraits) -> Result<Self, ProfileError> {
        if let Some(parent) = &raw.inherits_from {
            return Err(ProfileError::Incomplete(format!(
                "version `{}` still inherits from `{parent}`",
                raw.id
            )));
        }
        let main_class = raw.main_class.ok_or_else(|| {
            ProfileError::Incomplete(format!("version `{}` has no main class", raw.id))
        })?;
        let mut components = vec![ComponentRef {
            uid: MINECRAFT_UID.to_owned(),
            version: raw.id.as_str().to_owned(),
        }];
        components.extend(lwjgl_component(&raw.libraries));
        let (jvm_args, game_args, legacy_arguments) = match (raw.arguments, raw.minecraft_arguments)
        {
            (Some(arguments), _) => (arguments.jvm, arguments.game, false),
            (None, Some(legacy)) => (
                Vec::new(),
                legacy
                    .split_whitespace()
                    .map(|arg| Argument::Plain(arg.to_owned()))
                    .collect(),
                true,
            ),
            (None, None) => (Vec::new(), Vec::new(), true),
        };
        let assets = raw
            .assets
            .or_else(|| raw.asset_index.as_ref().map(|index| index.id.clone()))
            .unwrap_or_else(|| "legacy".to_owned());
        Ok(Self {
            game_version: raw.id,
            version_type: raw.version_type,
            release_time: raw.release_time,
            components,
            main_class,
            libraries: raw.libraries,
            jvm_args,
            game_args,
            legacy_arguments,
            client: raw.downloads.client,
            asset_index: raw.asset_index,
            assets,
            java: raw.java_version,
            logging: raw.logging.and_then(|logging| logging.client),
            traits,
        })
    }

    /// Returns the component with this UID.
    #[must_use]
    pub fn component(&self, uid: &str) -> Option<&ComponentRef> {
        self.components
            .iter()
            .find(|component| component.uid == uid)
    }
}
