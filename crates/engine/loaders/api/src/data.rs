use std::fmt::Write as _;

use coblet_catalog::Timestamp;
use coblet_mojang::RawVersionJson;
use coblet_platform::{Component, ComponentPatch, glob_matches};
use coblet_runtime::{BoxFuture, Extension};
use coblet_types::{ExtensionId, GameVersionId, VersionType};
use serde::Deserialize;
use serde_json::Value;

use crate::kit::{MavenMetadata, SplitRule, VersionJsonImport, metadata_url};
use crate::{InstallRecipe, LoaderContext, LoaderError, LoaderVersion, ModLoader};

/// Channel given to versions a data loader marks as not stable.
const UNSTABLE: &str = "beta";

/// Mod Loader described by data instead of code: where its versions are
/// listed and what a version adds to an Instance.
///
/// Loaders driven by a metadata server, such as Fabric and its relatives,
/// need no code: a TOML or JSON file names the listing and the profile
/// address, with `{game}` and `{version}` filled in (percent-encoded in
/// addresses). It registers like any other [`ModLoader`].
///
/// # Format
///
/// ```toml
/// id = "acme:legacy-loader"
/// name = "Legacy Loader"
/// uid = "net.acme.loader"           # component that identifies an installed copy
/// accepts = ["coblet:fabric"]       # other loaders whose Mods it runs
///
/// [versions]                        # format: "json", "maven-metadata" or "static"
/// format = "json"
/// url = "https://meta.acme.net/v2/versions/loader/{game}"
/// items = ""                        # JSON pointer to the array; "" is the document
/// id = "/loader/version"            # pointer to the version string in an item
/// stable = "/loader/stable"         # optional pointer to a boolean
/// recommended = "/recommended"      # optional pointer to a boolean
/// game = "/intermediary/version"    # optional pointer: keep items for the requested game
/// released = "/released"            # optional pointer to an RFC 3339 time
///
/// [profile]                         # format: "version-json", "component", "recipe" or "static"
/// format = "version-json"
/// url = "https://meta.acme.net/v2/versions/loader/{game}/{version}/profile/json"
///
/// [[profile.split]]                 # libraries that become a component of their own
/// library = "net.acme:mappings:*"
/// uid = "net.acme.mappings"
/// name = "Acme Mappings"
/// ```
///
/// - `maven-metadata` versions read `repository`, `group` and `artifact`,
///   keep versions matching the glob `pattern` (with `{game}`, all when
///   absent) and mark those matching any glob in `unstable` as beta; the
///   newest comes first.
/// - `static` versions list `[[versions.list]]` entries with `id` and
///   optional `game`, `type` and `recommended`.
/// - `component` and `recipe` profiles fetch one component file or a whole
///   [`InstallRecipe`] in JSON; `static` profiles give `[[profile.components]]`
///   inline, in the component file format, with `{game}` and `{version}`
///   filled into every string.
///
/// Without a `recommended` pointer, the first stable version is recommended.
///
/// # Examples
///
/// ```
/// use coblet_loader::{DataLoader, ModLoader};
/// use coblet_runtime::Extension;
///
/// let loader = DataLoader::from_toml(r#"
///     id = "acme:tiny"
///     name = "Tiny"
///     uid = "net.acme.tiny"
///     [versions]
///     format = "static"
///     list = [{ id = "1.0" }]
///     [profile]
///     format = "static"
///     components = [{ uid = "net.acme.tiny", version = "{version}", mainClass = "net.acme.Main" }]
/// "#)?;
/// assert_eq!(loader.id().as_str(), "acme:tiny");
/// assert_eq!(loader.component_uid(), "net.acme.tiny");
/// # Ok::<(), coblet_loader::LoaderError>(())
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct DataLoader {
    id: ExtensionId,
    definition: Definition,
    accepts: Vec<ExtensionId>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    name: String,
    uid: String,
    #[serde(default)]
    accepts: Vec<String>,
    versions: Versions,
    profile: Profile,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case")]
enum Versions {
    Json(JsonListing),
    MavenMetadata(MavenListing),
    Static(StaticListing),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonListing {
    url: String,
    #[serde(default)]
    items: String,
    id: String,
    #[serde(default)]
    stable: Option<String>,
    #[serde(default)]
    recommended: Option<String>,
    #[serde(default)]
    game: Option<String>,
    #[serde(default)]
    released: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct MavenListing {
    repository: String,
    group: String,
    artifact: String,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    unstable: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticListing {
    list: Vec<StaticVersion>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticVersion {
    id: String,
    #[serde(default)]
    game: Option<String>,
    #[serde(default, rename = "type")]
    version_type: Option<String>,
    #[serde(default)]
    recommended: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case", deny_unknown_fields)]
enum Profile {
    VersionJson {
        url: String,
        #[serde(default)]
        split: Vec<SplitRule>,
    },
    Component {
        url: String,
    },
    Recipe {
        url: String,
    },
    Static {
        components: Vec<Value>,
    },
}

/// Percent-encodes everything but unreserved characters, for addresses and cache keys.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn fill(template: &str, game: &str, version: &str) -> String {
    template
        .replace("{game}", game)
        .replace("{version}", version)
}

fn fill_value(value: &mut Value, game: &str, version: &str) {
    match value {
        Value::String(text) => *text = fill(text, game, version),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| fill_value(item, game, version)),
        Value::Object(map) => map
            .values_mut()
            .for_each(|item| fill_value(item, game, version)),
        _ => {}
    }
}

fn unstable(entry: LoaderVersion) -> LoaderVersion {
    entry.with_type(VersionType::Other(UNSTABLE.to_owned()))
}

impl JsonListing {
    async fn versions(
        &self,
        cx: &LoaderContext<'_>,
        game: &GameVersionId,
    ) -> Result<Vec<LoaderVersion>, LoaderError> {
        let game = game.as_str();
        let address = fill(&self.url, &encode(game), "");
        let document: Value = cx
            .fetch_json(&address, &format!("versions/{}.json", encode(game)))
            .await?;
        let invalid =
            |message: String| LoaderError::invalid(cx.loader(), format!("{address}: {message}"));
        let list = document
            .pointer(&self.items)
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("no array at `{}`", self.items)))?;
        let mut versions = Vec::new();
        for item in list {
            let text = |pointer: &Option<String>| {
                pointer
                    .as_deref()
                    .and_then(|pointer| item.pointer(pointer))
                    .and_then(Value::as_str)
            };
            let flag = |pointer: &Option<String>| {
                pointer
                    .as_deref()
                    .and_then(|pointer| item.pointer(pointer))
                    .and_then(Value::as_bool)
            };
            if self.game.is_some() && text(&self.game) != Some(game) {
                continue;
            }
            let id = item
                .pointer(&self.id)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("an item has no `{}`", self.id)))?;
            let mut entry = LoaderVersion::new(id);
            if flag(&self.stable) == Some(false) {
                entry = unstable(entry);
            }
            entry.recommended = flag(&self.recommended) == Some(true);
            entry.released = text(&self.released).and_then(|time| time.parse::<Timestamp>().ok());
            versions.push(entry);
        }
        Ok(versions)
    }
}

