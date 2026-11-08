use std::sync::{Arc, Mutex, PoisonError, mpsc};

use coblet::catalog::{
    CatalogContext, CatalogError, CatalogPage, CatalogQuery, CatalogSource, Timestamp,
};
use coblet::loader::LoaderVersion;
use coblet::mojang::{
    GameVersion, VersionTrait, VersionTraits, fetch_for_source, keep_version_json,
};
use coblet::runtime::{BoxFuture, Extension, HttpUrl};
use coblet::types::{ExtensionId, GameVersionId, Sha1Digest, VersionType};

use crate::java::extension_id;
use crate::loaders::{LoaderVersionInfo, loader_id};
use crate::{Coblet, CobletError, ErrorKind};

/// Game version or Mod Loader version source known to a [`Coblet`].
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VersionSourceInfo {
    /// ID, such as `coblet:mojang`.
    pub id: String,
    /// Name shown to users.
    pub name: String,
}

/// One game version a [`GameVersionSource`] lists.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GameVersionEntry {
    /// Version ID, such as `1.20.1`.
    pub id: String,
    /// Release channel, such as `release`, `snapshot` or `old_beta`.
    #[uniffi(default = "release")]
    pub version_type: String,
    /// Address of the version JSON, in Mojang's format.
    pub url: String,
    /// SHA-1 of the version JSON, checked when the version is launched.
    pub sha1: String,
    /// Release time in RFC 3339.
    pub released: String,
    /// Whether the source names this version as its newest release.
    #[uniffi(default = false)]
    pub latest_release: bool,
    /// Version traits the source declares; the trait sources run after.
    #[uniffi(default = [])]
    pub traits: Vec<String>,
}

impl GameVersionEntry {
    fn into_version(self) -> Result<GameVersion, String> {
        let id = GameVersionId::new(&self.id).map_err(|error| format!("id: {error}"))?;
        let url = HttpUrl::new(&self.url).map_err(|error| format!("url: {error}"))?;
        let sha1 = Sha1Digest::from_hex(&self.sha1).map_err(|error| format!("sha1: {error}"))?;
        let released =
            Timestamp::parse(&self.released).map_err(|error| format!("released: {error}"))?;
        let traits = self
            .traits
            .into_iter()
            .map(|name| {
                if name.is_empty() || name.chars().any(char::is_whitespace) {
                    Err(format!("invalid trait name `{name}`"))
                } else {
                    Ok(VersionTrait::from(name))
                }
            })
            .collect::<Result<VersionTraits, _>>()?;
        let mut version = GameVersion::new(
            id,
            VersionType::from(self.version_type),
            url,
            sha1,
            released,
        )
        .with_traits(traits);
        version.latest_release = self.latest_release;
        Ok(version)
    }
}

/// What a [`SourceContext`] fetches for, which decides the cache folder
/// and the kind of request mirror rules see.
#[derive(Clone, Copy, Debug)]
enum Fetches {
    GameVersions,
    LoaderVersions,
}

/// Requests a version source written in Swift, Kotlin or Python makes.
///
/// Documents come through coblet's metadata cache, so mirror rules,
/// fallback to the original address and offline caching apply. Each
/// source's documents are cached apart: those of a game version source
/// under `sources/<namespace>/<name>/`, those of a Mod Loader version
/// source as Mod Loader metadata under `loaders/<namespace>/<name>/`.
#[derive(uniffi::Object)]
pub struct SourceContext {
    catalog: CatalogContext,
    source: ExtensionId,
    fetches: Fetches,
    runtime: tokio::runtime::Handle,
    /// Documents a game version source fetched, offered to the launch
    /// cache once it has listed its versions.
    fetched: Mutex<Vec<bytes::Bytes>>,
}

impl std::fmt::Debug for SourceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceContext")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl SourceContext {
    async fn get(&self, url: &str, cache_name: &str) -> Result<Vec<u8>, coblet::Error> {
        let body = match self.fetches {
            Fetches::GameVersions => {
                let body = fetch_for_source(&self.catalog, &self.source, url, cache_name).await?;
                self.fetched
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(body.clone());
                body
            }
            Fetches::LoaderVersions => {
                coblet::loader::LoaderContext::new(&self.catalog, &self.source)
                    .fetch(url, cache_name)
                    .await?
            }
        };
        Ok(body.to_vec())
    }
}

#[uniffi::export]
impl SourceContext {
    /// Returns the ID of the source the requests are made for.
    #[must_use]
    pub fn source_id(&self) -> String {
        self.source.as_str().to_owned()
    }

