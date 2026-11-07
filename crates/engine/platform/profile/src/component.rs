use std::collections::BTreeMap;

use coblet_mojang::{Arguments, AssetIndexRef, Library, VersionTrait};
use serde::{Deserialize, Serialize};

/// What one component contributes to the launch: a component file in the
/// format Prism Launcher and MultiMC use for `patches/<uid>.json` and their
/// metadata server.
///
/// Mod Loaders, mapping layers, LWJGL and hand-written patches are all
/// described this way, so the launch layer never needs to know which Mod
/// Loader it runs. Fields the engine does not interpret, such as `order` or
/// `formatVersion`, are kept in [`extra`](ComponentPatch::extra) and written
/// back unchanged.
///
/// One field goes beyond the Prism format: [`arguments`](ComponentPatch::arguments)
/// takes Mojang-style argument lists with rules, which is what Mod Loader
/// metadata in the `client.json` format carries.
///
/// # Examples
///
/// ```
/// use coblet_platform::ComponentPatch;
///
/// let patch: ComponentPatch = serde_json::from_str(r#"{
///     "formatVersion": 1,
///     "uid": "net.fabricmc.fabric-loader",
///     "version": "0.16.9",
///     "requires": [{ "uid": "net.fabricmc.intermediary" }],
///     "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
///     "libraries": [{ "name": "net.fabricmc:fabric-loader:0.16.9", "url": "https://maven.fabricmc.net/" }]
/// }"#)?;
/// assert_eq!(patch.requires[0].uid, "net.fabricmc.intermediary");
/// assert!(patch.extra.contains_key("formatVersion"));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentPatch {
    /// Component UID, such as `net.fabricmc.fabric-loader`.
    pub uid: String,
    /// Component version.
    pub version: String,
    /// Name shown to users, such as `Fabric Loader`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Components that must be in the stack, optionally at an exact version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,
    /// Components that must not be in the stack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<Requirement>,
    /// Replaces the main class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_class: Option<String>,
    /// Libraries put on the classpath; one with the same group, artifact and
    /// classifier as an earlier library replaces it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<Library>,
    /// More libraries, applied after [`libraries`](ComponentPatch::libraries) the same way.
    #[serde(default, rename = "+libraries", skip_serializing_if = "Vec::is_empty")]
    pub plus_libraries: Vec<Library>,
    /// Files downloaded into the libraries directory but kept off the
    /// classpath, such as the inputs of a Mod Loader installer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maven_files: Vec<Library>,
    /// Launchwrapper tweak classes, passed as `--tweakClass`.
    #[serde(default, rename = "+tweakers", skip_serializing_if = "Vec::is_empty")]
    pub tweakers: Vec<String>,
    /// JVM arguments appended after those of earlier components.
    #[serde(default, rename = "+jvmArgs", skip_serializing_if = "Vec::is_empty")]
    pub jvm_args: Vec<String>,
    /// Replaces the game arguments with this pre-1.13 argument string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minecraft_arguments: Option<String>,
    /// Mojang-style JVM and game arguments, appended with their rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Arguments>,
    /// Archives merged into the client jar, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jar_mods: Vec<Library>,
    /// Java agents, passed as `-javaagent`.
    #[serde(default, rename = "+agents", skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<Agent>,
    /// Version traits the component adds, such as `legacy-fml-libs`.
    #[serde(default, rename = "+traits", skip_serializing_if = "Vec::is_empty")]
    pub traits: Vec<VersionTrait>,
    /// Java major versions the component runs on; empty when it does not say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatible_java_majors: Vec<u32>,
    /// Replaces the asset index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_index: Option<AssetIndexRef>,
    /// Fields the engine does not interpret, kept verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl ComponentPatch {
    /// Creates a patch that changes nothing.
    #[must_use]
    pub fn new(uid: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            version: version.into(),
            name: None,
            requires: Vec::new(),
            conflicts: Vec::new(),
            main_class: None,
            libraries: Vec::new(),
            plus_libraries: Vec::new(),
            maven_files: Vec::new(),
            tweakers: Vec::new(),
            jvm_args: Vec::new(),
            minecraft_arguments: None,
            arguments: None,
            jar_mods: Vec::new(),
            agents: Vec::new(),
            traits: Vec::new(),
            compatible_java_majors: Vec::new(),
            asset_index: None,
            extra: BTreeMap::new(),
        }
    }

    /// Parses a component file.
    ///
    /// # Errors
    ///
    /// Returns the parser's error when the JSON is malformed or lacks `uid` or `version`.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Reference from one component to another in
