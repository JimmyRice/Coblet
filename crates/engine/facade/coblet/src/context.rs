use std::path::{Path, PathBuf};
use std::sync::Arc;

use coblet_catalog::MetaCache;
#[cfg(any(feature = "java", feature = "mojang"))]
use coblet_catalog::{CachePolicy, CatalogContext};
use coblet_download::Downloader;
use coblet_runtime::{EventBus, EventSubscription, ProcessSpawner, Storage, Transport};
use coblet_types::Platform;

/// Entry point of coblet: shared services, the version catalog and launching.
///
/// Cloning is cheap; clones share everything. Every subsystem can also be
/// built without a `Coblet` from the trait objects it needs.
///
/// # Examples
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use coblet::{Coblet, runtime::UserAgent};
///
/// let coblet = Coblet::builder()
///     .data_root("/path/to/launcher-data")
///     .user_agent(UserAgent::new("MyLauncher/1.0 (contact@example.com)")?)
///     .build()?;
/// let latest = coblet.latest_release().await?;
/// println!("latest release: {}", latest.id);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Coblet {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) accounts: coblet_auth::AccountStore,
    #[cfg(feature = "auth-msa")]
    pub(crate) skins: coblet_player::SkinLibrary,
    pub(crate) data_root: PathBuf,
    pub(crate) transport: Arc<dyn Transport>,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) spawner: Arc<dyn ProcessSpawner>,
    pub(crate) platform: Platform,
    pub(crate) events: EventBus,
    pub(crate) meta: MetaCache,
    pub(crate) downloader: Downloader,
    #[cfg(feature = "mojang")]
    pub(crate) versions: crate::versions::VersionCatalog,
    #[cfg(feature = "launch")]
    pub(crate) launcher: coblet_launch::Launcher,
    #[cfg(feature = "launch")]
    pub(crate) profiles: coblet_platform::ProfileEngine,
    #[cfg(feature = "loaders")]
    pub(crate) loaders: coblet_loader::LoaderRegistry,
    /// Installs Java runtimes from the providers, under `data_root/runtimes`.
    #[cfg(feature = "java")]
    pub(crate) java: coblet_java::JavaInstaller,
    /// Places searched for installed Java.
    #[cfg(feature = "java")]
    pub(crate) java_locators: coblet_java::JavaLocators,
    /// Version catalogs of the Mod Loaders, created on first use.
    #[cfg(feature = "loaders")]
    pub(crate) loader_catalogs: std::sync::Mutex<
        std::collections::BTreeMap<
            coblet_types::ExtensionId,
            Arc<coblet_catalog::Catalog<coblet_loader::LoaderVersion>>,
        >,
    >,
}

impl std::fmt::Debug for Coblet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coblet")
            .field("data_root", &self.inner.data_root)
            .field("platform", &self.inner.platform)
            .finish_non_exhaustive()
    }
}

impl Coblet {
    pub(crate) fn from_inner(inner: Inner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Returns a metadata context that prefers cached documents.
    #[cfg(any(feature = "java", feature = "mojang"))]
    pub(crate) fn catalog_context(&self) -> CatalogContext {
        CatalogContext::new(self.inner.meta.clone(), CachePolicy::PreferCache)
    }

    /// Publishes the warnings of failed trait sources and any left in `cx`.
    #[cfg(feature = "mojang")]
    pub(crate) fn forward_trait_failures(
        &self,
        cx: &CatalogContext,
        failures: &[coblet_mojang::TraitFailure],
    ) {
        for failure in failures {
            cx.warn(failure.to_warning());
        }
        self.forward_warnings(cx);
    }

    /// Publishes the warnings collected by a metadata context.
    #[cfg(any(feature = "java", feature = "mojang"))]
    pub(crate) fn forward_warnings(&self, cx: &CatalogContext) {
        for warning in cx.take_warnings() {
            self.inner
                .events
                .publish(coblet_runtime::Event::Warning(warning));
        }
    }

    /// Returns the data root.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.inner.data_root
    }

    /// Returns the platform launches are prepared for.
    #[must_use]
    pub fn platform(&self) -> &Platform {
        &self.inner.platform
    }

