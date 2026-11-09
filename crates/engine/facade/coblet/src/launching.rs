use std::path::PathBuf;
use std::sync::Arc;

use coblet_launch::{GameLayout, GameSession, LaunchContext, LaunchRequest, LaunchServices};
use coblet_mojang::{GameVersion, TraitInput, fetch_version_json};
use coblet_platform::{Component, EffectiveLaunchProfile, ProfileEngine, ResolvedProfile};

use crate::{Coblet, Error};

impl Coblet {
    /// Adapts a version to this platform with the Platform Profiles of this
    /// `Coblet`: the built-in ones and those added with
    /// [`CobletBuilder::platform_profile`](crate::CobletBuilder::platform_profile).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] when the version JSON is unavailable and
    /// [`Error::Profile`] when the version cannot run here; the error
    /// lists what is missing.
    pub async fn resolve(&self, version: &GameVersion) -> Result<EffectiveLaunchProfile, Error> {
        self.resolve_with(version, &[]).await
    }

    /// Adapts a version with extra components on top, such as those of a
    /// Mod Loader's install recipe, in stack order.
    ///
    /// The components go through the Compose step before the Platform
    /// Profiles, so a loader's libraries and arguments are adapted like the
    /// game's own.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`resolve`](Self::resolve), and
    /// [`Error::Profile`] when a component's requirements are not met or two
    /// components conflict.
    pub async fn resolve_with(
        &self,
        version: &GameVersion,
        components: &[Component],
    ) -> Result<EffectiveLaunchProfile, Error> {
        let cx = self.catalog_context();
        let document = fetch_version_json(&cx, version).await;
        self.forward_warnings(&cx);
        let document = Arc::new(document?);
        let input = TraitInput::listed(version).with_version(Arc::clone(&document));
        let report = self.traits_of(&cx, input, version).await;
        let raw = Arc::unwrap_or_clone(document).value;
        let resolved = ResolvedProfile::from_version(raw, report.traits)?.compose(components)?;
        Ok(self
            .inner
            .profiles
            .resolve(&resolved, &self.inner.platform)?)
    }

    /// Returns the profile engine that adapts versions to this platform.
    #[must_use]
    pub fn profile_engine(&self) -> &ProfileEngine {
        &self.inner.profiles
    }

    /// Returns the services a launch uses: the Java providers and locators
    /// of this `Coblet`, with runtimes under `data_root/runtimes`.
    #[must_use]
    pub fn launch_services(&self) -> LaunchServices {
        let inner = &self.inner;
        LaunchServices::new(
            Arc::clone(&inner.storage),
            Arc::clone(&inner.spawner),
            inner.downloader.clone(),
            Arc::new(self.catalog_context()),
            inner.platform.os().clone(),
        )
        .with_java_locators(inner.java_locators.clone())
        .with_java_installer(inner.java.clone())
        .with_events(inner.events.clone())
        .with_placeholder_sources(inner.placeholder_sources.clone())
    }

    /// Prepares and starts a resolved profile.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Launch`] when a launch step fails.
    pub async fn launch_profile(
        &self,
        profile: EffectiveLaunchProfile,
        game_directory: impl Into<PathBuf>,
        request: LaunchRequest,
    ) -> Result<GameSession, Error> {
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
    /// Returns [`Error::UnknownVersion`], [`Error::Profile`] or
    /// [`Error::Launch`] for the step that failed.
    pub async fn launch(
        &self,
        version_id: &str,
        game_directory: impl Into<PathBuf>,
        request: LaunchRequest,
    ) -> Result<GameSession, Error> {
        let version = self.version(version_id).await?;
        let profile = self.resolve(&version).await?;
        self.launch_profile(profile, game_directory, request).await
    }
}
