use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use coblet_auth::{AccountStore, AuthCtx, AuthMethod};
use coblet_catalog::MetaCache;
use coblet_download::{Downloader, DownloaderConfig};
use coblet_runtime::{
    EventBus, MemorySecretStore, PlatformProbe, ProcessSpawner, SecretStore, StdPlatformProbe,
    Storage, TokioSpawner, TokioStorage, Transport, UrlRewriter, UrlRewriters, UserAgent,
};
use coblet_types::Platform;

use crate::context::Inner;
use crate::{Coblet, Error};

/// Builder state: the data root is missing.
#[derive(Debug)]
pub struct NoRoot;
/// Builder state: the data root is set and [`CobletBuilder::build`] is available.
#[derive(Debug)]
pub struct WithRoot;

/// Events kept for slow subscribers before they miss some.
const EVENT_CAPACITY: usize = 1024;

/// Builder of a [`Coblet`]; [`build`](CobletBuilder::build) needs a data root.
#[must_use]
pub struct CobletBuilder<State> {
    data_root: Option<PathBuf>,
    user_agent: Option<UserAgent>,
    transport: Option<Arc<dyn Transport>>,
    storage: Option<Arc<dyn Storage>>,
    spawner: Option<Arc<dyn ProcessSpawner>>,
    platform: Option<Platform>,
    events: Option<EventBus>,
    rewriters: UrlRewriters,
    mirror_fallback: bool,
    #[cfg(feature = "mojang")]
    versions: VersionSettings,
    #[cfg(feature = "launch")]
    launcher: Option<coblet_launch::Launcher>,
    #[cfg(feature = "launch")]
    customizers: Vec<Arc<dyn coblet_launch::LaunchCustomizer>>,
    #[cfg(feature = "launch")]
    auth_grants: Vec<coblet_types::ExtensionId>,
    #[cfg(feature = "launch")]
    profile_engine: Option<coblet_platform::ProfileEngine>,
    #[cfg(feature = "launch")]
    platform_profiles: Vec<Arc<dyn coblet_platform::PlatformProfile>>,
    #[cfg(feature = "loaders")]
    loaders: LoaderSettings,
    #[cfg(feature = "java")]
    java: JavaSettings,
    auth: AuthSettings,
    state: PhantomData<State>,
}

/// Mod Loader changes collected by the builder, applied in order by [`CobletBuilder::build`].
#[cfg(feature = "loaders")]
#[derive(Default)]
struct LoaderSettings {
    changes: Vec<LoaderChange>,
    /// The first data definition that failed to parse.
    error: Option<coblet_loader::LoaderError>,
}

#[cfg(feature = "loaders")]
enum LoaderChange {
    Register(Arc<dyn coblet_loader::ModLoader>),
    Replace(Arc<dyn coblet_loader::ModLoader>),
    Source(
        coblet_types::ExtensionId,
        Arc<dyn coblet_catalog::CatalogSource<coblet_loader::LoaderVersion>>,
    ),
}

/// Java provider and locator changes collected by the builder, applied in
/// order on top of the built-in lists by [`CobletBuilder::build`].
#[cfg(feature = "java")]
#[derive(Default)]
struct JavaSettings {
    providers: Vec<Change<Arc<dyn coblet_java::JavaProvider>>>,
    locators: Vec<Change<Arc<dyn coblet_java::JavaLocator>>>,
    /// Executables tried before every other place.
    executables: Vec<PathBuf>,
    /// Folders holding one Java per child, with the path to each Java home.
    directories: Vec<(PathBuf, String)>,
    /// The first data definition that failed to parse.
    error: Option<coblet_java::JavaError>,
}

/// One change to an ordered extension list.
#[cfg(any(feature = "java", feature = "mojang"))]
enum Change<T> {
    Register(T),
    Replace(T),
    Remove(coblet_types::ExtensionId),
}

/// Version catalog settings collected by the builder.
#[cfg(feature = "mojang")]
#[derive(Default)]
pub(crate) struct VersionSettings {
    mojang: Option<coblet_mojang::MojangVersions>,
    /// Changes applied in order to the built-in version sources.
    sources: Vec<Change<Arc<dyn coblet_catalog::CatalogSource<coblet_mojang::GameVersion>>>>,
    /// Changes applied in order to the built-in trait sources.
    traits: Vec<Change<Arc<dyn coblet_mojang::TraitSource>>>,
    /// The first trait table that failed to parse.
    traits_error: Option<coblet_mojang::TraitError>,
}

#[cfg(feature = "mojang")]
impl VersionSettings {
    /// Builds the version catalog with the sources and trait sources
    /// after the builder's changes.
    pub(crate) fn catalog(
        &mut self,
        meta: MetaCache,
        events: EventBus,
    ) -> Result<crate::versions::VersionCatalog, Error> {
        let traits = self.trait_sources()?;
        let (sources, mojang) = self.version_sources()?;
        Ok(crate::versions::version_catalog(
            meta, events, sources, mojang, traits,
        ))
    }