    /// Fetches a document and caches it under `cache_name`, a relative path
    /// such as `versions.json`.
    ///
    /// The call blocks until the document is there; call it from the
    /// thread `versions` runs on.
    ///
    /// # Errors
    ///
    /// Fails for a malformed address or cache name and when the document is
    /// unavailable and not cached.
    pub fn fetch(self: Arc<Self>, url: String, cache_name: String) -> Result<Vec<u8>, CobletError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = Arc::clone(&self);
        self.runtime.spawn(async move {
            let _ = sender.send(context.get(&url, &cache_name).await);
        });
        let body = receiver
            .recv()
            .map_err(|_| CobletError::new(ErrorKind::Other, "the request was dropped"))?
            .map_err(CobletError::from)?;
        Ok(body)
    }

    /// Fetches a document like [`fetch`](Self::fetch) and returns it as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Fails like [`fetch`](Self::fetch), and when the document is not UTF-8.
    pub fn fetch_text(
        self: Arc<Self>,
        url: String,
        cache_name: String,
    ) -> Result<String, CobletError> {
        String::from_utf8(self.fetch(url, cache_name)?)
            .map_err(|error| CobletError::new(ErrorKind::Other, format!("not UTF-8: {error}")))
    }
}

/// Game version source written in Swift, Kotlin or Python, registered with
/// `CobletBuilder.add_version_source` after the Mojang manifest.
///
/// `versions` runs on a worker thread where blocking is fine, once each
/// time the version list is loaded or refreshed. When two sources list the
/// same version ID, the earlier one wins. Launching a version fetches its
/// version JSON from the entry's `url` and checks it against `sha1`; a
/// version JSON the source already fetched through its context to hash it
/// is kept for the launch, which then does not download it again.
#[uniffi::export(foreign)]
pub trait GameVersionSource: Send + Sync {
    /// Namespaced ID, such as `acme:experiments`.
    fn id(&self) -> String;

    /// Name shown to users.
    fn display_name(&self) -> String;

    /// Lists the source's versions, newest first.
    ///
    /// # Errors
    ///
    /// Return an error when the versions cannot be listed; the other
    /// sources are still listed and a `catalog.source-failed` warning is
    /// published.
    fn versions(&self, cx: Arc<SourceContext>) -> Result<Vec<GameVersionEntry>, CobletError>;
}

/// Mod Loader version source written in Swift, Kotlin or Python,
/// registered with `CobletBuilder.add_loader_version_source`, such as a
/// mirror's listing. It is asked after the loader itself and the sources
/// added before it; for the same version string the earlier entry wins.
#[uniffi::export(foreign)]
pub trait LoaderVersionSource: Send + Sync {
    /// Namespaced ID, such as `acme:nightly`.
    fn id(&self) -> String;

    /// Name shown to users.
    fn display_name(&self) -> String;

    /// Lists the versions for the game version `game`.
    ///
    /// # Errors
    ///
    /// Return an error when the versions cannot be listed; the other
    /// sources are still listed and a `catalog.source-failed` warning is
    /// published.
    fn versions(
        &self,
        cx: Arc<SourceContext>,
        game: String,
    ) -> Result<Vec<LoaderVersionInfo>, CobletError>;
}

