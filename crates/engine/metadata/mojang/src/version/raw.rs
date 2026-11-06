use std::collections::BTreeMap;

use coblet_catalog::Timestamp;
use coblet_types::{GameVersionId, MavenCoordinate, Sha1Digest, VersionType};
use serde::{Deserialize, Serialize};

use super::Rule;

/// Parsed `client.json`: the version JSON format of Mojang and of Mod Loader patches.
///
/// It is the raw input of the Profile pipeline and keeps every rule
/// unevaluated. Fields the engine does not use are kept in
/// [`extra`](RawVersionJson::extra) so nothing is lost when a file is
/// written back. Mapping downloads and most other fields are optional,
/// because version 26.1 and later have no mappings and Mod Loader patches
/// carry only what they change.
///
/// # Examples
///
/// ```
/// use coblet_mojang::RawVersionJson;
///
/// let raw = RawVersionJson::from_slice(br#"{
///     "id": "b1.7.3",
///     "type": "old_beta",
///     "mainClass": "net.minecraft.launchwrapper.Launch",
///     "minecraftArguments": "${auth_player_name} ${auth_session}",
///     "libraries": []
/// }"#)?;
/// assert_eq!(raw.main_class.as_deref(), Some("net.minecraft.launchwrapper.Launch"));
/// assert!(raw.arguments.is_none());
/// # Ok::<(), serde_json::Error>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawVersionJson {
    /// Version ID, such as `1.20.1` or `fabric-loader-0.16.14-1.20.1`.
    pub id: GameVersionId,
    /// Release channel.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub version_type: Option<VersionType>,
    /// Version this file extends; its fields are merged first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherits_from: Option<GameVersionId>,
    /// Java class with the `main` method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_class: Option<String>,
    /// Arguments of versions 1.13 and later, with rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Arguments>,
    /// Space-separated game arguments of versions before 1.13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minecraft_arguments: Option<String>,
    /// Libraries, in classpath order.
    #[serde(default)]
    pub libraries: Vec<Library>,
    /// Asset index to download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_index: Option<AssetIndexRef>,
    /// Asset index ID, such as `pre-1.6`, `legacy` or `17`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<String>,
    /// Game jars and mappings.
    #[serde(default, skip_serializing_if = "Downloads::is_empty")]
    pub downloads: Downloads,
    /// Java runtime the version was built for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_version: Option<JavaVersion>,
    /// Log configuration passed to the game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Logging>,
    /// Release time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_time: Option<Timestamp>,
    /// Last modification time of the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Timestamp>,
    /// Safety level the official launcher shows warnings for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compliance_level: Option<u32>,
    /// Oldest official launcher format version that understands this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_launcher_version: Option<u32>,
    /// Fields the engine does not interpret, kept verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl RawVersionJson {
    /// Parses a version JSON document.
    ///
    /// # Errors
    ///
    /// Returns the [`serde_json::Error`] of malformed documents.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Returns the `--tweakClass` values of the arguments, in order.
    #[must_use]
    pub fn tweak_classes(&self) -> Vec<&str> {
        let legacy = self
            .minecraft_arguments
            .iter()
            .flat_map(|arguments| arguments.split_whitespace());
        let modern = self
            .arguments
            .iter()
            .flat_map(|arguments| arguments.game.iter().flat_map(Argument::values));
        let all: Vec<&str> = legacy.chain(modern).collect();
        all.windows(2)
            .filter(|pair| pair[0] == "--tweakClass")
            .map(|pair| pair[1])
            .collect()
    }
}

/// Arguments of versions 1.13 and later.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Arguments {
    /// Arguments after the main class.
    #[serde(default)]
    pub game: Vec<Argument>,
    /// Arguments before the main class.
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

/// One argument entry: a plain string or values guarded by rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    /// Always passed.
    Plain(String),
    /// Passed when the rules allow it.
    Conditional {
        /// The rules.
        rules: Vec<Rule>,
        /// One value or several.
        value: ArgumentValue,
    },
}

impl Argument {
    /// Iterates over the values regardless of rules.
    pub fn values(&self) -> impl Iterator<Item = &str> {
        let values: &[String] = match self {
            Self::Plain(value) => std::slice::from_ref(value),
            Self::Conditional { value, .. } => value.as_slice(),
        };
        values.iter().map(String::as_str)
    }
}

