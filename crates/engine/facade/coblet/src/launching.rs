use std::path::PathBuf;
use std::sync::Arc;

use coblet_catalog::{CachePolicy, CatalogContext};
use coblet_java::{RuntimeInstaller, SearchPaths};
use coblet_launch::{GameLayout, GameSession, LaunchContext, LaunchRequest, LaunchServices};
use coblet_mojang::{GameVersion, fetch_version_json};
use coblet_platform::{EffectiveLaunchProfile, ProfileEngine, ResolvedProfile};
use coblet_runtime::Event;

use crate::{Sdk, SdkError};

impl Sdk {
    fn catalog_context(&self) -> CatalogContext {
        CatalogContext::new(self.inner.meta.clone(), CachePolicy::PreferCache)
    }

    fn forward_warnings(&self, cx: &CatalogContext) {
        for warning in cx.take_warnings() {
            self.inner.events.publish(Event::Warning(warning));
        }
    }

    /// Adapts a version to this platform.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::Catalog`] when the version JSON is unavailable and
    /// [`SdkError::Profile`] when the version cannot run here; the error
    /// lists what is missing.
    pub async fn resolve(&self, version: &GameVersion) -> Result<EffectiveLaunchProfile, SdkError> {
        let cx = self.catalog_context();
        let document = fetch_version_json(&cx, version).await;
        self.forward_warnings(&cx);
        let raw = document?.value;
        let traits = self.inner.versions.mojang.traits_of(&raw);
        let resolved = ResolvedProfile::from_version(raw, traits)?;
        Ok(ProfileEngine::builtin().resolve(&resolved, &self.inner.platform)?)
    }

    /// Returns the services a launch uses, with Java runtimes under `data_root/runtimes`.
    #[must_use]
    pub fn launch_services(&self) -> LaunchServices {
        let inner = &self.inner;
        let runtimes = inner.data_root.join("runtimes");
        let installer = RuntimeInstaller::new(
            Arc::new(inner.downloader.clone()),
            Arc::clone(&inner.storage),
            &runtimes,
        );
        LaunchServices::new(
            Arc::clone(&inner.storage),
            Arc::clone(&inner.spawner),
            inner.downloader.clone(),
            Arc::new(self.catalog_context()),
            inner.platform.os().clone(),
        )
        .with_java_search(SearchPaths::from_env().with_runtimes(runtimes))
        .with_java_installer(installer)
        .with_events(inner.events.clone())
    }

    /// Prepares and starts a resolved profile.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::Launch`] when a launch step fails.
    pub async fn launch_profile(
        &self,
        profile: EffectiveLaunchProfile,
        game_directory: impl Into<PathBuf>,
        request: LaunchRequest,
    ) -> Result<GameSession, SdkError> {
        let services = self.launch_services();
        let catalog = Arc::clone(&services.catalog);
        let layout = GameLayout::new(&self.inner.data_root, game_directory);
        let cx = LaunchContext::new(services, profile, layout, request);
        let session = self.inner.launcher.launch(cx).await;
        self.forward_warnings(&catalog);
        Ok(session?)
    }

    /// Resolves a version by ID, prepares its files and starts it.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::UnknownVersion`], [`SdkError::Profile`] or
    /// [`SdkError::Launch`] for the step that failed.
    pub async fn launch(
        &self,
        version_id: &str,
        game_directory: impl Into<PathBuf>,
        request: LaunchRequest,
    ) -> Result<GameSession, SdkError> {
        let version = self.version(version_id).await?;
        let profile = self.resolve(&version).await?;
        self.launch_profile(profile, game_directory, request).await
    }
}