/// Runs a call of a foreign source on a blocking thread with its own
/// context, then hands the context's warnings to `cx`; returns the answer
/// and the documents a game version source fetched.
async fn call<T: Send + 'static>(
    cx: &CatalogContext,
    source: &ExtensionId,
    fetches: Fetches,
    body: impl FnOnce(Arc<SourceContext>) -> Result<T, CobletError> + Send + 'static,
) -> Result<(T, Vec<bytes::Bytes>), CatalogError> {
    let context = Arc::new(SourceContext {
        catalog: CatalogContext::new(cx.meta().clone(), cx.policy()),
        source: source.clone(),
        fetches,
        runtime: tokio::runtime::Handle::current(),
        fetched: Mutex::new(Vec::new()),
    });
    let given = Arc::clone(&context);
    let result = tokio::task::spawn_blocking(move || body(given)).await;
    for warning in context.catalog.take_warnings() {
        cx.warn(warning);
    }
    let documents = std::mem::take(
        &mut *context
            .fetched
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    match result {
        Ok(Ok(value)) => Ok((value, documents)),
        Ok(Err(error)) => Err(CatalogError::Failed(format!(
            "version source `{source}` failed: {error}"
        ))),
        Err(error) => Err(CatalogError::Failed(format!(
            "version source `{source}` panicked: {error}"
        ))),
    }
}

/// Adapts a [`GameVersionSource`] from foreign code to the engine's trait.
pub(crate) struct ForeignGameVersions {
    id: ExtensionId,
    name: String,
    source: Arc<dyn GameVersionSource>,
}

impl ForeignGameVersions {
    pub(crate) fn new(source: Arc<dyn GameVersionSource>) -> Result<Self, CobletError> {
        Ok(Self {
            id: extension_id("version source", &source.id())?,
            name: source.display_name(),
            source,
        })
    }
}

impl Extension for ForeignGameVersions {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl CatalogSource<GameVersion> for ForeignGameVersions {
    fn fetch<'a>(
        &'a self,
        cx: &'a CatalogContext,
        _query: &'a CatalogQuery,
    ) -> BoxFuture<'a, Result<CatalogPage<GameVersion>, CatalogError>> {
        Box::pin(async move {
            let source = Arc::clone(&self.source);
            let (entries, fetched) = call(cx, &self.id, Fetches::GameVersions, move |context| {
                source.versions(context)
            })
            .await?;
            let versions = entries
                .into_iter()
                .map(|entry| {
                    let id = entry.id.clone();
                    entry.into_version().map_err(|error| {
                        CatalogError::Failed(format!(
                            "version source `{}` listed `{id}` with a bad {error}",
                            self.id
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            // A source that hashed version JSON to list it saves the launch a download.
            if let Err(error) = keep_version_json(cx, &versions, &fetched).await {
                tracing::warn!(source = %self.id, %error, "could not keep fetched version JSON");
            }
            Ok(CatalogPage::new(versions))
        })
    }
}

/// Adapts a [`LoaderVersionSource`] from foreign code to the engine's trait.
pub(crate) struct ForeignLoaderVersions {
    id: ExtensionId,
    name: String,
    source: Arc<dyn LoaderVersionSource>,
}

impl ForeignLoaderVersions {
    pub(crate) fn new(source: Arc<dyn LoaderVersionSource>) -> Result<Self, CobletError> {
        Ok(Self {
            id: extension_id("version source", &source.id())?,
            name: source.display_name(),
            source,
        })
    }
}

impl Extension for ForeignLoaderVersions {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl CatalogSource<LoaderVersion> for ForeignLoaderVersions {
    fn fetch<'a>(
        &'a self,
        cx: &'a CatalogContext,
        query: &'a CatalogQuery,
    ) -> BoxFuture<'a, Result<CatalogPage<LoaderVersion>, CatalogError>> {
        Box::pin(async move {
            let Some(game) = query.game.clone() else {
                return Err(CatalogError::Failed(
                    "mod loader versions are listed per game version".to_owned(),
                ));
            };
            let source = Arc::clone(&self.source);
            let asked = game.as_str().to_owned();
            let (entries, _) = call(cx, &self.id, Fetches::LoaderVersions, move |context| {
                source.versions(context, asked)
            })
            .await?;
            let versions = entries
                .into_iter()
                .map(|entry| {
                    let id = entry.id.clone();
                    let mut version = entry.into_version().map_err(|error| {
                        CatalogError::Failed(format!(
                            "version source `{}` listed `{id}` with a bad {error}",
                            self.id
                        ))
                    })?;
                    version.game.get_or_insert_with(|| game.clone());
                    Ok(version)
                })
                .collect::<Result<Vec<_>, CatalogError>>()?;
            Ok(CatalogPage::new(versions))
        })
    }
}

fn infos<'a, Item: 'static>(
    sources: impl Iterator<Item = &'a Arc<dyn CatalogSource<Item>>>,
) -> Vec<VersionSourceInfo> {
    sources
        .map(|source| VersionSourceInfo {
            id: source.id().as_str().to_owned(),
            name: source.display_name().to_owned(),
        })
        .collect()
}

#[uniffi::export]
impl Coblet {
    /// Lists the game version sources, in the order they are asked.
    #[must_use]
    pub fn version_sources(&self) -> Vec<VersionSourceInfo> {
        infos(self.inner.version_sources().iter())
    }

    /// Lists the extra version sources of a Mod Loader, in the order they
    /// are asked after the loader itself.
    ///
    /// # Errors
    ///
    /// Fails for a malformed loader ID and an unknown loader.
    pub fn loader_version_sources(
        &self,
        loader: &str,
    ) -> Result<Vec<VersionSourceInfo>, CobletError> {
        let id = loader_id(loader)?;
        let loaders = self.inner.loaders();
        if loaders.get(&id).is_none() {
            return Err(coblet::Error::from(coblet::loader::LoaderError::UnknownLoader(id)).into());
        }
        Ok(loaders
            .version_sources(&id)
            .map(|sources| infos(sources.iter()))
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use coblet::runtime::{BoxFuture, MemoryBody, Request, Response, Transport, TransportError};
    use coblet::types::{Arch, ExtensionId, OsFamily, Platform};

    use super::{
        ForeignGameVersions, ForeignLoaderVersions, GameVersionEntry, GameVersionSource,
        LoaderVersionSource, SourceContext,
    };
    use crate::{Coblet, CobletBuilder, CobletError, ErrorKind, LoaderVersionInfo};

    /// Serves the Mojang fixtures by file name from any host.
    struct Fixtures;

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
                let name = url.rsplit('/').next().unwrap_or_default();
                let dir =
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/mojang");
                Ok(match std::fs::read(dir.join(name)) {
                    Ok(bytes) => Response::new(200, Vec::new(), MemoryBody::new(bytes)),
                    Err(_) => Response::new(404, Vec::new(), MemoryBody::new(Vec::new())),
                })
            })
        }
    }

    /// Lists b1.7.3 from its own host, with the SHA-1 its manifest gives,
    /// after downloading the version JSON as a source that hashes it would.
    struct Own;

    impl GameVersionSource for Own {
        fn id(&self) -> String {
            "test:own".to_owned()
        }

        fn display_name(&self) -> String {
            "Own versions".to_owned()
        }

        fn versions(&self, cx: Arc<SourceContext>) -> Result<Vec<GameVersionEntry>, CobletError> {
            assert_eq!(cx.source_id(), "test:own");
            let text = Arc::clone(&cx).fetch_text(
                "https://own.fake.test/version_manifest_v2.json".to_owned(),
                "manifest.json".to_owned(),
            )?;
            let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
            let beta = manifest["versions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == "b1.7.3")
                .unwrap();
            cx.fetch(
                "https://own.fake.test/b1.7.3.json".to_owned(),
                "b1.7.3.json".to_owned(),
            )?;
            Ok(vec![GameVersionEntry {
                id: "b1.7.3".to_owned(),
                version_type: "old_beta".to_owned(),
                url: "https://own.fake.test/b1.7.3.json".to_owned(),
                sha1: beta["sha1"].as_str().unwrap().to_owned(),
                released: beta["releaseTime"].as_str().unwrap().to_owned(),
                latest_release: true,
                traits: vec!["acme:declared".to_owned()],
            }])
        }
    }

    /// Lists a version with a malformed SHA-1.
    struct Bad;

    impl GameVersionSource for Bad {
        fn id(&self) -> String {
            "test:bad".to_owned()
        }

        fn display_name(&self) -> String {
            "Bad".to_owned()
        }

        fn versions(&self, _cx: Arc<SourceContext>) -> Result<Vec<GameVersionEntry>, CobletError> {
            Ok(vec![GameVersionEntry {
                id: "x".to_owned(),
                version_type: "release".to_owned(),
                url: "https://own.fake.test/x.json".to_owned(),
                sha1: "nope".to_owned(),
                released: "2020-01-01T00:00:00Z".to_owned(),
                latest_release: false,
                traits: Vec::new(),
            }])
        }
    }

    /// Lists a newer beta and a copy of the loader's own version.
    struct Nightly;

    impl LoaderVersionSource for Nightly {
        fn id(&self) -> String {
            "test:nightly".to_owned()
        }

        fn display_name(&self) -> String {
            "Nightly".to_owned()
        }

        fn versions(
            &self,
            _cx: Arc<SourceContext>,
            game: String,
        ) -> Result<Vec<LoaderVersionInfo>, CobletError> {
            assert_eq!(game, "1.20.1");
            let entry = |id: &str, kind: &str| LoaderVersionInfo {
                id: id.to_owned(),
                game: None,
                version_type: kind.to_owned(),
                released: None,
                recommended: false,
                data_json: None,
                source: Some("ignored".to_owned()),
            };
            Ok(vec![entry("2.0", "beta"), entry("1.0", "beta")])
        }
    }

    /// Always fails.
    struct Down;

    impl LoaderVersionSource for Down {
        fn id(&self) -> String {
            "test:down".to_owned()
        }

        fn display_name(&self) -> String {
            "Down".to_owned()
        }

        fn versions(
            &self,
            _cx: Arc<SourceContext>,
            _game: String,
        ) -> Result<Vec<LoaderVersionInfo>, CobletError> {
            Err(CobletError::new(ErrorKind::Other, "offline"))
        }
    }

    const LOADER: &str = r#"
        id = "test:fake"
        name = "Fake"
        uid = "test.fake"
        [versions]
        format = "static"
        list = [{ id = "1.0" }]
        [profile]
        format = "static"
        [[profile.components]]
        uid = "test.fake"
        version = "{version}"
    "#;

    fn id(text: &str) -> ExtensionId {
        ExtensionId::new(text).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn foreign_sources_list_game_and_loader_versions() {
        let dir = tempfile::tempdir().unwrap();
        let loader = id("test:fake");
        let inner = coblet::Coblet::builder()
            .data_root(dir.path())
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .without_version_source(id("coblet:mojang"))
            .version_source(Arc::new(ForeignGameVersions::new(Arc::new(Own)).unwrap()))
            .loader_toml(LOADER)
            .loader_version_source(
                loader.clone(),
                Arc::new(ForeignLoaderVersions::new(Arc::new(Nightly)).unwrap()),
            )
            .loader_version_source(
                loader,
                Arc::new(ForeignLoaderVersions::new(Arc::new(Down)).unwrap()),
            )
            .build()
            .unwrap();
        let coblet = Coblet::from_inner(inner);

        let sources: Vec<String> = coblet.version_sources().into_iter().map(|s| s.id).collect();
        assert_eq!(sources, ["test:own"]);
        let versions = coblet.versions().await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].source.as_deref(), Some("test:own"));
        assert!(versions[0].latest_release);
        assert!(versions[0].traits.contains(&"acme:declared".to_owned()));
        assert!(
            dir.path()
                .join("meta/sources/test/own/manifest.json")
                .is_file()
        );
        // The version JSON the source downloaded is kept for the launch.
        let sha1 = coblet
            .inner
            .versions(&coblet::catalog::Filter::default())
            .await
            .unwrap()[0]
            .sha1;
        assert!(
            dir.path()
                .join(format!("meta/mojang/versions/{sha1}.json"))
                .is_file()
        );
        let report = coblet.version_traits("b1.7.3".to_owned()).await.unwrap();
        assert!(report.traits.contains(&"jre-legacy".to_owned()));

        let listed = coblet
            .loader_versions("test:fake".to_owned(), "1.20.1".to_owned())
            .await
            .unwrap();
        let merged: Vec<(&str, &str)> = listed
            .iter()
            .map(|version| (version.id.as_str(), version.source.as_deref().unwrap()))
            .collect();
        assert_eq!(merged, [("1.0", "test:fake"), ("2.0", "test:nightly")]);
        let ids: Vec<String> = coblet
            .loader_version_sources("test:fake")
            .unwrap()
            .into_iter()
            .map(|source| source.id)
            .collect();
        assert_eq!(ids, ["test:nightly", "test:down"]);
        let unknown = coblet.loader_version_sources("test:missing").unwrap_err();
        assert!(matches!(
            unknown,
            CobletError::Failed {
                kind: ErrorKind::NotFound,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_entries_fail_their_source() {
        let dir = tempfile::tempdir().unwrap();
        let inner = coblet::Coblet::builder()
            .data_root(dir.path())
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .without_version_source(id("coblet:mojang"))
            .version_source(Arc::new(ForeignGameVersions::new(Arc::new(Bad)).unwrap()))
            .build()
            .unwrap();
        let error = Coblet::from_inner(inner).versions().await.unwrap_err();
        assert!(error.to_string().contains("bad sha1"), "{error}");
    }

    #[test]
    fn the_builder_checks_ids_and_definitions() {
        let builder =
            CobletBuilder::new("/tmp/coblet-sources".to_owned(), "test/1.0".to_owned()).unwrap();
        for result in [
            builder.remove_version_source("no namespace"),
            builder.add_loader_version_source_toml("test:fake", "id = 1"),
            builder.add_loader_version_source_json("no namespace", "{}"),
            builder.remove_loader_version_source("test:fake", "bad id"),
        ] {
            let error = result.unwrap_err();
            assert!(matches!(
                error,
                CobletError::Failed {
                    kind: ErrorKind::InvalidArgument,
                    ..
                }
            ));
        }
        builder
            .add_loader_version_source_toml(
                "test:fake",
                "id = \"test:fixed\"\nname = \"Fixed\"\n[versions]\nformat = \"static\"\nlist = []",
            )
            .unwrap();
        // No loader has the ID.
        let error = builder.build().unwrap_err();
        assert!(matches!(
            error,
            CobletError::Failed {
                kind: ErrorKind::NotFound,
                ..
            }
        ));
    }
}