    /// Applies the version source changes to the built-in list, and
    /// returns the Mojang source when it is still in the list.
    fn version_sources(
        &mut self,
    ) -> Result<
        (
            coblet_catalog::CatalogSources<coblet_mojang::GameVersion>,
            Option<Arc<coblet_mojang::MojangVersions>>,
        ),
        Error,
    > {
        let mojang = Arc::new(self.mojang.take().unwrap_or_default());
        let mut sources = coblet_catalog::CatalogSources::new();
        sources.register(Arc::clone(&mojang) as _)?;
        for change in std::mem::take(&mut self.sources) {
            match change {
                Change::Register(source) => sources.register(source)?,
                Change::Replace(source) => {
                    sources.replace(source);
                }
                Change::Remove(id) => {
                    sources.remove(&id);
                }
            }
        }
        if sources.is_empty() {
            return Err(Error::Config("every game version source was removed"));
        }
        let kept = sources
            .iter()
            .any(|source| std::ptr::addr_eq(Arc::as_ptr(source), Arc::as_ptr(&mojang)));
        Ok((sources, kept.then_some(mojang)))
    }

    /// Applies the trait source changes to the built-in list.
    fn trait_sources(&mut self) -> Result<coblet_mojang::TraitSources, Error> {
        if let Some(error) = self.traits_error.take() {
            return Err(error.into());
        }
        let mut sources = coblet_mojang::TraitSources::builtin();
        for change in std::mem::take(&mut self.traits) {
            match change {
                Change::Register(source) => sources.register(source)?,
                Change::Replace(source) => {
                    sources.replace(source);
                }
                Change::Remove(id) => {
                    sources.remove(&id);
                }
            }
        }
        Ok(sources)
    }
}

/// Account settings collected by the builder.
#[derive(Default)]
struct AuthSettings {
    secrets: Option<Arc<dyn SecretStore>>,
    methods: Vec<Arc<dyn AuthMethod>>,
    #[cfg(feature = "auth-msa")]
    msa: Option<coblet_auth::MsaConfig>,
}

impl<State> std::fmt::Debug for CobletBuilder<State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CobletBuilder")
            .field("data_root", &self.data_root)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl Coblet {
    /// Starts a builder.
    pub fn builder() -> CobletBuilder<NoRoot> {
        CobletBuilder {
            data_root: None,
            user_agent: None,
            transport: None,
            storage: None,
            spawner: None,
            platform: None,
            events: None,
            rewriters: UrlRewriters::new(),
            mirror_fallback: true,
            #[cfg(feature = "mojang")]
            versions: VersionSettings::default(),
            #[cfg(feature = "launch")]
            launcher: None,
            #[cfg(feature = "launch")]
            customizers: Vec::new(),
            #[cfg(feature = "launch")]
            auth_grants: Vec::new(),
            #[cfg(feature = "launch")]
            profile_engine: None,
            #[cfg(feature = "launch")]
            platform_profiles: Vec::new(),
            #[cfg(feature = "loaders")]
            loaders: LoaderSettings::default(),
            #[cfg(feature = "java")]
            java: JavaSettings::default(),
            auth: AuthSettings::default(),
            state: PhantomData,
        }
    }
}

impl CobletBuilder<NoRoot> {
    /// Sets the directory for shared game files, caches and runtimes; a
    /// relative path is resolved against the current directory.
    pub fn data_root(self, path: impl Into<PathBuf>) -> CobletBuilder<WithRoot> {
        CobletBuilder {
            data_root: Some(path.into()),
            user_agent: self.user_agent,
            transport: self.transport,
            storage: self.storage,
            spawner: self.spawner,
            platform: self.platform,
            events: self.events,
            rewriters: self.rewriters,
            mirror_fallback: self.mirror_fallback,
            #[cfg(feature = "mojang")]
            versions: self.versions,
            #[cfg(feature = "launch")]
            launcher: self.launcher,
            #[cfg(feature = "launch")]
            customizers: self.customizers,
            #[cfg(feature = "launch")]
            auth_grants: self.auth_grants,
            #[cfg(feature = "launch")]
            profile_engine: self.profile_engine,
            #[cfg(feature = "launch")]
            platform_profiles: self.platform_profiles,
            #[cfg(feature = "loaders")]
            loaders: self.loaders,
            #[cfg(feature = "java")]
            java: self.java,
            auth: self.auth,
            state: PhantomData,
        }
    }
}

impl<State> CobletBuilder<State> {
    /// Sets the User-Agent of the default transport, such as `MyLauncher/1.0 (contact@example.com)`.
    pub fn user_agent(mut self, user_agent: UserAgent) -> Self {
        self.user_agent = Some(user_agent);
        self
    }