/// [`requires`](ComponentPatch::requires) or [`conflicts`](ComponentPatch::conflicts).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// UID of the other component.
    pub uid: String,
    /// Exact version the other component must have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    /// Version to install when the other component is missing; not a constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggests: Option<String>,
    /// Fields the engine does not interpret, kept verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Requirement {
    /// Refers to a component at any version.
    #[must_use]
    pub fn new(uid: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            equals: None,
            suggests: None,
            extra: BTreeMap::new(),
        }
    }

    /// Requires an exact version.
    #[must_use]
    pub fn equals(self, version: impl Into<String>) -> Self {
        Self {
            equals: Some(version.into()),
            ..self
        }
    }
}

/// Java agent of a component: a library passed as `-javaagent:<jar>[=<argument>]`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    /// Text after `=` in the `-javaagent` option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
    /// The agent jar.
    #[serde(flatten)]
    pub library: Library,
}

/// Entry of a component stack: a [`ComponentPatch`] and how the stack treats it.
///
/// The flags use the names of Prism's `mmc-pack.json`, so a component
/// serializes as its patch with `important` and `dependencyOnly` beside the
/// other fields.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    /// The component cannot be removed, like the game itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub important: bool,
    /// The component is only there because another one requires it, like a
    /// mapping layer; removing its dependents removes it too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dependency_only: bool,
    /// What it contributes.
    #[serde(flatten)]
    pub patch: ComponentPatch,
}

impl Component {
    /// Wraps a patch as an ordinary component.
    #[must_use]
    pub fn new(patch: ComponentPatch) -> Self {
        Self {
            important: false,
            dependency_only: false,
            patch,
        }
    }

    /// Marks the component as present only for its dependents.
    #[must_use]
    pub fn dependency_only(self) -> Self {
        Self {
            dependency_only: true,
            ..self
        }
    }

    /// Returns the UID.
    #[must_use]
    pub fn uid(&self) -> &str {
        &self.patch.uid
    }

    /// Returns the version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.patch.version
    }
}

#[cfg(test)]
mod tests {
    use super::{Component, ComponentPatch};

    const FORGE_1_12: &str = r#"{
        "formatVersion": 1,
        "uid": "net.minecraftforge",
        "version": "14.23.5.2860",
        "name": "Forge",
        "order": 5,
        "requires": [{ "uid": "net.minecraft", "equals": "1.12.2" }],
        "mainClass": "net.minecraft.launchwrapper.Launch",
        "+tweakers": ["net.minecraftforge.fml.common.launcher.FMLTweaker"],
        "+traits": ["legacy-fml-libs", "FirstThreadOnMacOS"],
        "compatibleJavaMajors": [8],
        "libraries": [{ "name": "net.minecraftforge:forge:1.12.2-14.23.5.2860", "MMC-hint": "local" }],
        "+agents": [{ "name": "acme:agent:1", "url": "https://maven.example/", "argument": "x=1" }]
    }"#;

    #[test]
    fn round_trips_unknown_fields() {
        let patch = ComponentPatch::from_slice(FORGE_1_12.as_bytes()).unwrap();
        assert_eq!(patch.tweakers.len(), 1);
        assert_eq!(patch.compatible_java_majors, [8]);
        assert_eq!(patch.requires[0].equals.as_deref(), Some("1.12.2"));
        assert_eq!(patch.agents[0].argument.as_deref(), Some("x=1"));
        assert_eq!(patch.agents[0].library.name.to_string(), "acme:agent:1");
        assert_eq!(patch.traits[0].as_str(), "legacy-fml-libs");
        assert_eq!(patch.traits[1].as_str(), "FirstThreadOnMacOS");
        assert!(patch.libraries[0].extra.contains_key("MMC-hint"));

        let written: serde_json::Value = serde_json::to_value(&patch).unwrap();
        let original: serde_json::Value = serde_json::from_str(FORGE_1_12).unwrap();
        assert_eq!(written, original);
    }

    #[test]
    fn component_flags_sit_beside_the_patch() {
        let component: Component = serde_json::from_str(
            r#"{ "uid": "net.fabricmc.intermediary", "version": "1.20.1", "dependencyOnly": true }"#,
        )
        .unwrap();
        assert!(component.dependency_only);
        assert!(!component.important);
        assert!(component.patch.extra.is_empty());
        let json = serde_json::to_string(&component).unwrap();
        assert!(json.contains(r#""dependencyOnly":true"#));
        assert!(!json.contains("important"));
    }
}