/// Value of a conditional argument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgumentValue {
    /// A single argument.
    One(String),
    /// Several arguments.
    Many(Vec<String>),
}

impl ArgumentValue {
    /// Returns the arguments as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

/// Library entry: a Maven coordinate with downloads, natives and rules.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Library {
    /// Maven coordinate, such as `org.lwjgl:lwjgl:3.3.1:natives-macos-arm64`.
    pub name: MavenCoordinate,
    /// Files to download; Mod Loader libraries may give only a repository [`url`](Library::url).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<LibraryDownloads>,
    /// Base URL of the Maven repository, used when `downloads` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Classifier of the native jar per Mojang OS name (versions before 1.19);
    /// may contain `${arch}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub natives: BTreeMap<String, String>,
    /// Paths left out when native jars are extracted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<Extract>,
    /// Rules that decide whether the library is used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    /// Fields the engine does not interpret, kept verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Downloads of a library.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryDownloads {
    /// The main jar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Artifact>,
    /// Extra jars by classifier, used for native jars before 1.19.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub classifiers: BTreeMap<String, Artifact>,
}

/// File inside the `libraries` directory.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Path relative to the `libraries` directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// SHA-1 of the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<Sha1Digest>,
    /// Size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Download address; empty in some Mod Loader files whose jar is produced locally.
    #[serde(default)]
    pub url: String,
}

/// Extraction settings of a native jar.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extract {
    /// Path prefixes to leave out, such as `META-INF/`.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Game jars and mapping files of a version.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Downloads {
    /// The client jar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Download>,
    /// The dedicated server jar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Download>,
    /// Official client mappings; absent from 26.1 on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_mappings: Option<Download>,
    /// Official server mappings; absent from 26.1 on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_mappings: Option<Download>,
    /// Other downloads, such as `windows_server`, kept verbatim.
    #[serde(flatten)]
    pub other: BTreeMap<String, Download>,
}

impl Downloads {
    /// Reports whether there are no downloads.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.client.is_none()
            && self.server.is_none()
            && self.client_mappings.is_none()
            && self.server_mappings.is_none()
            && self.other.is_empty()
    }
}

/// Downloadable file with its SHA-1 and size.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    /// SHA-1 of the file.
    pub sha1: Sha1Digest,
    /// Size in bytes.
    pub size: u64,
    /// Download address.
    pub url: String,
}

/// Reference to the asset index of a version.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    /// Index ID, such as `pre-1.6` or `17`.
    pub id: String,
    /// SHA-1 of the index file.
    pub sha1: Sha1Digest,
    /// Size of the index file.
    pub size: u64,
    /// Total size of all objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_size: Option<u64>,
    /// Download address of the index file.
    pub url: String,
}

/// Java runtime a version was built for.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    /// Runtime component in the Mojang Java runtime index, such as `jre-legacy` or `java-runtime-delta`.
    pub component: String,
    /// Java major version, such as 8 or 21.
    pub major_version: u32,
}

/// Log configuration of a version.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logging {
    /// Configuration for the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<LoggingConfig>,
}