    /// Uses this transport instead of the default one.
    pub fn transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Uses this file access instead of `tokio::fs`.
    pub fn storage(mut self, storage: Arc<dyn Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Uses this process spawner instead of `tokio::process`.
    pub fn spawner(mut self, spawner: Arc<dyn ProcessSpawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Describes the platform instead of detecting it.
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = Some(platform);
        self
    }

    /// Keeps account tokens in this store instead of process memory, for
    /// example a system keychain or an
    /// [`EncryptedFileStore`](coblet_auth::EncryptedFileStore).
    ///
    /// Without one, tokens are lost when the process exits and Microsoft
    /// accounts have to sign in again.
    pub fn secret_store(mut self, store: Arc<dyn SecretStore>) -> Self {
        self.auth.secrets = Some(store);
        self
    }

    /// Registers an auth method, such as a third-party login server.
    pub fn auth_method(mut self, method: Arc<dyn AuthMethod>) -> Self {
        self.auth.methods.push(method);
        self
    }

    /// Enables Microsoft login with the launcher's own Client ID.
    ///
    /// Without it, Microsoft logins and launches of Microsoft accounts fail
    /// with [`Error::Unsupported`].
    #[cfg(feature = "auth-msa")]
    pub fn msa_client_id(mut self, client_id: coblet_auth::MsaClientId) -> Self {
        self.auth.msa = Some(coblet_auth::MsaConfig::new(client_id));
        self
    }

    /// Enables Microsoft login with full settings, such as other scopes.
    #[cfg(feature = "auth-msa")]
    pub fn msa_config(mut self, config: coblet_auth::MsaConfig) -> Self {
        self.auth.msa = Some(config);
        self
    }

    /// Sends requests through `rewriter`, for regional mirrors and proxies.
    ///
    /// Rewriters apply to game files, libraries, assets, Java runtimes and
    /// metadata (version manifest, version files, runtime indexes), in the
    /// order they were added. Hashes are still checked, so a mirror cannot
    /// change content that is listed with a hash. When a mirror fails or
    /// serves something that does not match, the original address is tried
    /// next; see [`mirror_fallback`](CobletBuilder::mirror_fallback). A
    /// mirror that reformats JSON can be allowed with
    /// [`PrefixRewriter::trusting_metadata`](coblet_runtime::PrefixRewriter::trusting_metadata).
    /// Account and skin requests are not rewritten.
    /// [`PrefixRewriter`](coblet_runtime::PrefixRewriter) describes a
    /// mirror without code.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use coblet::Coblet;
    /// use coblet::runtime::{ArtifactKind, PrefixRewriter};
    ///
    /// let builder = Coblet::builder().url_rewriter(Arc::new(
    ///     PrefixRewriter::new("https://libraries.minecraft.net/", "https://mirror.example/libraries/")?
    ///         .for_kinds([ArtifactKind::Library]),
    /// ));
    /// # drop(builder);
    /// # Ok::<(), coblet::runtime::HttpUrlError>(())
    /// ```
    pub fn url_rewriter(mut self, rewriter: Arc<dyn UrlRewriter>) -> Self {
        self.rewriters = self.rewriters.with(rewriter);
        self
    }

    /// Sets whether requests fall back to the original address when a
    /// rewritten one fails or serves something that does not match its
    /// hash; on by default. Turn it off when the original hosts must never
    /// be contacted.
    pub fn mirror_fallback(mut self, enabled: bool) -> Self {
        self.mirror_fallback = enabled;
        self
    }

    /// Uses this Mojang version source, for example one with another
    /// manifest address. It keeps its place first in the list unless
    /// [`without_version_source`](Self::without_version_source) removes it.
    #[cfg(feature = "mojang")]
    pub fn mojang_versions(mut self, source: coblet_mojang::MojangVersions) -> Self {
        self.versions.mojang = Some(source);
        self
    }

    /// Adds a game version source after the Mojang manifest and the
    /// sources already added, such as one listing experimental versions.
    ///
    /// When two sources list the same version ID, the earlier one wins.
    /// Launching a version fetches its version file from the address the
    /// source gave, checked against the source's SHA-1.
    /// [`build`](CobletBuilder::build) fails when the ID is taken; use
    /// [`replace_version_source`](Self::replace_version_source) to swap one
    /// on purpose.
    #[cfg(feature = "mojang")]
    pub fn version_source(
        mut self,
        source: Arc<dyn coblet_catalog::CatalogSource<coblet_mojang::GameVersion>>,
    ) -> Self {
        self.versions.sources.push(Change::Register(source));
        self
    }

    /// Puts a game version source in place of the one with the same ID,
    /// keeping its place in the order, such as `coblet:mojang`; appends it
    /// when the ID is free.
    #[cfg(feature = "mojang")]
    pub fn replace_version_source(
        mut self,
        source: Arc<dyn coblet_catalog::CatalogSource<coblet_mojang::GameVersion>>,
    ) -> Self {
        self.versions.sources.push(Change::Replace(source));
        self
    }

    /// Removes a game version source, such as `coblet:mojang` for a
    /// launcher that lists only its own versions; an unknown ID is ignored.
    /// [`build`](CobletBuilder::build) fails when no source is left.
    #[cfg(feature = "mojang")]
    pub fn without_version_source(mut self, id: coblet_types::ExtensionId) -> Self {
        self.versions.sources.push(Change::Remove(id));
        self
    }

    /// Adds a game version source that reads a manifest in the format of
    /// Mojang's `version_manifest_v2.json` from `url`, under the ID `id`.
    #[cfg(feature = "mojang")]
    pub fn version_manifest(
        self,
        id: coblet_types::ExtensionId,
        url: coblet_runtime::HttpUrl,
    ) -> Self {
        self.version_source(Arc::new(coblet_mojang::MojangVersions::other(id, url)))
    }

    /// Adds a version trait source after the built-in table and the
    /// sources already added, so it can remove traits they gave.
    ///
    /// [`build`](CobletBuilder::build) fails when the ID is taken; use
    /// [`replace_trait_source`](Self::replace_trait_source) to swap one on
    /// purpose. Trait sources tag every listed version, whichever version
    /// source listed it, and run again with the version JSON when a
    /// version is resolved.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use coblet::Coblet;
    /// use coblet::mojang::{FnTraitSource, VersionTrait};
    ///
    /// let builder = Coblet::builder().trait_source(Arc::new(FnTraitSource::new(
    ///     "acme:snapshots".parse()?,
    ///     "Snapshot marker",
    ///     |target| {
    ///         if target.input().id.as_str().contains('w') {
    ///             target.traits_mut().insert(VersionTrait::from("acme:snapshot"));
    ///         }
    ///         Ok(())
    ///     },
    /// )));
    /// # drop(builder);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "mojang")]
    pub fn trait_source(mut self, source: Arc<dyn coblet_mojang::TraitSource>) -> Self {
        self.versions.traits.push(Change::Register(source));
        self
    }

    /// Puts a trait source in place of the one with the same ID, keeping
    /// its place in the order; appends it when the ID is free. Replacing
    /// `coblet:version-traits` swaps the built-in table.
    #[cfg(feature = "mojang")]
    pub fn replace_trait_source(mut self, source: Arc<dyn coblet_mojang::TraitSource>) -> Self {
        self.versions.traits.push(Change::Replace(source));
        self
    }

    /// Removes a trait source, such as the built-in table
    /// `coblet:version-traits`; an unknown ID is ignored.
    #[cfg(feature = "mojang")]
    pub fn without_trait_source(mut self, id: coblet_types::ExtensionId) -> Self {
        self.versions.traits.push(Change::Remove(id));
        self
    }

    /// Adds a trait table written in TOML, like
    /// [`trait_source`](Self::trait_source); see
    /// [`TraitTable`](coblet_mojang::TraitTable) for the format.
    ///
    /// A malformed table makes [`build`](CobletBuilder::build) fail with
    /// [`Error::Traits`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// use coblet::{Coblet, runtime::{Extension, UserAgent}};
    ///
    /// let coblet = Coblet::builder()
    ///     .data_root("/path/to/launcher-data")
    ///     .user_agent(UserAgent::new("MyLauncher/1.0 (contact@example.com)")?)
    ///     .trait_table_toml(r#"
    ///         id = "acme:fixes"
    ///         name = "Acme fixes"
    ///
    ///         [[rule]]
    ///         add = ["acme:offline-skins"]
    ///         released_before = "2014-04-09T07:52:06Z"
    ///
    ///         [[rule]]
    ///         remove = ["legacy-macos-rendering"]
    ///         ids = ["b1.7.3"]
    ///     "#)
    ///     .build()?;
    /// let ids: Vec<_> = coblet.trait_sources().iter().map(|source| source.id().as_str()).collect();
    /// assert_eq!(ids, ["coblet:version-traits", "acme:fixes"]);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "mojang")]
    pub fn trait_table_toml(self, table: &str) -> Self {
        self.trait_table(coblet_mojang::TraitTable::from_toml(table))
    }

    /// Adds a trait table written in JSON, with the keys of the TOML form
    /// of [`trait_table_toml`](Self::trait_table_toml).
    #[cfg(feature = "mojang")]
    pub fn trait_table_json(self, table: &str) -> Self {
        self.trait_table(coblet_mojang::TraitTable::from_json(table))
    }

    #[cfg(feature = "mojang")]
    fn trait_table(
        mut self,
        parsed: Result<coblet_mojang::TraitTable, coblet_mojang::TraitError>,
    ) -> Self {
        match parsed {
            Ok(table) => self.trait_source(Arc::new(table)),
            Err(error) => {
                self.versions.traits_error.get_or_insert(error);
                self
            }
        }
    }

    /// Publishes events on this bus instead of a new one.
    pub fn events(mut self, events: EventBus) -> Self {
        self.events = Some(events);
        self
    }

    /// Launches with this step chain instead of the built-in one.
    #[cfg(feature = "launch")]
    pub fn launcher(mut self, launcher: coblet_launch::Launcher) -> Self {
        self.launcher = Some(launcher);
        self
    }

    /// Adapts versions with this profile engine instead of one holding the
    /// built-in Platform Profiles; profiles from
    /// [`platform_profile`](Self::platform_profile) are added to it.
    #[cfg(feature = "launch")]
    pub fn profile_engine(mut self, engine: coblet_platform::ProfileEngine) -> Self {
        self.profile_engine = Some(engine);
        self
    }

    /// Adds a Platform Profile, such as one for an operating system or
    /// architecture the built-in profiles do not cover.
    ///
    /// Profiles from TOML come from
    /// [`DataProfile::from_toml`](coblet_platform::DataProfile::from_toml);
    /// code profiles implement [`PlatformProfile`](coblet_platform::PlatformProfile).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// use coblet::Coblet;
    /// use coblet::platform::{DataProfile, ProfileLayer};
    /// use coblet::runtime::UserAgent;
    ///
    /// let profile = DataProfile::from_toml(r#"
    ///     id = "community:linux-loongarch64"
    ///
    ///     [match]
    ///     os = ["linux"]
    ///     arch = ["loongarch64"]
    ///     requires_component = { uid = "org.lwjgl3" }
    ///
    ///     [rules]
    ///     mojang_os = "linux"
    ///
    ///     [[natives]]
    ///     match = "org.lwjgl:*:*:natives-linux"
    ///     source = { system = "/usr/lib/lwjgl3" }
    ///
    ///     [java]
    ///     source = "system-only"
    /// "#, ProfileLayer::ThirdParty)?;
    /// let coblet = Coblet::builder()
    ///     .data_root("/path/to/launcher-data")
    ///     .user_agent(UserAgent::new("MyLauncher/1.0 (contact@example.com)")?)
    ///     .platform_profile(Arc::new(profile))
    ///     .build()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "launch")]
    pub fn platform_profile(mut self, profile: Arc<dyn coblet_platform::PlatformProfile>) -> Self {
        self.platform_profiles.push(profile);
        self
    }

    /// Registers a launch customizer on the launcher.
    ///
    /// It runs only for launches whose
    /// [`LaunchRequest`](coblet_launch::LaunchRequest) names its ID.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use coblet::launch::{CustomizeCtx, LaunchCustomizer, LaunchError, LaunchRequest};
    /// # use coblet::launch_command::LaunchAuth;
    /// # use coblet::mojang::{VersionTrait, VersionTraits};
    /// # use coblet::platform::{EffectiveLaunchProfile, ProfileArg};
    /// # use coblet::runtime::{Extension, UserAgent};
    /// # use coblet::types::{ExtensionId, PlayerName};
    /// # struct Agent(ExtensionId);
    /// # impl Extension for Agent {
    /// #     fn id(&self) -> &ExtensionId { &self.0 }
    /// #     fn display_name(&self) -> &str { "Agent" }
    /// # }
    /// # impl LaunchCustomizer for Agent {
    /// #     fn applies_to(&self, traits: &VersionTraits) -> bool {
    /// #         traits.contains(&VersionTrait::LegacyOnlineServices)
    /// #     }
    /// #     fn customize(&self, profile: &mut EffectiveLaunchProfile, _: &CustomizeCtx) -> Result<(), LaunchError> {
    /// #         profile.jvm_args.push(ProfileArg::always("-javaagent:/opt/acme/fix.jar"));
    /// #         Ok(())
    /// #     }
    /// # }
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// use coblet::Coblet;
    ///
    /// let id = ExtensionId::new("acme:agent")?;
    /// let coblet = Coblet::builder()
    ///     .data_root("/path/to/launcher-data")
    ///     .user_agent(UserAgent::new("MyLauncher/1.0 (contact@example.com)")?)
    ///     .customizer(Arc::new(Agent(id.clone())))
    ///     .build()?;
    /// let request = LaunchRequest::new(LaunchAuth::offline(PlayerName::new("Steve")?))
    ///     .with_customizer(id);
    /// let session = coblet.launch("b1.7.3", "/path/to/game", request).await?;
    /// print!("{}", session.profile().explain());
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "launch")]
    pub fn customizer(mut self, customizer: Arc<dyn coblet_launch::LaunchCustomizer>) -> Self {
        self.customizers.push(customizer);
        self
    }

    /// Lets the customizer `id` add and remove authentication arguments,
    /// such as `--accessToken` or `${auth_uuid}`, for example one that
    /// points the game at a third-party login server.
    ///
    /// Customizers cannot do this by default: a launch fails with
    /// [`LaunchError::AuthArgument`](coblet_launch::LaunchError::AuthArgument)
    /// when one tries. Grant it only to customizers the launcher trusts with
    /// the account; their changes still appear in the explanation.
    /// [`build`](CobletBuilder::build) fails when no customizer is
    /// registered as `id`.
    #[cfg(feature = "launch")]
    pub fn allow_auth_changes(mut self, id: coblet_types::ExtensionId) -> Self {
        self.auth_grants.push(id);
        self
    }

    /// Registers a Mod Loader written as code.
    ///
    /// Built-in and third-party loaders are registered the same way;
    /// [`build`](CobletBuilder::build) fails when two share an ID. Use
    /// [`replace_loader`](Self::replace_loader) to swap one on purpose.
    #[cfg(feature = "loaders")]
    pub fn loader(mut self, loader: Arc<dyn coblet_loader::ModLoader>) -> Self {
        self.loaders.changes.push(LoaderChange::Register(loader));
        self
    }

    /// Puts a Mod Loader in place of the one registered under the same ID,
    /// such as a patched copy of a built-in loader; registers it when the ID is free.
    #[cfg(feature = "loaders")]
    pub fn replace_loader(mut self, loader: Arc<dyn coblet_loader::ModLoader>) -> Self {
        self.loaders.changes.push(LoaderChange::Replace(loader));
        self
    }

    /// Registers a Mod Loader described in TOML, in the format of
    /// [`DataLoader`](coblet_loader::DataLoader); [`build`](CobletBuilder::build)
    /// reports a malformed definition.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::sync::Arc;
    /// use coblet::Coblet;
    /// use coblet::runtime::UserAgent;
    ///
    /// let coblet = Coblet::builder()
    ///     .data_root("/path/to/launcher-data")
    ///     .user_agent(UserAgent::new("MyLauncher/1.0 (contact@example.com)")?)
    ///     .loader_toml(r#"
    ///         id = "acme:loader"
    ///         name = "Acme Loader"
    ///         uid = "net.acme.loader"
    ///         [versions]
    ///         format = "json"
    ///         url = "https://meta.acme.net/versions/{game}"
    ///         id = "/version"
    ///         stable = "/stable"
    ///         [profile]
    ///         format = "version-json"
    ///         url = "https://meta.acme.net/versions/{game}/{version}/profile"
    ///     "#)
    ///     .build()?;
    /// assert!(coblet.loaders().get(&"acme:loader".parse()?).is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "loaders")]
    pub fn loader_toml(self, definition: &str) -> Self {
        self.data_loader(coblet_loader::DataLoader::from_toml(definition))
    }

    /// Registers a Mod Loader described in JSON, with the keys of the TOML
    /// form of [`loader_toml`](Self::loader_toml).
    #[cfg(feature = "loaders")]
    pub fn loader_json(self, definition: &str) -> Self {
        self.data_loader(coblet_loader::DataLoader::from_json(definition))
    }

    #[cfg(feature = "loaders")]
    fn data_loader(
        mut self,
        parsed: Result<coblet_loader::DataLoader, coblet_loader::LoaderError>,
    ) -> Self {
        match parsed {
            Ok(loader) => self.loader(Arc::new(loader)),
            Err(error) => {
                self.loaders.error.get_or_insert(error);
                self
            }
        }
    }

    /// Adds a version source for a Mod Loader, asked after the loader itself,
    /// such as a mirror listing or Prism's metadata server.
    /// [`build`](CobletBuilder::build) fails when no loader has the ID.
    #[cfg(feature = "loaders")]
    pub fn loader_version_source(
        mut self,
        loader: coblet_types::ExtensionId,
        source: Arc<dyn coblet_catalog::CatalogSource<coblet_loader::LoaderVersion>>,
    ) -> Self {
        self.loaders
            .changes
            .push(LoaderChange::Source(loader, source));
        self
    }

    /// Adds a Java provider written as code, asked after the providers
    /// already in the list when no installed Java fits.
    ///
    /// The built-in providers (Mojang, then Adoptium, then Azul Zulu) are
    /// registered the same way; [`build`](CobletBuilder::build) fails when
    /// the ID is taken, so use [`replace_java_provider`](Self::replace_java_provider)
    /// to swap one on purpose.
    #[cfg(feature = "java")]
    pub fn java_provider(mut self, provider: Arc<dyn coblet_java::JavaProvider>) -> Self {
        self.java.providers.push(Change::Register(provider));
        self
    }

    /// Puts a Java provider in place of the one with the same ID, keeping
    /// its place in the order; appends it when the ID is free.
    #[cfg(feature = "java")]
    pub fn replace_java_provider(mut self, provider: Arc<dyn coblet_java::JavaProvider>) -> Self {
        self.java.providers.push(Change::Replace(provider));
        self
    }

    /// Removes a Java provider, such as a built-in one the launcher must not
    /// download from; an unknown ID is ignored.
    #[cfg(feature = "java")]
    pub fn without_java_provider(mut self, id: coblet_types::ExtensionId) -> Self {
        self.java.providers.push(Change::Remove(id));
        self
    }

    /// Adds a Java provider described in TOML, in the format of
    /// [`DataJavaProvider`](coblet_java::DataJavaProvider);
    /// [`build`](CobletBuilder::build) reports a malformed definition.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// use coblet::{Coblet, runtime::UserAgent};
    ///
    /// let coblet = Coblet::builder()
    ///     .data_root("/path/to/launcher-data")
    ///     .user_agent(UserAgent::new("MyLauncher/1.0 (contact@example.com)")?)
    ///     .java_provider_toml(r#"
    ///         id = "acme:jdk"
    ///         name = "Acme JDK"
    ///
    ///         [packages]
    ///         format = "json"
    ///         url = "https://jdk.example.com/{os}/{arch}/{major}.json"
    ///         items = "/packages"
    ///         id = "/name"
    ///         version = "/version"
    ///         download = "/url"
    ///         hash = "/sha256"
    ///         hash_algorithm = "sha256"
    ///     "#)
    ///     .build()?;
    /// assert!(coblet.java_providers().get(&"acme:jdk".parse()?).is_some());
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "java")]
    pub fn java_provider_toml(self, definition: &str) -> Self {
        self.data_java_provider(coblet_java::DataJavaProvider::from_toml(definition))
    }

    /// Adds a Java provider described in JSON, with the keys of the TOML
    /// form of [`java_provider_toml`](Self::java_provider_toml).
    #[cfg(feature = "java")]
    pub fn java_provider_json(self, definition: &str) -> Self {
        self.data_java_provider(coblet_java::DataJavaProvider::from_json(definition))
    }

    #[cfg(feature = "java")]
    fn data_java_provider(
        mut self,
        parsed: Result<coblet_java::DataJavaProvider, coblet_java::JavaError>,
    ) -> Self {
        match parsed {
            Ok(provider) => self.java_provider(Arc::new(provider)),
            Err(error) => {
                self.java.error.get_or_insert(error);
                self
            }
        }
    }

    /// Adds a place to look for installed Java, searched after the places
    /// already in the list; [`build`](CobletBuilder::build) fails when the
    /// ID is taken.
    #[cfg(feature = "java")]
    pub fn java_locator(mut self, locator: Arc<dyn coblet_java::JavaLocator>) -> Self {
        self.java.locators.push(Change::Register(locator));
        self
    }

    /// Puts a Java locator in place of the one with the same ID, keeping its
    /// place in the order; appends it when the ID is free.
    #[cfg(feature = "java")]
    pub fn replace_java_locator(mut self, locator: Arc<dyn coblet_java::JavaLocator>) -> Self {
        self.java.locators.push(Change::Replace(locator));
        self
    }

    /// Removes a Java locator, such as `coblet:path` to ignore `PATH`;
    /// an unknown ID is ignored.
    #[cfg(feature = "java")]
    pub fn without_java_locator(mut self, id: coblet_types::ExtensionId) -> Self {
        self.java.locators.push(Change::Remove(id));
        self
    }

    /// Adds a `java` executable chosen by the user, searched before every
    /// other place.
    #[cfg(feature = "java")]
    pub fn java_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.java.executables.push(executable.into());
        self
    }

    /// Adds a folder holding one Java per child, such as another launcher's
    /// runtime folder; `home` leads from each child to its Java home, like
    /// `Contents/Home` on macOS or `""`.
    #[cfg(feature = "java")]
    pub fn java_directory(mut self, dir: impl Into<PathBuf>, home: impl Into<String>) -> Self {
        self.java.directories.push((dir.into(), home.into()));
        self
    }
}

