use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use coblet_catalog::{
    Catalog, CatalogContext, CatalogError, CatalogPage, CatalogQuery, CatalogSource, MetaCache,
};
use coblet_platform::ComponentRef;
use coblet_runtime::{BoxFuture, Extension, Registry, RegistryError};
use coblet_types::{ExtensionId, GameVersionId};

use crate::{InstallRecipe, LoaderContext, LoaderError, LoaderVersion, ModLoader};

/// The Mod Loaders of a launcher, keyed by [`ExtensionId`], with any extra
/// version sources per loader.
///
/// Built-in and third-party loaders are registered the same way.
/// [`register`](Self::register) refuses an ID that is taken;
/// [`replace`](Self::replace) is the explicit way to swap a loader, for
/// example a built-in one with a patched copy.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_loader::{InstallRecipe, LoaderContext, LoaderError, LoaderRegistry, LoaderVersion, ModLoader};
/// use coblet_runtime::{BoxFuture, Extension};
/// use coblet_types::{ExtensionId, GameVersionId};
///
/// struct Acme(ExtensionId);
///
/// impl Extension for Acme {
///     fn id(&self) -> &ExtensionId { &self.0 }
///     fn display_name(&self) -> &str { "Acme" }
/// }
///
/// impl ModLoader for Acme {
///     fn component_uid(&self) -> &str { "com.acme.loader" }
///     fn versions<'a>(&'a self, _: &'a LoaderContext<'a>, _: &'a GameVersionId)
///         -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>> {
///         Box::pin(async { Ok(vec![LoaderVersion::new("1.0").recommended()]) })
///     }
///     fn install<'a>(&'a self, _: &'a LoaderContext<'a>, _: &'a GameVersionId, _: &'a LoaderVersion)
///         -> BoxFuture<'a, Result<InstallRecipe, LoaderError>> {
///         Box::pin(async { Ok(InstallRecipe::default()) })
///     }
/// }
///
/// let mut loaders = LoaderRegistry::new();
/// loaders.register(Arc::new(Acme(ExtensionId::new("acme:loader")?)))?;
/// assert!(loaders.register(Arc::new(Acme(ExtensionId::new("acme:loader")?))).is_err());
/// assert!(loaders.replace(Arc::new(Acme(ExtensionId::new("acme:loader")?))).is_some());
/// assert_eq!(loaders.for_component("com.acme.loader").unwrap().id().as_str(), "acme:loader");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Default)]
pub struct LoaderRegistry {
    loaders: Registry<dyn ModLoader>,
    sources: BTreeMap<ExtensionId, Vec<Arc<dyn CatalogSource<LoaderVersion>>>>,
}

/// A Mod Loader found in a component stack.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedLoader {
    /// The Mod Loader.
    pub loader: ExtensionId,
    /// The installed version.
    pub version: String,
}