impl MavenListing {
    async fn versions(
        &self,
        cx: &LoaderContext<'_>,
        game: &GameVersionId,
    ) -> Result<Vec<LoaderVersion>, LoaderError> {
        let address = metadata_url(&self.repository, &self.group, &self.artifact);
        let body = cx.fetch(&address, "maven-metadata.xml").await?;
        let metadata = MavenMetadata::parse(&String::from_utf8_lossy(&body))
            .map_err(|error| LoaderError::invalid(cx.loader(), format!("{address}: {error}")))?;
        let pattern = self
            .pattern
            .as_deref()
            .map(|pattern| fill(pattern, game.as_str(), ""));
        Ok(metadata
            .versions
            .iter()
            .rev()
            .filter(|version| {
                pattern
                    .as_deref()
                    .is_none_or(|pattern| glob_matches(pattern, version))
            })
            .map(|version| {
                let entry = LoaderVersion::new(version.as_str());
                if self.unstable.iter().any(|glob| glob_matches(glob, version)) {
                    unstable(entry)
                } else {
                    entry
                }
            })
            .collect())
    }
}

impl StaticListing {
    fn versions(&self, game: &GameVersionId) -> Vec<LoaderVersion> {
        self.list
            .iter()
            .filter(|entry| entry.game.as_deref().is_none_or(|own| own == game.as_str()))
            .map(|entry| LoaderVersion {
                recommended: entry.recommended,
                version_type: entry
                    .version_type
                    .as_deref()
                    .map_or(VersionType::Release, VersionType::from),
                ..LoaderVersion::new(entry.id.as_str())
            })
            .collect()
    }
}