impl CobletBuilder<WithRoot> {
    fn accounts(
        &mut self,
        data_root: &std::path::Path,
        transport: &Arc<dyn Transport>,
        storage: &Arc<dyn Storage>,
        events: &EventBus,
    ) -> Result<AccountStore, Error> {
        let secrets = self
            .auth
            .secrets
            .take()
            .unwrap_or_else(|| Arc::new(MemorySecretStore::new()));
        let ctx = AuthCtx::new(Arc::clone(transport)).with_events(events.clone());
        let mut store = AccountStore::new(Arc::clone(storage), data_root, secrets, ctx);
        #[cfg(feature = "auth-msa")]
        if let Some(config) = self.auth.msa.take() {
            store.register(Arc::new(coblet_auth::MicrosoftMethod::new(config)))?;
        }
        for method in std::mem::take(&mut self.auth.methods) {
            store.register(method)?;
        }
        Ok(store)
    }

    #[cfg(feature = "loaders")]
    fn loader_registry(&mut self) -> Result<coblet_loader::LoaderRegistry, Error> {
        let settings = std::mem::take(&mut self.loaders);
        if let Some(error) = settings.error {
            return Err(error.into());
        }
        let mut registry = coblet_loader::LoaderRegistry::new();
        let mut sources = Vec::new();
        for change in settings.changes {
            match change {
                LoaderChange::Register(loader) => registry.register(loader)?,
                LoaderChange::Replace(loader) => {
                    registry.replace(loader);
                }
                LoaderChange::Source(id, source) => sources.push((id, source)),
            }
        }
        for (id, source) in sources {
            if registry.get(&id).is_none() {
                return Err(coblet_loader::LoaderError::UnknownLoader(id).into());
            }
            registry.add_version_source(id, source);
        }
        Ok(registry)
    }

