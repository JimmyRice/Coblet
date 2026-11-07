use std::sync::Arc;

use coblet_catalog::{Catalog, CatalogContext, Filter, MetaCache};
use coblet_mojang::{
    GameVersion, MojangVersions, TraitInput, TraitReport, TraitSources, fetch_version_json,
};
use coblet_runtime::EventBus;

use crate::builder::VersionSettings;
use crate::{Coblet, Error};

/// Game version catalog: the Mojang manifest first, then added sources.
pub(crate) struct VersionCatalog {
    pub(crate) catalog: Catalog<GameVersion>,
    pub(crate) mojang: Arc<MojangVersions>,
    pub(crate) traits: Arc<TraitSources>,
}

pub(crate) fn version_catalog(
    meta: MetaCache,
    events: EventBus,
    settings: VersionSettings,
    traits: TraitSources,
) -> VersionCatalog {
    let mojang = Arc::new(settings.mojang.unwrap_or_default());
    let traits = Arc::new(traits);
    let catalog = settings.sources.into_iter().fold(
        Catalog::new(meta).with_source(Arc::clone(&mojang) as _),
        Catalog::with_source,
    );
    VersionCatalog {
        catalog: catalog
            .with_stage(Arc::clone(&traits) as _)
            .with_events(events),
        mojang,
        traits,
    }
}

impl Coblet {
    /// Lists game versions that match `filter`, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] when the manifest is unavailable and not cached.
    pub async fn versions(&self, filter: &Filter) -> Result<Vec<GameVersion>, Error> {
        Ok(self.inner.versions.catalog.list(filter).await?)
    }

    /// Returns the version with this ID.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownVersion`] when the manifest does not list it.
    pub async fn version(&self, id: &str) -> Result<GameVersion, Error> {
        self.versions(&Filter::new())
            .await?
            .into_iter()
            .find(|version| version.id.as_str() == id)
            .ok_or_else(|| Error::UnknownVersion(id.to_owned()))
    }

    /// Returns the latest release named by the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownVersion`] when the manifest names none.
    pub async fn latest_release(&self) -> Result<GameVersion, Error> {
        self.versions(&Filter::new())
            .await?
            .into_iter()
            .find(|version| version.latest_release)
            .ok_or_else(|| Error::UnknownVersion("latest release".to_owned()))
    }

    /// Returns the Mojang version source, for its manifest address.
    #[must_use]
    pub fn mojang_versions(&self) -> &Arc<MojangVersions> {
        &self.inner.versions.mojang
    }

    /// Returns the version trait sources, in the order they run.
    #[must_use]
    pub fn trait_sources(&self) -> &TraitSources {
        &self.inner.versions.traits
    }

    /// Returns the traits of a version with its version JSON, and which
    /// trait source added or removed each.
    ///
    /// Trait sources that fail are skipped and reported as
    /// `version.trait-source-failed` warnings.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] when the version JSON is unavailable.
    pub async fn version_traits(&self, version: &GameVersion) -> Result<TraitReport, Error> {
        let cx = self.catalog_context();
        let document = fetch_version_json(&cx, version).await;
        self.forward_warnings(&cx);
        let input = TraitInput::listed(version).with_version(Arc::new(document?));
        Ok(self.traits_of(&cx, input, version).await)
    }

    /// Runs the trait sources for one version and publishes their failures.
    pub(crate) async fn traits_of(
        &self,
        cx: &CatalogContext,
        input: TraitInput,
        version: &GameVersion,
    ) -> TraitReport {
        let (report, failures) = self
            .inner
            .versions
            .traits
            .report(cx, input, version.declared_traits.clone())
            .await;
        self.forward_trait_failures(cx, &failures);
        report
    }

    /// Fetches the version manifest again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] when the manifest cannot be fetched.
    pub async fn refresh_versions(&self) -> Result<(), Error> {
        Ok(self.inner.versions.catalog.refresh().await?)
    }
}