impl LoaderRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a Mod Loader under its own ID.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when the ID is taken.
    pub fn register(&mut self, loader: Arc<dyn ModLoader>) -> Result<(), RegistryError> {
        self.loaders.register(loader)
    }

    /// Puts a Mod Loader under its own ID, replacing and returning the one already there.
    pub fn replace(&mut self, loader: Arc<dyn ModLoader>) -> Option<Arc<dyn ModLoader>> {
        self.loaders.replace(loader)
    }

    /// Removes a Mod Loader and its extra version sources.
    pub fn remove(&mut self, id: &ExtensionId) -> Option<Arc<dyn ModLoader>> {
        self.sources.remove(id);
        self.loaders.remove(id)
    }

    /// Looks up a Mod Loader.
    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&Arc<dyn ModLoader>> {
        self.loaders.get(id)
    }

    /// Iterates over the Mod Loaders in ID order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Arc<dyn ModLoader>> {
        self.loaders.iter()
    }

    /// Returns the number of Mod Loaders.
    #[must_use]
    pub fn len(&self) -> usize {
        self.loaders.len()
    }

    /// Reports whether no Mod Loader is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.loaders.is_empty()
    }

    /// Adds a version source for a Mod Loader, asked after the loader itself;
    /// for items with the same version string the loader's own entry wins.
    pub fn add_version_source(
        &mut self,
        loader: ExtensionId,
        source: Arc<dyn CatalogSource<LoaderVersion>>,
    ) {
        self.sources.entry(loader).or_default().push(source);
    }

    /// Returns the extra version sources of a Mod Loader.
    #[must_use]
    pub fn version_sources(
        &self,
        loader: &ExtensionId,
    ) -> &[Arc<dyn CatalogSource<LoaderVersion>>] {
        self.sources.get(loader).map_or(&[], Vec::as_slice)
    }

    /// Returns the Mod Loader whose identifying component has this UID.
    #[must_use]
    pub fn for_component(&self, uid: &str) -> Option<&Arc<dyn ModLoader>> {
        self.loaders
            .iter()
            .find(|loader| loader.component_uid() == uid)
    }

    /// Lists the Mod Loaders installed in a component stack.
    #[must_use]
    pub fn detect(&self, components: &[ComponentRef]) -> Vec<DetectedLoader> {
        self.loaders
            .iter()
            .filter_map(|loader| {
                loader.detect(components).map(|version| DetectedLoader {
                    loader: loader.id().clone(),
                    version,
                })
            })
            .collect()
    }

    fn loader(&self, id: &ExtensionId) -> Result<&Arc<dyn ModLoader>, LoaderError> {
        self.get(id)
            .ok_or_else(|| LoaderError::UnknownLoader(id.clone()))
    }

    /// Builds the version catalog of a Mod Loader: the loader first, then its
    /// extra sources. List it with a [`Filter`](coblet_catalog::Filter) that
    /// names the game version.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::UnknownLoader`] when no loader has this ID.
    pub fn catalog(
        &self,
        id: &ExtensionId,
        meta: MetaCache,
    ) -> Result<Catalog<LoaderVersion>, LoaderError> {
        let loader = self.loader(id)?;
        let mut catalog = Catalog::new(meta).with_source(Arc::new(LoaderSource {
            loader: Arc::clone(loader),
        }));
        for source in self.version_sources(id) {
            catalog = catalog.with_source(Arc::clone(source));
        }
        Ok(catalog)
    }

    /// Asks a Mod Loader what installing a version adds, and checks that the
    /// recipe contains the loader's identifying component.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::UnknownLoader`], the loader's own error, or
    /// [`LoaderError::InvalidMetadata`] for a recipe without the loader's component.
    pub async fn install(
        &self,
        catalog: &CatalogContext,
        id: &ExtensionId,
        game: &GameVersionId,
        version: &LoaderVersion,
    ) -> Result<InstallRecipe, LoaderError> {
        let loader = self.loader(id)?;
        let cx = LoaderContext::new(catalog, id);
        let recipe = loader.install(&cx, game, version).await?;
        let uid = loader.component_uid();
        if recipe.component(uid).is_none() {
            return Err(LoaderError::invalid(
                id,
                format!("the recipe for {} has no `{uid}` component", version.id),
            ));
        }
        Ok(recipe)
    }
}