    /// Returns the transport.
    #[must_use]
    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.inner.transport
    }

    /// Returns the file access.
    #[must_use]
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.inner.storage
    }

    /// Returns the process spawner.
    #[must_use]
    pub fn spawner(&self) -> &Arc<dyn ProcessSpawner> {
        &self.inner.spawner
    }

    /// Returns the metadata cache.
    #[must_use]
    pub fn meta(&self) -> &MetaCache {
        &self.inner.meta
    }

    /// Returns the downloader.
    #[must_use]
    pub fn downloader(&self) -> &Downloader {
        &self.inner.downloader
    }

    /// Returns the event bus.
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.inner.events
    }

    /// Returns the launcher with its steps, customizers and the customizers
    /// allowed to change authentication arguments.
    #[cfg(feature = "launch")]
    #[must_use]
    pub fn launcher(&self) -> &coblet_launch::Launcher {
        &self.inner.launcher
    }

    /// Subscribes to events: Operation and launch progress, file transfer
    /// totals, warnings, login prompts, game starts, log lines and exits.
    ///
    /// A launch's events carry the ID of its `LaunchRequest`; choose it
    /// with `LaunchRequest::with_operation` to follow one launch among
    /// several. Events nobody subscribes to are not built. Every
    /// subscriber has its own queue: a newer transfer or step progress
    /// replaces the queued one of the same batch, and a subscriber that
    /// still falls behind loses game log copies first, then progress, and
    /// only then other events, learning how many with
    /// [`Received::Missed`](coblet_runtime::Received::Missed) instead of
    /// slowing the work down.
    #[must_use]
    pub fn subscribe(&self) -> EventSubscription {
        self.inner.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use coblet_runtime::{
        BoxFuture, MemoryBody, Request, Response, Transport, TransportError, UserAgent,
    };
    use coblet_types::{Arch, ExtensionId, OsFamily, Platform};

    use crate::{Coblet, Error};

    /// Serves the Mojang fixtures by file name.
    struct Fixtures;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../fixtures/mojang")
    }

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
                let name = url.rsplit('/').next().unwrap_or_default();
                Ok(match std::fs::read(fixtures().join(name)) {
                    Ok(bytes) => Response::new(200, Vec::new(), MemoryBody::new(bytes)),
                    Err(_) => Response::new(404, Vec::new(), MemoryBody::new(Vec::new())),
                })
            })
        }
    }

    fn coblet(root: &std::path::Path) -> Coblet {
        Coblet::builder()
            .data_root(root)
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .build()
            .unwrap()
    }

    #[test]
    fn the_default_transport_needs_a_user_agent() {
        let error = Coblet::builder().data_root("/tmp/x").build().unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{error}");
        let coblet = Coblet::builder()
            .data_root("/tmp/x")
            .user_agent(UserAgent::new("coblet-test/1.0").unwrap())
            .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
            .build()
            .unwrap();
        assert_eq!(coblet.data_root(), std::path::Path::new("/tmp/x"));
        let relative = Coblet::builder()
            .data_root("data")
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
            .build()
            .unwrap();
        assert!(relative.data_root().is_absolute());
    }

    #[tokio::test]
    async fn lists_versions_and_resolves_one() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(dir.path());
        let versions = coblet
            .versions(&coblet_catalog::Filter::new())
            .await
            .unwrap();
        assert!(versions.len() > 5);
        assert_eq!(coblet.latest_release().await.unwrap().id.as_str(), "26.3");
        assert!(matches!(
            coblet.version("0.0").await,
            Err(Error::UnknownVersion(_))
        ));

        let version = coblet.version("1.20.1").await.unwrap();
        let profile = coblet.resolve(&version).await.unwrap();
        let ids: Vec<&str> = profile.profiles.iter().map(ExtensionId::as_str).collect();
        assert_eq!(ids, ["coblet:mojang-macos-arm64", "coblet:macos-lwjgl3"]);
        assert_eq!(profile.java.major, 17);
    }

    struct Noop(ExtensionId);

    impl coblet_runtime::Extension for Noop {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Noop"
        }
    }

    impl coblet_launch::LaunchCustomizer for Noop {
        fn applies_to(&self, _traits: &coblet_mojang::VersionTraits) -> bool {
            true
        }

        fn customize(
            &self,
            _profile: &mut coblet_platform::EffectiveLaunchProfile,
            _ctx: &coblet_launch::CustomizeCtx,
        ) -> Result<(), coblet_launch::LaunchError> {
            Ok(())
        }
    }

    #[test]
    fn customizers_need_distinct_ids() {
        let id = ExtensionId::new("acme:noop").unwrap();
        let builder = || {
            Coblet::builder()
                .data_root("/tmp/x")
                .transport(Arc::new(Fixtures))
                .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
                .customizer(Arc::new(Noop(id.clone())))
        };
        let error = builder()
            .customizer(Arc::new(Noop(id.clone())))
            .build()
            .unwrap_err();
        assert!(matches!(error, Error::Registry(_)), "{error}");
        let coblet = builder().build().unwrap();
        assert!(coblet.inner.launcher.customizers().get(&id).is_some());
    }

    #[test]
    fn auth_grants_name_registered_customizers() {
        let id = ExtensionId::new("acme:login").unwrap();
        let builder = || {
            Coblet::builder()
                .data_root("/tmp/x")
                .transport(Arc::new(Fixtures))
                .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
        };
        let coblet = builder()
            .allow_auth_changes(id.clone())
            .customizer(Arc::new(Noop(id.clone())))
            .build()
            .unwrap();
        assert!(coblet.launcher().allows_auth_changes(&id));
        let coblet = builder()
            .customizer(Arc::new(Noop(id.clone())))
            .build()
            .unwrap();
        assert!(!coblet.launcher().allows_auth_changes(&id));
        let error = builder().allow_auth_changes(id).build().unwrap_err();
        assert!(
            matches!(
                &error,
                Error::Launch(coblet_launch::LaunchError::UnknownCustomizer(gone)) if gone.as_str() == "acme:login"
            ),
            "{error}"
        );
    }

    /// Profile a community could publish for an architecture coblet does not name.
    const LOONGARCH: &str = r#"
