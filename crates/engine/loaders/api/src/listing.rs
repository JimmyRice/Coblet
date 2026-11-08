//! Version listings shared by data loaders and data version sources.

use std::fmt::Write as _;

use coblet_catalog::Timestamp;
use coblet_platform::glob_matches;
use coblet_types::{GameVersionId, VersionType};
use serde::Deserialize;
use serde_json::Value;

use crate::kit::{MavenMetadata, metadata_url};
use crate::{LoaderContext, LoaderError, LoaderVersion};

/// Channel given to versions a data loader marks as not stable.
const UNSTABLE: &str = "beta";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case")]
pub(crate) enum Versions {
    Json(JsonListing),
    MavenMetadata(MavenListing),
    Static(StaticListing),
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonListing {
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
pub(crate) struct MavenListing {
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
pub(crate) struct StaticListing {
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

/// Percent-encodes everything but unreserved characters, for addresses and cache keys.
pub(crate) fn encode(text: &str) -> String {
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

pub(crate) fn fill(template: &str, game: &str, version: &str) -> String {
    template
        .replace("{game}", game)
        .replace("{version}", version)
}

fn unstable(entry: LoaderVersion) -> LoaderVersion {
    entry.with_type(VersionType::Other(UNSTABLE.to_owned()))
}

impl Versions {
    /// Lists the versions for `game`, and whether the listing itself
    /// marks recommended versions.
    pub(crate) async fn list(
        &self,
        cx: &LoaderContext<'_>,
        game: &GameVersionId,
    ) -> Result<(Vec<LoaderVersion>, bool), LoaderError> {
        Ok(match self {
            Self::Json(listing) => (
                listing.versions(cx, game).await?,
                listing.recommended.is_some(),
            ),
            Self::MavenMetadata(listing) => (listing.versions(cx, game).await?, false),
            Self::Static(listing) => (
                listing.versions(game),
                listing.list.iter().any(|entry| entry.recommended),
            ),
        })
    }
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