impl fmt::Debug for LoaderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoaderRegistry")
            .field("loaders", &self.loaders)
            .field(
                "sources",
                &self
                    .sources
                    .iter()
                    .map(|(id, sources)| (id.as_str(), sources.len()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// A Mod Loader seen as the first source of its version catalog.
struct LoaderSource {
    loader: Arc<dyn ModLoader>,
}

impl Extension for LoaderSource {
    fn id(&self) -> &ExtensionId {
        self.loader.id()
    }

    fn display_name(&self) -> &str {
        self.loader.display_name()
    }
}

impl CatalogSource<LoaderVersion> for LoaderSource {
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
            let loader = LoaderContext::new(cx, self.loader.id());
            let mut versions = self.loader.versions(&loader, game).await?;
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

    use coblet_catalog::{CachePolicy, CatalogContext, Filter, MetaCache};
    use coblet_catalog::{CatalogError, CatalogPage, CatalogQuery, CatalogSource};
    use coblet_platform::{Component, ComponentPatch, ComponentRef};
    use coblet_runtime::{ArtifactKind, BoxFuture, Extension, PrefixRewriter, TokioStorage};
    use coblet_types::{ExtensionId, GameVersionId};

    use super::LoaderRegistry;
    use crate::mock::Served;
    use crate::{InstallRecipe, LoaderContext, LoaderError, LoaderVersion, ModLoader};

    /// Loader whose versions come from a JSON document, to show requests go
    /// through the metadata cache.
    struct Listed {
        id: ExtensionId,
        uid: &'static str,
    }

    impl Extension for Listed {
        fn id(&self) -> &ExtensionId {
            &self.id
        }

        fn display_name(&self) -> &'static str {
            "Listed"
        }
    }

    impl ModLoader for Listed {
        fn component_uid(&self) -> &str {
            self.uid
        }

        fn versions<'a>(
            &'a self,
            cx: &'a LoaderContext<'a>,
            game: &'a GameVersionId,
        ) -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>> {
            Box::pin(async move {
                let url = format!("https://meta.example/{game}.json");
                cx.fetch_json(&url, &format!("{game}.json")).await
            })
        }

        fn install<'a>(
            &'a self,
            _cx: &'a LoaderContext<'a>,
            _game: &'a GameVersionId,
            version: &'a LoaderVersion,
        ) -> BoxFuture<'a, Result<InstallRecipe, LoaderError>> {
            Box::pin(async move {
                Ok(InstallRecipe::new(vec![Component::new(
                    ComponentPatch::new("com.acme.loader", version.id.clone()),
                )]))
            })
        }
    }

    /// Extra version source with one fixed entry.
    struct Extra(ExtensionId);

    impl Extension for Extra {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Extra"
        }
    }

    impl CatalogSource<LoaderVersion> for Extra {
        fn fetch<'a>(
            &'a self,
            _cx: &'a CatalogContext,
            _query: &'a CatalogQuery,
        ) -> BoxFuture<'a, Result<CatalogPage<LoaderVersion>, CatalogError>> {
            Box::pin(async { Ok(CatalogPage::new(vec![LoaderVersion::new("0.9")])) })
        }
    }

    fn listed(id: &str, uid: &'static str) -> Arc<dyn ModLoader> {
        Arc::new(Listed {
            id: ExtensionId::new(id).unwrap(),
            uid,
        })
    }

    #[tokio::test]
    async fn lists_versions_through_the_metadata_cache() {
        let dir = tempfile::tempdir().unwrap();
        let served = Arc::new(Served::new([(
            "https://mirror.example/1.20.1.json",
            r#"[{ "id": "2.0", "type": "beta" }, { "id": "1.0", "recommended": true }]"#,
        )]));
        // Only loader requests are mirrored, so the listing proves its kind.
        let mirror = PrefixRewriter::new("https://meta.example/", "https://mirror.example/")
            .unwrap()
            .for_kinds([ArtifactKind::Loader]);
        let meta = MetaCache::new(served.clone(), Arc::new(TokioStorage::new()), dir.path())
            .with_rewriter(Arc::new(mirror));
        let mut loaders = LoaderRegistry::new();
        let id = ExtensionId::new("acme:loader").unwrap();
        loaders
            .register(listed("acme:loader", "com.acme.loader"))
            .unwrap();

        let catalog = loaders.catalog(&id, meta.clone()).unwrap();
        let game: GameVersionId = "1.20.1".parse().unwrap();
        let versions = catalog
            .list(&Filter::new().compatible_with(game.clone()))
            .await
            .unwrap();
        let ids: Vec<&str> = versions.iter().map(|version| version.id.as_str()).collect();
        assert_eq!(ids, ["2.0", "1.0"]);
        assert_eq!(catalog.recommended(&versions).unwrap().id, "1.0");
        assert!(
            versions
                .iter()
                .all(|version| version.game.as_ref() == Some(&game))
        );
        assert!(dir.path().join("loaders/acme/loader/1.20.1.json").is_file());
        assert_eq!(served.asked(), ["https://mirror.example/1.20.1.json"]);

        assert!(
            versions
                .iter()
                .all(|version| version.source.as_ref() == Some(&id))
        );

        let extra = ExtensionId::new("acme:extra").unwrap();
        loaders.add_version_source(id.clone(), Arc::new(Extra(extra.clone())));
        let merged = loaders
            .catalog(&id, meta.clone())
            .unwrap()
            .list(&Filter::new().compatible_with(game.clone()))
            .await
            .unwrap();
        let sources: Vec<(&str, &str)> = merged
            .iter()
            .map(|version| {
                (
                    version.id.as_str(),
                    version.source.as_ref().unwrap().as_str(),
                )
            })
            .collect();
        assert_eq!(
            sources,
            [
                ("2.0", "acme:loader"),
                ("1.0", "acme:loader"),
                ("0.9", "acme:extra")
            ]
        );

        let unlisted = catalog.list(&Filter::new()).await.unwrap_err();
        assert!(
            unlisted.to_string().contains("per game version"),
            "{unlisted}"
        );
    }

    #[tokio::test]
    async fn install_checks_the_identifying_component() {
        let dir = tempfile::tempdir().unwrap();
        let served = Arc::new(Served::new([]));
        let meta = MetaCache::new(served, Arc::new(TokioStorage::new()), dir.path());
        let cx = CatalogContext::new(meta, CachePolicy::PreferCache);
        let mut loaders = LoaderRegistry::new();
        loaders
            .register(listed("acme:good", "com.acme.loader"))
            .unwrap();
        loaders
            .register(listed("acme:bad", "com.acme.other"))
            .unwrap();
        let game: GameVersionId = "1.20.1".parse().unwrap();
        let version = LoaderVersion::new("1.0");

        let good = ExtensionId::new("acme:good").unwrap();
        let recipe = loaders.install(&cx, &good, &game, &version).await.unwrap();
        assert_eq!(recipe.components[0].version(), "1.0");

        let bad = ExtensionId::new("acme:bad").unwrap();
        let error = loaders
            .install(&cx, &bad, &game, &version)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid metadata from acme:bad: the recipe for 1.0 has no `com.acme.other` component"
        );
        let missing = ExtensionId::new("acme:missing").unwrap();
        assert!(matches!(
            loaders.install(&cx, &missing, &game, &version).await,
            Err(LoaderError::UnknownLoader(_))
        ));
    }

    #[test]
    fn detects_installed_loaders_and_removes_their_sources() {
        let mut loaders = LoaderRegistry::new();
        loaders
            .register(listed("acme:loader", "com.acme.loader"))
            .unwrap();
        let stack = [
            ComponentRef {
                uid: "net.minecraft".to_owned(),
                version: "1.20.1".to_owned(),
            },
            ComponentRef {
                uid: "com.acme.loader".to_owned(),
                version: "1.0".to_owned(),
            },
        ];
        let found = loaders.detect(&stack);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].loader.as_str(), "acme:loader");
        assert_eq!(found[0].version, "1.0");

        let id = ExtensionId::new("acme:loader").unwrap();
        loaders.add_version_source(id.clone(), Arc::new(Extra(id.clone())));
        assert_eq!(loaders.version_sources(&id).len(), 1);
        assert!(loaders.remove(&id).is_some());
        assert!(loaders.version_sources(&id).is_empty());
        assert!(loaders.is_empty());
    }
}