    /// Applies the Java changes to the built-in providers and locators and
    /// creates the installer for `data_root/runtimes`.
    #[cfg(feature = "java")]
    fn java(
        &mut self,
        data_root: &std::path::Path,
        downloader: &Downloader,
        storage: &Arc<dyn Storage>,
    ) -> Result<(coblet_java::JavaInstaller, coblet_java::JavaLocators), Error> {
        let runtimes = data_root.join("runtimes");
        let settings = std::mem::take(&mut self.java);
        if let Some(error) = settings.error {
            return Err(error.into());
        }
        let mut providers = coblet_java::JavaProviders::builtin();
        for change in settings.providers {
            match change {
                Change::Register(provider) => providers.register(provider)?,
                Change::Replace(provider) => {
                    providers.replace(provider);
                }
                Change::Remove(id) => {
                    providers.remove(&id);
                }
            }
        }
        let mut search = coblet_java::SearchPaths::from_env().with_runtimes(&runtimes);
        search.configured.extend(settings.executables);
        search.directories.extend(settings.directories);
        let mut locators = coblet_java::JavaLocators::builtin(search);
        for change in settings.locators {
            match change {
                Change::Register(locator) => locators.register(locator)?,
                Change::Replace(locator) => {
                    locators.replace(locator);
                }
                Change::Remove(id) => {
                    locators.remove(&id);
                }
            }
        }
        let installer = coblet_java::JavaInstaller::new(
            Arc::new(downloader.clone()),
            Arc::clone(storage),
            runtimes,
        )
        .with_providers(providers);
        Ok((installer, locators))
    }