id = "community:linux-loongarch64"

[match]
os = ["linux"]
arch = ["loongarch64"]
requires_component = { uid = "org.lwjgl3" }

[rules]
mojang_os = "linux"

[[natives]]
match = "org.lwjgl:*:*:natives-linux"
source = { maven = "https://maven.example.org/", classifier = "natives-linux-loongarch64" }

[java]
source = "system-only"
"#;

    fn loongarch(root: &std::path::Path) -> crate::CobletBuilder<crate::WithRoot> {
        Coblet::builder()
            .data_root(root)
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(
                OsFamily::Linux,
                "loongarch64".parse().unwrap(),
            ))
    }

    #[tokio::test]
    async fn registered_profiles_adapt_unknown_architectures() {
        let dir = tempfile::tempdir().unwrap();
        let plain = loongarch(dir.path()).build().unwrap();
        let version = plain.version("1.20.1").await.unwrap();
        let error = plain.resolve(&version).await.unwrap_err();
        assert!(matches!(error, Error::Profile(_)), "{error}");

        let profile = coblet_platform::DataProfile::from_toml(
            LOONGARCH,
            coblet_platform::ProfileLayer::ThirdParty,
        )
        .unwrap();
        let extended = loongarch(dir.path())
            .platform_profile(Arc::new(profile))
            .build()
            .unwrap();
        let resolved = extended.resolve(&version).await.unwrap();
        let ids: Vec<&str> = resolved.profiles.iter().map(ExtensionId::as_str).collect();
        assert_eq!(ids, ["community:linux-loongarch64"]);
        assert_eq!(
            resolved.java.source,
            coblet_platform::JavaSource::SystemOnly
        );
        let natives: Vec<&str> = resolved
            .classpath()
            .map(|file| file.path.as_str())
            .filter(|path| path.contains("natives-linux"))
            .collect();
        assert!(!natives.is_empty(), "{}", resolved.explain());
        assert!(
            natives
                .iter()
                .all(|path| path.ends_with("-natives-linux-loongarch64.jar")),
            "{natives:?}"
        );
        assert!(
            extended
                .profile_engine()
                .profiles()
                .iter()
                .any(|known| known.id().as_str() == "community:linux-loongarch64")
        );
    }

    #[test]
    fn platform_profiles_need_distinct_ids() {
        let profile = || {
            Arc::new(
                coblet_platform::DataProfile::from_toml(
                    LOONGARCH,
                    coblet_platform::ProfileLayer::ThirdParty,
                )
                .unwrap(),
            )
        };
        let error = loongarch(std::path::Path::new("/tmp/x"))
            .platform_profile(profile())
            .platform_profile(profile())
            .build()
            .unwrap_err();
        assert!(matches!(error, Error::Profile(_)), "{error}");
    }

    /// Serves the fixtures and records every requested address.
    #[derive(Default)]
    struct Recording(std::sync::Mutex<Vec<String>>);

    impl Transport for Recording {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            self.0
                .lock()
                .unwrap()
                .push(request.url().as_str().to_owned());
            Fixtures.send(request)
        }
    }

    #[tokio::test]
    async fn rewriters_reach_metadata_requests() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Recording::default());
        let coblet = Coblet::builder()
            .data_root(dir.path())
            .transport(transport.clone())
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .url_rewriter(Arc::new(
                coblet_runtime::PrefixRewriter::new(
                    "https://piston-meta.mojang.com/",
                    "https://mirror.example/piston-meta/",
                )
                .unwrap(),
            ))
            .build()
            .unwrap();
        let version = coblet.version("1.20.1").await.unwrap();
        coblet.resolve(&version).await.unwrap();
        let urls = transport.0.lock().unwrap().clone();
        assert!(urls.len() >= 2, "{urls:?}");
        assert!(
            urls.iter()
                .all(|url| url.starts_with("https://mirror.example/piston-meta/")),
            "{urls:?}"
        );
    }

    /// Lists one experimental version served from another address.
    struct Experiments(ExtensionId);

    impl coblet_runtime::Extension for Experiments {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Experiments"
        }
    }

    impl coblet_catalog::CatalogSource<coblet_mojang::GameVersion> for Experiments {
        fn fetch<'a>(
            &'a self,
            _cx: &'a coblet_catalog::CatalogContext,
            _query: &'a coblet_catalog::CatalogQuery,
        ) -> BoxFuture<
            'a,
            Result<
                coblet_catalog::CatalogPage<coblet_mojang::GameVersion>,
                coblet_catalog::CatalogError,
            >,
        > {
            Box::pin(async {
                let version = coblet_mojang::GameVersion::new(
                    coblet_types::GameVersionId::new("1.16_combat-6").unwrap(),
                    coblet_types::VersionType::Experiment,
                    coblet_runtime::HttpUrl::new("https://meta.example/1.16_combat-6.json")
                        .unwrap(),
                    coblet_types::Sha1Digest::from_hex("a9993e364706816aba3e25717850c26c9cd0d89d")
                        .unwrap(),
                    "2020-08-14T12:00:00Z".parse().unwrap(),
                );
                Ok(coblet_catalog::CatalogPage::new(vec![version]))
            })
        }
    }

    #[tokio::test]
    async fn added_version_sources_join_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = Coblet::builder()
            .data_root(dir.path())
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .version_source(Arc::new(Experiments(
                ExtensionId::new("acme:experiments").unwrap(),
            )))
            .build()
            .unwrap();
        let experiment = coblet.version("1.16_combat-6").await.unwrap();
        assert_eq!(
            experiment.version_type,
            coblet_types::VersionType::Experiment
        );
        assert_eq!(coblet.latest_release().await.unwrap().id.as_str(), "26.3");
    }

    #[test]
    fn mirror_fallback_can_be_turned_off() {
        let build = |fallback: bool| {
            Coblet::builder()
                .data_root("/tmp/coblet-fallback")
                .transport(Arc::new(Fixtures))
                .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
                .mirror_fallback(fallback)
                .build()
                .unwrap()
        };
        let on = build(true);
        let off = build(false);
        assert!(on.meta().rewriters().falls_back());
        assert!(on.downloader().rewriters().falls_back());
        assert!(!off.meta().rewriters().falls_back());
        assert!(!off.downloader().rewriters().falls_back());
    }
}
