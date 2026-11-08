use coblet_catalog::{CatalogContext, CatalogError, CatalogPage, CatalogQuery, CatalogSource};
use coblet_runtime::{BoxFuture, Extension};
use coblet_types::ExtensionId;
use serde::Deserialize;

use crate::listing::Versions;
use crate::{LoaderContext, LoaderError, LoaderVersion};

/// Version source of a Mod Loader described by data: a TOML or JSON file
/// with an ID, a name and a `[versions]` table in the format of
/// [`DataLoader`](crate::DataLoader)'s, such as a mirror's listing or a
/// fixed list of in-house builds.
///
/// Add it to a loader with
/// [`LoaderRegistry::add_version_source`](crate::LoaderRegistry::add_version_source);
/// the loader's own versions come first. The source marks only the versions
/// its listing calls recommended: picking a default is up to the loader.
/// Its documents are cached under its own ID.
///
/// # Examples
///
/// ```
/// use coblet_loader::DataLoaderVersions;
/// use coblet_runtime::Extension;
///
/// let source = DataLoaderVersions::from_toml(r#"
///     id = "acme:nightly"
///     name = "Acme nightly builds"
///
///     [versions]
///     format = "static"
///     list = [{ id = "0.17.0+nightly.3", type = "beta" }]
/// "#)?;
/// assert_eq!(source.id().as_str(), "acme:nightly");
/// # Ok::<(), coblet_loader::LoaderError>(())
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct DataLoaderVersions {
    id: ExtensionId,
    name: String,
    versions: Versions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    name: String,
    versions: Versions,
}

impl DataLoaderVersions {
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
        let id =
            ExtensionId::new(&definition.id).map_err(|error| LoaderError::InvalidDefinition {
                name: definition.id.clone(),
                message: error.to_string(),
            })?;
        Ok(Self {
            id,
            name: definition.name,
            versions: definition.versions,
        })
    }
}

impl Extension for DataLoaderVersions {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl CatalogSource<LoaderVersion> for DataLoaderVersions {
    fn fetch<'a>(
        &'a self,
        cx: &'a CatalogContext,
        query: &'a CatalogQuery,
    ) -> BoxFuture<'a, Result<CatalogPage<LoaderVersion>, CatalogError>> {
        Box::pin(async move {
            let Some(game) = &query.game else {
                return Err(CatalogError::Failed(
                    "mod loader versions are listed per game version".to_owned(),
                ));
            };
            let loader = LoaderContext::new(cx, &self.id);
            let (mut versions, _) = self.versions.list(&loader, game).await?;
            for version in &mut versions {
                version.game.get_or_insert_with(|| game.clone());
            }
            Ok(CatalogPage::new(versions))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, CatalogQuery, CatalogSource, MetaCache};
    use coblet_runtime::TokioStorage;
    use coblet_types::{GameVersionId, VersionType};

    use super::DataLoaderVersions;
    use crate::LoaderError;
    use crate::mock::Served;

    const MIRROR: &str = r#"
        id = "acme:mirror"
        name = "Acme mirror"

        [versions]
        format = "json"
        url = "https://mirror.acme.net/loader/{game}"
        id = "/version"
        stable = "/stable"
    "#;

    #[tokio::test]
    async fn lists_json_and_static_versions_for_the_game() {
        let dir = tempfile::tempdir().unwrap();
        let served = Arc::new(Served::new([(
            "https://mirror.acme.net/loader/1.20.1",
            r#"[{ "version": "0.17.0", "stable": false }, { "version": "0.16.9", "stable": true }]"#,
        )]));
        let meta = MetaCache::new(served.clone(), Arc::new(TokioStorage::new()), dir.path());
        let cx = CatalogContext::new(meta, CachePolicy::PreferCache);
        let game: GameVersionId = "1.20.1".parse().unwrap();

        let mirror = DataLoaderVersions::from_toml(MIRROR).unwrap();
        let page = mirror
            .fetch(&cx, &CatalogQuery::for_game(game.clone()))
            .await
            .unwrap();
        let listed: Vec<(&str, &VersionType, bool)> = page
            .items
            .iter()
            .map(|version| {
                (
                    version.id.as_str(),
                    &version.version_type,
                    version.recommended,
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("0.17.0", &VersionType::Other("beta".to_owned()), false),
                ("0.16.9", &VersionType::Release, false)
            ]
        );
        assert!(
            page.items
                .iter()
                .all(|version| version.game.as_ref() == Some(&game))
        );
        assert!(
            dir.path()
                .join("loaders/acme/mirror/versions/1.20.1.json")
                .is_file()
        );

        let fixed = DataLoaderVersions::from_json(
            r#"{"id": "acme:fixed", "name": "Fixed", "versions": {"format": "static",
                "list": [{"id": "1.0", "recommended": true}, {"id": "0.9", "game": "1.12.2"}]}}"#,
        )
        .unwrap();
        let page = fixed
            .fetch(&cx, &CatalogQuery::for_game(game))
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].recommended);
        let unlisted = fixed.fetch(&cx, &CatalogQuery::all()).await.unwrap_err();
        assert!(
            unlisted.to_string().contains("per game version"),
            "{unlisted}"
        );
    }

    #[test]
    fn rejects_bad_definitions() {
        for text in [
            "id = \"acme:x\"\nname = \"X\"",
            "id = \"no-namespace\"\nname = \"X\"\n[versions]\nformat = \"static\"\nlist = []",
            "id = \"acme:x\"\nname = \"X\"\nuid = \"a.b\"\n[versions]\nformat = \"static\"\nlist = []",
        ] {
            let error = DataLoaderVersions::from_toml(text).unwrap_err();
            assert!(
                matches!(error, LoaderError::InvalidDefinition { .. }),
                "{error}"
            );
        }
        assert!(DataLoaderVersions::from_json("{}").is_err());
    }
}