    /// Returns the launcher with the customizers and auth grants applied.
    #[cfg(feature = "launch")]
    fn take_launcher(&mut self) -> Result<coblet_launch::Launcher, Error> {
        let mut launcher = self.launcher.take().unwrap_or_default();
        for customizer in std::mem::take(&mut self.customizers) {
            launcher.register_customizer(customizer)?;
        }
        for id in std::mem::take(&mut self.auth_grants) {
            launcher.allow_auth_changes(id)?;
        }
        Ok(launcher)
    }

    fn transport_or_default(&mut self) -> Result<Arc<dyn Transport>, Error> {
        if let Some(transport) = self.transport.take() {
            return Ok(transport);
        }
        #[cfg(feature = "tls-rustls")]
        {
            let user_agent = self.user_agent.as_ref().ok_or(Error::Config(
                "the default transport needs a user agent; call user_agent()",
            ))?;
            Ok(Arc::new(coblet_runtime::ReqwestTransport::new(user_agent)?))
        }
        #[cfg(not(feature = "tls-rustls"))]
        Err(Error::Config(
            "this build has no default transport; call transport()",
        ))
    }

    /// Creates the `Coblet`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] without a transport or User-Agent,
    /// [`Error::Transport`] when the default transport cannot start,
    /// [`Error::Platform`] when the platform cannot be detected and
    /// [`Error::Registry`] when two customizers or two auth methods share an ID,
    /// [`Error::Launch`] when [`allow_auth_changes`](CobletBuilder::allow_auth_changes)
    /// names a customizer that is not registered, [`Error::Profile`]
    /// when two Platform Profiles share an ID, and, with the `loaders`
    /// feature, [`Error::Registry`] when two Mod Loaders share an ID and
    /// [`Error::Loader`] for a malformed loader definition or a version
    /// source for a loader that is not registered. With the `java` feature
    /// it returns [`Error::Registry`] when two Java providers or two Java
    /// locators share an ID and [`Error::Java`] for a malformed provider
    /// definition. With the `mojang` feature it returns
    /// [`Error::Registry`] when two version trait sources or two game
    /// version sources share an ID, [`Error::Traits`] for a malformed trait
    /// table and [`Error::Config`] when every game version source was removed.
    pub fn build(mut self) -> Result<Coblet, Error> {
        #[cfg(feature = "loaders")]
        let loaders = self.loader_registry()?;
        #[cfg(feature = "launch")]
        let profiles = {
            let mut engine = self
                .profile_engine
                .take()
                .unwrap_or_else(coblet_platform::ProfileEngine::builtin);
            for profile in std::mem::take(&mut self.platform_profiles) {
                engine.register(profile)?;
            }
            engine
        };
        #[cfg(feature = "launch")]
        let launcher = self.take_launcher()?;
        let transport = self.transport_or_default()?;
        // The game and the Java runtime run with other working directories,
        // so every path derived from the data root must be absolute.
        let data_root = self.data_root.take().unwrap_or_default();
        let data_root = std::path::absolute(&data_root).unwrap_or(data_root);
        let storage = self
            .storage
            .take()
            .unwrap_or_else(|| Arc::new(TokioStorage::new()));
        let spawner = self
            .spawner
            .take()
            .unwrap_or_else(|| Arc::new(TokioSpawner::new()));
        let platform = match self.platform.take() {
            Some(platform) => platform,
            None => StdPlatformProbe.detect()?,
        };
        let events = self
            .events
            .take()
            .unwrap_or_else(|| EventBus::new(EVENT_CAPACITY));
        let rewriters = std::mem::take(&mut self.rewriters);
        let rewriters = if self.mirror_fallback {
            rewriters
        } else {
            rewriters.without_fallback()
        };
        let meta = MetaCache::new(
            Arc::clone(&transport),
            Arc::clone(&storage),
            data_root.join("meta"),
        )
        .with_rewriters(rewriters.clone());
        let config = DownloaderConfig::default()
            .with_limits(
                NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN),
                NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
            )
            .with_cache_root(data_root.join("cache"));
        let downloader = Downloader::new(Arc::clone(&transport), Arc::clone(&storage), config)
            .with_rewriters(rewriters);
        #[cfg(feature = "java")]
        let (java, java_locators) = self.java(&data_root, &downloader, &storage)?;
        let accounts = self.accounts(&data_root, &transport, &storage, &events)?;
        #[cfg(feature = "auth-msa")]
        let skins = coblet_player::SkinLibrary::new(
            Arc::clone(&storage),
            coblet_player::Player::new(Arc::clone(&transport)),
            data_root.join("skins"),
        );
        #[cfg(feature = "mojang")]
        let versions = self.versions.catalog(meta.clone(), events.clone())?;
        Ok(Coblet::from_inner(Inner {
            accounts,
            #[cfg(feature = "auth-msa")]
            skins,
            data_root,
            transport,
            storage,
            spawner,
            platform,
            events,
            meta,
            downloader,
            #[cfg(feature = "mojang")]
            versions,
            #[cfg(feature = "launch")]
            launcher,
            #[cfg(feature = "launch")]
            profiles,
            #[cfg(feature = "loaders")]
            loaders,
            #[cfg(feature = "java")]
            java,
            #[cfg(feature = "java")]
            java_locators,
            #[cfg(feature = "loaders")]
            loader_catalogs: std::sync::Mutex::default(),
        }))
    }
}
