use std::sync::Arc;

use coblet_catalog::{Catalog, Filter, MetaCache};
use coblet_mojang::{GameVersion, MojangVersions};
use coblet_runtime::EventBus;

use crate::{Sdk, SdkError};

/// Game version catalog backed by the Mojang manifest.
pub(crate) struct VersionCatalog {
    pub(crate) catalog: Catalog<GameVersion>,
    pub(crate) mojang: Arc<MojangVersions>,
}

pub(crate) fn version_catalog(meta: MetaCache, events: EventBus) -> VersionCatalog {
    let mojang = Arc::new(MojangVersions::new());
    VersionCatalog {
        catalog: Catalog::new(meta)
            .with_source(Arc::clone(&mojang) as _)
            .with_events(events),
        mojang,
    }
}

impl Sdk {
    /// Lists game versions that match `filter`, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::Catalog`] when the manifest is unavailable and not cached.
    pub async fn versions(&self, filter: &Filter) -> Result<Vec<GameVersion>, SdkError> {
        Ok(self.inner.versions.catalog.list(filter).await?)
    }

    /// Returns the version with this ID.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::UnknownVersion`] when the manifest does not list it.
    pub async fn version(&self, id: &str) -> Result<GameVersion, SdkError> {
        self.versions(&Filter::new())
            .await?
            .into_iter()
            .find(|version| version.id.as_str() == id)
            .ok_or_else(|| SdkError::UnknownVersion(id.to_owned()))
    }

    /// Returns the latest release named by the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::UnknownVersion`] when the manifest names none.
    pub async fn latest_release(&self) -> Result<GameVersion, SdkError> {
        self.versions(&Filter::new())
            .await?
            .into_iter()
            .find(|version| version.latest_release)
            .ok_or_else(|| SdkError::UnknownVersion("latest release".to_owned()))
    }

    /// Returns the Mojang version source, for its trait table and manifest address.
    #[must_use]
    pub fn mojang_versions(&self) -> &Arc<MojangVersions> {
        &self.inner.versions.mojang
    }

    /// Fetches the version manifest again.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::Catalog`] when the manifest cannot be fetched.
    pub async fn refresh_versions(&self) -> Result<(), SdkError> {
        Ok(self.inner.versions.catalog.refresh().await?)
    }
}