/// Log4j configuration file and the JVM argument that loads it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Argument template, such as `-Dlog4j.configurationFile=${path}`.
    pub argument: String,
    /// The configuration file.
    pub file: LoggingFile,
    /// Format of the file, such as `log4j2-xml`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// Log configuration file.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingFile {
    /// File name, such as `client-1.12.xml`.
    pub id: String,
    /// SHA-1 of the file.
    pub sha1: Sha1Digest,
    /// Size in bytes.
    pub size: u64,
    /// Download address.
    pub url: String,
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use crate::{Argument, Condition, RawVersionJson, RuleContext, evaluate};

    pub(crate) fn fixture_bytes(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/../../../../fixtures/mojang/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    fn fixture(name: &str) -> RawVersionJson {
        RawVersionJson::from_slice(&fixture_bytes(&format!("{name}.json"))).unwrap()
    }

    const FIXTURES: [&str; 9] = [
        "26.3",
        "1.21.11",
        "1.20.1",
        "1.13.2",
        "1.12.2",
        "1.5.2",
        "b1.7.3",
        "a1.2.6",
        "c0.30_01c",
    ];

    #[test]
    fn parses_and_round_trips_every_fixture() {
        for name in FIXTURES {
            let raw = fixture(name);
            assert_eq!(raw.id.as_str(), name);
            assert!(raw.main_class.is_some(), "{name}");
            assert!(raw.downloads.client.is_some(), "{name}");
            assert!(raw.asset_index.is_some(), "{name}");
            assert!(raw.java_version.is_some(), "{name}");
            assert!(
                raw.arguments.is_some() != raw.minecraft_arguments.is_some(),
                "{name} has exactly one argument style"
            );
            let json = serde_json::to_vec(&raw).unwrap();
            assert_eq!(RawVersionJson::from_slice(&json).unwrap(), raw, "{name}");
        }
    }

    #[test]
    fn mapping_downloads_are_optional() {
        let modern = fixture("26.3");
        assert!(modern.downloads.client_mappings.is_none());
        assert!(modern.downloads.server_mappings.is_none());
        assert_eq!(
            modern.java_version.unwrap().component,
            "java-runtime-epsilon"
        );
        let obfuscated = fixture("1.20.1");
        assert!(obfuscated.downloads.client_mappings.is_some());
    }

    #[test]
    fn reads_legacy_arguments_and_natives() {
        let beta = fixture("b1.7.3");
        assert_eq!(
            beta.main_class.as_deref(),
            Some("net.minecraft.launchwrapper.Launch")
        );
        assert_eq!(beta.assets.as_deref(), Some("pre-1.6"));
        assert_eq!(beta.tweak_classes(), Vec::<&str>::new());
        let natives = beta
            .libraries
            .iter()
            .find(|library| !library.natives.is_empty())
            .unwrap();
        assert_eq!(natives.natives["osx"], "natives-osx");
        assert_eq!(natives.extract.as_ref().unwrap().exclude, ["META-INF/"]);
        let classic = fixture("c0.30_01c");
        assert_eq!(
            classic.tweak_classes(),
            ["net.minecraft.launchwrapper.AlphaVanillaTweaker"]
        );
    }

    #[test]
    fn evaluates_fixture_rules_per_platform() {
        let raw = fixture("1.20.1");
        let arguments = raw.arguments.as_ref().unwrap();
        let mac = RuleContext::new("osx", "arm64").with_os_version("15.5");
        let linux = RuleContext::new("linux", "x86_64");
        let start_on_first_thread = arguments
            .jvm
            .iter()
            .find_map(|argument| match argument {
                Argument::Conditional { rules, value }
                    if value.as_slice() == ["-XstartOnFirstThread"] =>
                {
                    Some(rules)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(evaluate(start_on_first_thread, &mac), Condition::Always);
        assert_eq!(evaluate(start_on_first_thread, &linux), Condition::Never);

        let demo = arguments
            .game
            .iter()
            .find_map(|argument| match argument {
                Argument::Conditional { rules, value } if value.as_slice() == ["--demo"] => {
                    Some(rules)
                }
                _ => None,
            })
            .unwrap();
        let condition = evaluate(demo, &linux);
        assert!(!condition.is_allowed_by_default());
        assert!(condition.allows(&BTreeMap::from([("is_demo_user".to_owned(), true)])));

        let macos_natives: Vec<&str> = raw
            .libraries
            .iter()
            .filter(|library| evaluate(&library.rules, &mac).is_allowed_by_default())
            .filter_map(|library| library.name.classifier())
            .collect();
        assert!(
            macos_natives.contains(&"natives-macos-arm64"),
            "{macos_natives:?}"
        );
        assert!(
            !macos_natives
                .iter()
                .any(|c| c.contains("linux") || c.contains("windows"))
        );
    }

    #[test]
    fn keeps_unknown_fields() {
        let raw = RawVersionJson::from_slice(
            br#"{"id":"x","libraries":[{"name":"a:b:1","acme":1}],"_comment":"kept"}"#,
        )
        .unwrap();
        assert_eq!(raw.extra["_comment"], "kept");
        assert_eq!(raw.libraries[0].extra["acme"], 1);
    }
}