impl DataLoader {
    /// Parses a definition in TOML.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::InvalidDefinition`] for malformed files, unknown keys or a bad ID.
    pub fn from_toml(text: &str) -> Result<Self, LoaderError> {
        let definition: Definition =
            toml::from_str(text).map_err(|error| LoaderError::InvalidDefinition {
                name: "<toml>".to_owned(),
                message: error.message().to_owned(),
            })?;
        Self::new(definition)
    }

    /// Parses a definition in JSON, with the same keys as the TOML form.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::InvalidDefinition`] for malformed files, unknown keys or a bad ID.
    pub fn from_json(text: &str) -> Result<Self, LoaderError> {
        let definition: Definition =
            serde_json::from_str(text).map_err(|error| LoaderError::InvalidDefinition {
                name: "<json>".to_owned(),
                message: error.to_string(),
            })?;
        Self::new(definition)
    }

    fn new(definition: Definition) -> Result<Self, LoaderError> {
        let invalid = |message: String| LoaderError::InvalidDefinition {
            name: definition.id.clone(),
            message,
        };
        let id = ExtensionId::new(&definition.id).map_err(|error| invalid(error.to_string()))?;
        let accepts = definition
            .accepts
            .iter()
            .map(|accepted| ExtensionId::new(accepted).map_err(|error| invalid(error.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        if let Profile::Static { components } = &definition.profile {
            for component in components {
                serde_json::from_value::<Component>(component.clone())
                    .map_err(|error| invalid(format!("static component: {error}")))?;
            }
        }
        Ok(Self {
            id,
            definition,
            accepts,
        })
    }

    async fn list(
        &self,
        cx: &LoaderContext<'_>,
        game: &GameVersionId,
    ) -> Result<Vec<LoaderVersion>, LoaderError> {
        let (mut versions, flagged) = match &self.definition.versions {
            Versions::Json(listing) => (
                listing.versions(cx, game).await?,
                listing.recommended.is_some(),
            ),
            Versions::MavenMetadata(listing) => (listing.versions(cx, game).await?, false),
            Versions::Static(listing) => (
                listing.versions(game),
                listing.list.iter().any(|entry| entry.recommended),
            ),
        };
        if !flagged
            && let Some(first) = versions
                .iter_mut()
                .find(|version| version.version_type == VersionType::Release)
        {
            first.recommended = true;
        }
        Ok(versions)
    }

    async fn recipe(
        &self,
        cx: &LoaderContext<'_>,
        game: &GameVersionId,
        version: &LoaderVersion,
    ) -> Result<InstallRecipe, LoaderError> {
        let (game_text, id) = (game.as_str(), version.id.as_str());
        let address = |url: &str| fill(url, &encode(game_text), &encode(id));
        let key = |kind: &str| format!("{kind}/{}/{}.json", encode(game_text), encode(id));
        let parse = |what: &str, error: serde_json::Error| {
            LoaderError::invalid(&self.id, format!("{what}: {error}"))
        };
        match &self.definition.profile {
            Profile::VersionJson { url, split } => {
                let address = address(url);
                let body = cx.fetch(&address, &key("profiles")).await?;
                let raw =
                    RawVersionJson::from_slice(&body).map_err(|error| parse(&address, error))?;
                let mut import =
                    VersionJsonImport::new(&self.definition.uid, id).named(&self.definition.name);
                import.splits.clone_from(split);
                Ok(InstallRecipe::new(import.components(raw)))
            }
            Profile::Component { url } => {
                let address = address(url);
                let body = cx.fetch(&address, &key("components")).await?;
                let patch =
                    ComponentPatch::from_slice(&body).map_err(|error| parse(&address, error))?;
                Ok(InstallRecipe::new(vec![Component::new(patch)]))
            }
            Profile::Recipe { url } => {
                let address = address(url);
                let body = cx.fetch(&address, &key("recipes")).await?;
                InstallRecipe::from_json(&body).map_err(|error| parse(&address, error))
            }
            Profile::Static { components } => {
                let components = components
                    .iter()
                    .map(|component| {
                        let mut value = component.clone();
                        fill_value(&mut value, game_text, id);
                        serde_json::from_value(value)
                            .map_err(|error| parse("static component", error))
                    })
                    .collect::<Result<Vec<Component>, _>>()?;
                Ok(InstallRecipe::new(components))
            }
        }
    }
}

impl Extension for DataLoader {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.definition.name
    }
}

impl ModLoader for DataLoader {
    fn component_uid(&self) -> &str {
        &self.definition.uid
    }

    fn versions<'a>(
        &'a self,
        cx: &'a LoaderContext<'a>,
        game: &'a GameVersionId,
    ) -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>> {
        Box::pin(self.list(cx, game))
    }

    fn install<'a>(
        &'a self,
        cx: &'a LoaderContext<'a>,
        game: &'a GameVersionId,
        version: &'a LoaderVersion,
    ) -> BoxFuture<'a, Result<InstallRecipe, LoaderError>> {
        Box::pin(self.recipe(cx, game, version))
    }

    fn accepted_content_loaders(&self) -> Vec<ExtensionId> {
        let mut accepted = vec![self.id.clone()];
        accepted.extend(self.accepts.iter().cloned());
        accepted
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_runtime::{Extension, TokioStorage};
    use coblet_types::{ExtensionId, GameVersionId, VersionType};

    use coblet_platform::Component;

    use super::{DataLoader, encode};
    use crate::mock::Served;
    use crate::{LoaderContext, LoaderError, LoaderVersion, ModLoader};

    const FABRIC_LIKE: &str = r#"
        id = "acme:fabric-like"
        name = "Fabric-like Loader"
        uid = "net.acme.loader"
        accepts = ["coblet:fabric"]

        [versions]
        format = "json"
        url = "https://meta.acme.net/v2/versions/loader/{game}"
        id = "/loader/version"
        stable = "/loader/stable"

        [profile]
        format = "version-json"
        url = "https://meta.acme.net/v2/versions/loader/{game}/{version}/profile/json"

        [[profile.split]]
        library = "net.acme:intermediary:*"
        uid = "net.acme.intermediary"
        name = "Intermediary"
    "#;

    const LISTING: &str = r#"[
        { "loader": { "version": "0.17.0-beta.1", "stable": false } },
        { "loader": { "version": "0.16.9", "stable": true } },
        { "loader": { "version": "0.16.8", "stable": true } }
    ]"#;

    const PROFILE: &str = r#"{
        "id": "acme-loader-0.16.9-1.21 Pre-Release 1",
        "inheritsFrom": "1.21 Pre-Release 1",
        "mainClass": "net.acme.Knot",
        "libraries": [
            { "name": "net.acme:intermediary:1.21-pre1", "url": "https://maven.acme.net/" },
            { "name": "net.acme:loader:0.16.9", "url": "https://maven.acme.net/" }
        ]
    }"#;

    fn context(dir: &std::path::Path, served: Served) -> (Arc<Served>, CatalogContext) {
        let served = Arc::new(served);
        let meta = MetaCache::new(served.clone(), Arc::new(TokioStorage::new()), dir);
        (served, CatalogContext::new(meta, CachePolicy::PreferCache))
    }

    #[tokio::test]
    async fn lists_and_installs_a_meta_driven_loader() {
        let dir = tempfile::tempdir().unwrap();
        let (served, catalog) = context(
            dir.path(),
            Served::new([
                (
                    "https://meta.acme.net/v2/versions/loader/1.21%20Pre-Release%201",
                    LISTING,
                ),
                (
                    "https://meta.acme.net/v2/versions/loader/1.21%20Pre-Release%201/0.16.9/profile/json",
                    PROFILE,
                ),
            ]),
        );
        let loader = DataLoader::from_toml(FABRIC_LIKE).unwrap();
        let cx = LoaderContext::new(&catalog, loader.id());
        let game: GameVersionId = "1.21 Pre-Release 1".parse().unwrap();

        let versions = loader.versions(&cx, &game).await.unwrap();
        let summary: Vec<(&str, bool, bool)> = versions
            .iter()
            .map(|version| {
                (
                    version.id.as_str(),
                    version.version_type == VersionType::Release,
                    version.recommended,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("0.17.0-beta.1", false, false),
                ("0.16.9", true, true),
                ("0.16.8", true, false)
            ]
        );

        let recipe = loader.install(&cx, &game, &versions[1]).await.unwrap();
        let uids: Vec<&str> = recipe.components.iter().map(Component::uid).collect();
        assert_eq!(uids, ["net.acme.intermediary", "net.acme.loader"]);
        assert_eq!(
            recipe.components[1].patch.name.as_deref(),
            Some("Fabric-like Loader")
        );
        assert_eq!(served.asked().len(), 2);
        assert!(
            dir.path()
                .join("loaders/acme/fabric-like/profiles/1.21%20Pre-Release%201/0.16.9.json")
                .is_file()
        );
        let accepted: Vec<String> = loader
            .accepted_content_loaders()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(accepted, ["acme:fabric-like", "coblet:fabric"]);
    }

    #[tokio::test]
    async fn maven_listings_filter_by_pattern_and_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let (_, catalog) = context(
            dir.path(),
            Served::new([(
                "https://maven.acme.net/net/acme/forge-like/maven-metadata.xml",
                "<metadata><versioning><versions>\
                 <version>1.20.1-1.0</version><version>1.20.1-1.1-beta</version>\
                 <version>1.20.2-1.0</version><version>1.20.1-1.2</version>\
                 </versions></versioning></metadata>",
            )]),
        );
        let loader = DataLoader::from_json(
            r#"{
                "id": "acme:forge-like", "name": "Forge-like", "uid": "net.acme.forge",
                "versions": { "format": "maven-metadata", "repository": "https://maven.acme.net/",
                              "group": "net.acme", "artifact": "forge-like",
                              "pattern": "{game}-*", "unstable": ["*-beta"] },
                "profile": { "format": "recipe", "url": "https://maven.acme.net/recipes/{version}.json" }
            }"#,
        )
        .unwrap();
        let cx = LoaderContext::new(&catalog, loader.id());
        let versions = loader
            .versions(&cx, &"1.20.1".parse().unwrap())
            .await
            .unwrap();
        let ids: Vec<(&str, bool)> = versions
            .iter()
            .map(|version| (version.id.as_str(), version.recommended))
            .collect();
        assert_eq!(
            ids,
            [
                ("1.20.1-1.2", true),
                ("1.20.1-1.1-beta", false),
                ("1.20.1-1.0", false)
            ]
        );
    }

    #[tokio::test]
    async fn static_loaders_fill_in_the_versions() {
        let dir = tempfile::tempdir().unwrap();
        let (served, catalog) = context(dir.path(), Served::new([]));
        let loader = DataLoader::from_toml(
            r#"
            id = "acme:static"
            name = "Static"
            uid = "net.acme.static"
            [versions]
            format = "static"
            list = [
                { id = "2.0", type = "snapshot" },
                { id = "1.0", game = "1.20.1" },
                { id = "0.9", game = "1.19.4" },
            ]
            [profile]
            format = "static"
            [[profile.components]]
            uid = "net.acme.static"
            version = "{version}"
            requires = [{ uid = "net.minecraft", equals = "{game}" }]
            libraries = [{ name = "net.acme:static:{version}", url = "https://maven.acme.net/" }]
            "#,
        )
        .unwrap();
        let cx = LoaderContext::new(&catalog, loader.id());
        let game: GameVersionId = "1.20.1".parse().unwrap();
        let versions = loader.versions(&cx, &game).await.unwrap();
        assert_eq!(versions.len(), 2);
        assert!(versions[1].recommended);
        let recipe = loader
            .install(&cx, &game, &LoaderVersion::new("1.0"))
            .await
            .unwrap();
        let patch = &recipe.components[0].patch;
        assert_eq!(patch.libraries[0].name.to_string(), "net.acme:static:1.0");
        assert_eq!(patch.requires[0].equals.as_deref(), Some("1.20.1"));
        assert_eq!(served.asked(), Vec::<String>::new());
    }

    #[test]
    fn rejects_bad_definitions() {
        let unknown_key = DataLoader::from_toml(
            r#"
            id = "acme:x"
            name = "X"
            uid = "x"
            colour = "red"
            [versions]
            format = "static"
            list = []
            [profile]
            format = "static"
            components = []
            "#,
        );
        assert!(matches!(
            unknown_key,
            Err(LoaderError::InvalidDefinition { .. })
        ));
        let bad_id = DataLoader::from_json(
            r#"{ "id": "no namespace", "name": "X", "uid": "x",
                 "versions": { "format": "static", "list": [] },
                 "profile": { "format": "static", "components": [] } }"#,
        )
        .unwrap_err();
        assert!(bad_id.to_string().contains("no namespace"), "{bad_id}");
        let bad_component = DataLoader::from_json(
            r#"{ "id": "acme:x", "name": "X", "uid": "x",
                 "versions": { "format": "static", "list": [] },
                 "profile": { "format": "static", "components": [{ "uid": "x" }] } }"#,
        )
        .unwrap_err();
        assert!(
            bad_component.to_string().contains("static component"),
            "{bad_component}"
        );
        assert_eq!(ExtensionId::new("acme:x").unwrap().as_str(), "acme:x");
        assert_eq!(encode("1.21 Pre-Release 1"), "1.21%20Pre-Release%201");
    }
}
