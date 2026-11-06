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
    auth: AuthSettings,
    state: PhantomData<State>,
}

/// Version catalog settings collected by the builder.
#[cfg(feature = "mojang")]
#[derive(Default)]
pub(crate) struct VersionSettings {
    pub(crate) mojang: Option<coblet_mojang::MojangVersions>,
    pub(crate) sources: Vec<Arc<dyn coblet_catalog::CatalogSource<coblet_mojang::GameVersion>>>,
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
    /// manifest address or extra trait sources.
    #[cfg(feature = "mojang")]
    pub fn mojang_versions(mut self, source: coblet_mojang::MojangVersions) -> Self {
        self.versions.mojang = Some(source);
        self
    }

    /// Adds a game version source after the Mojang manifest, such as one
    /// listing experimental versions.
    ///
    /// When two sources list the same version ID, the earlier one wins.
    /// Launching a version fetches its version file from the address the
    /// source gave, checked against the source's SHA-1.
    #[cfg(feature = "mojang")]
    pub fn version_source(
        mut self,
        source: Arc<dyn coblet_catalog::CatalogSource<coblet_mojang::GameVersion>>,
    ) -> Self {
        self.versions.sources.push(source);
        self
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
    /// names a customizer that is not registered and [`Error::Profile`]
    /// when two Platform Profiles share an ID.
    pub fn build(mut self) -> Result<Coblet, Error> {
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
        let launcher = {
            let mut launcher = self.launcher.take().unwrap_or_default();
            for customizer in std::mem::take(&mut self.customizers) {
                launcher.register_customizer(customizer)?;
            }
            for id in std::mem::take(&mut self.auth_grants) {
                launcher.allow_auth_changes(id)?;
            }
            launcher
        };
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
        let accounts = self.accounts(&data_root, &transport, &storage, &events)?;
        #[cfg(feature = "auth-msa")]
        let skins = coblet_player::SkinLibrary::new(
            Arc::clone(&storage),
            coblet_player::Player::new(Arc::clone(&transport)),
            data_root.join("skins"),
        );
        Ok(Coblet::from_inner(Inner {
            accounts,
            #[cfg(feature = "auth-msa")]
            skins,
            #[cfg(feature = "mojang")]
            versions: crate::versions::version_catalog(
                meta.clone(),
                events.clone(),
                std::mem::take(&mut self.versions),
            ),
            #[cfg(feature = "launch")]
            launcher,
            #[cfg(feature = "launch")]
            profiles,
            data_root,
            transport,
            storage,
            spawner,
            platform,
            events,
            meta,
            downloader,
        }))
    }
}
