use std::sync::{Arc, mpsc};

use coblet::catalog::CatalogContext;
use coblet::mojang::{
    BUILTIN_TRAITS_TOML, TraitContext, TraitError, TraitReport, TraitTarget, VersionTrait,
    VersionTraits,
};
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::ExtensionId;

use crate::java::extension_id;
use crate::{Coblet, CobletError, ErrorKind};

/// Version trait source known to a [`Coblet`](crate::Coblet).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TraitSourceInfo {
    /// ID, such as `coblet:version-traits`.
    pub id: String,
    /// Name shown to users.
    pub name: String,
}

/// One version handed to a [`TraitSource`]: what is known about it and its
/// traits so far.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TraitVersionInfo {
    /// Version ID, such as `b1.7.3`.
    pub id: String,
    /// Release channel, such as `old_beta`, when known.
    pub version_type: Option<String>,
    /// Release time in RFC 3339, when known.
    pub released: Option<String>,
    /// Version source that listed the version, such as `coblet:mojang`.
    pub source: Option<String>,
    /// The version JSON as fetched; present when a version is launched,
    /// absent while a version list is built.
    pub version_json: Option<String>,
    /// Traits so far.
    pub traits: Vec<String>,
}

impl From<&TraitTarget> for TraitVersionInfo {
    fn from(target: &TraitTarget) -> Self {
        let input = target.input();
        Self {
            id: input.id.as_str().to_owned(),
            version_type: input
                .version_type
                .as_ref()
                .map(|kind| kind.as_str().to_owned()),
            released: input.released.map(|time| time.to_string()),
            source: input.source.as_ref().map(|id| id.as_str().to_owned()),
            version_json: input
                .version
                .as_ref()
                .map(|document| String::from_utf8_lossy(&document.bytes).into_owned()),
            traits: trait_names(target.traits()),
        }
    }
}

/// One trait a source added or removed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TraitChangeInfo {
    /// Trait source ID.
    pub source: String,
    /// `added` or `removed`.
    pub action: String,
    /// The trait.
    pub value: String,
}

/// Traits of a version and which trait source added or removed each.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TraitReportInfo {
    /// The resulting traits.
    pub traits: Vec<String>,
    /// Every change, in the order the trait sources made them.
    pub changes: Vec<TraitChangeInfo>,
}

impl From<TraitReport> for TraitReportInfo {
    fn from(report: TraitReport) -> Self {
        Self {
            traits: trait_names(&report.traits),
            changes: report
                .changes
                .iter()
                .map(|change| TraitChangeInfo {
                    source: change.source.as_str().to_owned(),
                    action: change.action.as_str().to_owned(),
                    value: change.value.as_str().to_owned(),
                })
                .collect(),
        }
    }
}

fn trait_names(traits: &VersionTraits) -> Vec<String> {
    traits
        .iter()
        .map(|value| value.as_str().to_owned())
        .collect()
}

/// Requests a trait source written in Swift, Kotlin or Python makes.
///
/// Documents come through coblet's metadata cache, so mirror rules,
/// fallback to the original address and offline caching apply. Each
/// source's documents are cached apart.
#[derive(uniffi::Object)]
pub struct TraitSourceContext {
    catalog: CatalogContext,
    source: ExtensionId,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for TraitSourceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraitSourceContext")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl TraitSourceContext {
    /// Returns the ID of the trait source the requests are made for.
    #[must_use]
    pub fn source_id(&self) -> String {
        self.source.as_str().to_owned()
    }

    /// Fetches a document and caches it under `cache_name`, a relative path
    /// such as `traits.json`.
    ///
    /// The call blocks until the document is there; call it from the
    /// thread `adjust` runs on.
    ///
    /// # Errors
    ///
    /// Fails for a malformed address or cache name and when the document is
    /// unavailable and not cached.
    pub fn fetch(self: Arc<Self>, url: String, cache_name: String) -> Result<Vec<u8>, CobletError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = Arc::clone(&self);
        self.runtime.spawn(async move {
            let cx = TraitContext::new(&context.catalog, &context.source);
            let _ = sender.send(cx.fetch(&url, &cache_name).await);
        });
        let body = receiver
            .recv()
            .map_err(|_| CobletError::new(ErrorKind::Other, "the request was dropped"))?
            .map_err(|error| CobletError::from(coblet::Error::from(error)))?;
        Ok(body.to_vec())
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

/// Version trait source written in Swift, Kotlin or Python, registered with
/// `CobletBuilder.add_trait_source` like the built-in table.
///
/// `adjust` runs once per batch on a worker thread where blocking is fine:
/// once with every listed version when the version list is loaded, and
/// once with the version being launched, then with its version JSON.
#[uniffi::export(foreign)]
pub trait TraitSource: Send + Sync {
    /// Namespaced ID, such as `acme:fixes`.
    fn id(&self) -> String;

    /// Name shown to users.
    fn display_name(&self) -> String;

    /// Returns the new traits of every version, in the order given.
    ///
    /// # Errors
    ///
    /// Return an error when the traits cannot be decided; the engine then
    /// keeps the traits as they were and reports a
    /// `version.trait-source-failed` warning.
    fn adjust(
        &self,
        cx: Arc<TraitSourceContext>,
        versions: Vec<TraitVersionInfo>,
    ) -> Result<Vec<Vec<String>>, CobletError>;
}

/// Adapts a [`TraitSource`] from foreign code to the engine's trait.
pub(crate) struct ForeignTraitSource {
    id: ExtensionId,
    name: String,
    source: Arc<dyn TraitSource>,
}

impl ForeignTraitSource {
    pub(crate) fn new(source: Arc<dyn TraitSource>) -> Result<Self, CobletError> {
        Ok(Self {
            id: extension_id("trait source", &source.id())?,
            name: source.display_name(),
            source,
        })
    }
}

impl Extension for ForeignTraitSource {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

/// Parses trait names a foreign source returned.
fn parse_traits(names: Vec<String>) -> Result<VersionTraits, TraitError> {
    names
        .into_iter()
        .map(|name| {
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                Err(TraitError::failed(format!("invalid trait name `{name}`")))
            } else {
                Ok(VersionTrait::from(name))
            }
        })
        .collect()
}

impl coblet::mojang::TraitSource for ForeignTraitSource {
    fn adjust<'a>(
        &'a self,
        cx: &'a TraitContext<'a>,
        versions: &'a mut [TraitTarget],
    ) -> BoxFuture<'a, Result<(), TraitError>> {
        Box::pin(async move {
            let catalog = cx.catalog();
            let context = Arc::new(TraitSourceContext {
                catalog: CatalogContext::new(catalog.meta().clone(), catalog.policy()),
                source: self.id.clone(),
                runtime: tokio::runtime::Handle::current(),
            });
            let source = Arc::clone(&self.source);
            let given = Arc::clone(&context);
            let batch: Vec<TraitVersionInfo> =
                versions.iter().map(TraitVersionInfo::from).collect();
            let result = tokio::task::spawn_blocking(move || source.adjust(given, batch)).await;
            for warning in context.catalog.take_warnings() {
                catalog.warn(warning);
            }
            let answered = match result {
                Ok(Ok(answered)) => answered,
                Ok(Err(error)) => return Err(TraitError::failed(error.to_string())),
                Err(error) => {
                    return Err(TraitError::failed(format!("the source panicked: {error}")));
                }
            };
            if answered.len() != versions.len() {
                return Err(TraitError::failed(format!(
                    "returned {} trait lists for {} versions",
                    answered.len(),
                    versions.len()
                )));
            }
            let parsed = answered
                .into_iter()
                .map(parse_traits)
                .collect::<Result<Vec<_>, _>>()?;
            for (target, traits) in versions.iter_mut().zip(parsed) {
                *target.traits_mut() = traits;
            }
            Ok(())
        })
    }
}

/// Returns the text of the built-in version trait table, to copy into a
/// table that replaces it under the ID `coblet:version-traits`.
#[uniffi::export]
#[must_use]
pub fn builtin_version_traits_toml() -> String {
    BUILTIN_TRAITS_TOML.to_owned()
}

#[uniffi::export(async_runtime = "tokio")]
impl Coblet {
    /// Lists the version trait sources, in the order they run.
    #[must_use]
    pub fn trait_sources(&self) -> Vec<TraitSourceInfo> {
        self.inner
            .trait_sources()
            .iter()
            .map(|source| TraitSourceInfo {
                id: source.id().as_str().to_owned(),
                name: source.display_name().to_owned(),
            })
            .collect()
    }

    /// Returns the traits of a version with its version JSON, and which
    /// trait source added or removed each. Sources that fail are skipped
    /// and reported as `version.trait-source-failed` warnings.
    ///
    /// # Errors
    ///
    /// Fails when the version is unknown or its version JSON is unavailable.
    pub async fn version_traits(&self, version: String) -> Result<TraitReportInfo, CobletError> {
        let version = self.inner.version(&version).await?;
        Ok(self.inner.version_traits(&version).await?.into())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use coblet::runtime::{BoxFuture, MemoryBody, Request, Response, Transport, TransportError};
    use coblet::types::{Arch, OsFamily, Platform};

    use super::{ForeignTraitSource, TraitSource, TraitSourceContext, TraitVersionInfo};
    use crate::{Coblet, CobletBuilder, CobletError, ErrorKind, builtin_version_traits_toml};

    /// Serves the Mojang fixtures by file name and a remote trait list.
    struct Fixtures;

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
                if url == "https://traits.fake.test/traits.json" {
                    let body = r#"{"b1.7.3": ["acme:remote"]}"#;
                    return Ok(Response::new(200, Vec::new(), MemoryBody::new(body)));
                }
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

    /// A trait source as foreign code would write it: it reads a remote
    /// list and records the batches it was given.
    #[derive(Default)]
    struct Remote {
        batches: Mutex<Vec<Vec<TraitVersionInfo>>>,
    }

    impl TraitSource for Remote {
        fn id(&self) -> String {
            "test:remote".to_owned()
        }

        fn display_name(&self) -> String {
            "Remote traits".to_owned()
        }

        fn adjust(
            &self,
            cx: Arc<TraitSourceContext>,
            versions: Vec<TraitVersionInfo>,
        ) -> Result<Vec<Vec<String>>, CobletError> {
            assert_eq!(cx.source_id(), "test:remote");
            let text = cx.fetch_text(
                "https://traits.fake.test/traits.json".to_owned(),
                "traits.json".to_owned(),
            )?;
            let remote: serde_json::Value = serde_json::from_str(&text).unwrap();
            let answer = versions
                .iter()
                .map(|version| {
                    let mut traits = version.traits.clone();
                    if let Some(extra) = remote[version.id.as_str()].as_array() {
                        traits.extend(extra.iter().map(|value| value.as_str().unwrap().to_owned()));
                    }
                    if version.version_json.is_some() {
                        traits.retain(|name| name != "legacy-macos-rendering");
                    }
                    traits
                })
                .collect();
            self.batches.lock().unwrap().push(versions);
            Ok(answer)
        }
    }

    /// Returns one list too few.
    struct Short;

    impl TraitSource for Short {
        fn id(&self) -> String {
            "test:short".to_owned()
        }

        fn display_name(&self) -> String {
            "Short".to_owned()
        }

        fn adjust(
            &self,
            _cx: Arc<TraitSourceContext>,
            _versions: Vec<TraitVersionInfo>,
        ) -> Result<Vec<Vec<String>>, CobletError> {
            Ok(Vec::new())
        }
    }

    fn coblet(root: &std::path::Path, remote: Arc<Remote>) -> Coblet {
        let inner = coblet::Coblet::builder()
            .data_root(root)
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .trait_source(Arc::new(ForeignTraitSource::new(remote).unwrap()))
            .trait_source(Arc::new(ForeignTraitSource::new(Arc::new(Short)).unwrap()))
            .build()
            .unwrap();
        Coblet::from_inner(inner)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn foreign_sources_tag_listed_and_launched_versions() {
        let dir = tempfile::tempdir().unwrap();
        let remote = Arc::new(Remote::default());
        let coblet = coblet(dir.path(), Arc::clone(&remote));
        let ids: Vec<String> = coblet
            .trait_sources()
            .into_iter()
            .map(|source| source.id)
            .collect();
        assert_eq!(ids, ["coblet:version-traits", "test:remote", "test:short"]);

        let versions = coblet.versions().await.unwrap();
        let beta = versions
            .iter()
            .find(|version| version.id == "b1.7.3")
            .unwrap();
        assert_eq!(beta.traits, ["legacy-online-services", "acme:remote"]);
        assert_eq!(beta.source.as_deref(), Some("coblet:mojang"));
        {
            let batches = remote.batches.lock().unwrap();
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].len(), versions.len());
            assert!(
                batches[0]
                    .iter()
                    .all(|version| version.version_json.is_none())
            );
        }

        let report = coblet.version_traits("b1.7.3".to_owned()).await.unwrap();
        assert_eq!(
            report.traits,
            ["jre-legacy", "legacy-online-services", "acme:remote"]
        );
        let removed = report
            .changes
            .iter()
            .find(|change| change.action == "removed")
            .unwrap();
        assert_eq!(removed.source, "test:remote");
        assert_eq!(removed.value, "legacy-macos-rendering");
        let batches = remote.batches.lock().unwrap();
        let launched = &batches[1][0];
        assert_eq!(launched.version_type.as_deref(), Some("old_beta"));
        assert!(
            launched
                .version_json
                .as_deref()
                .unwrap()
                .contains("\"b1.7.3\"")
        );
        // Both batches read the remote list through the metadata cache.
        assert!(
            walk(dir.path())
                .iter()
                .any(|path| path.ends_with("traits/test/remote/traits.json"))
        );
    }

    fn walk(root: &std::path::Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        found
    }

    fn kind(error: &CobletError) -> ErrorKind {
        let CobletError::Failed { kind, .. } = error;
        *kind
    }

    #[test]
    fn builder_checks_tables_and_ids() {
        let builder =
            CobletBuilder::new("/tmp/x".to_owned(), "coblet-test/1.0".to_owned()).unwrap();
        let error = builder
            .add_trait_table_toml("id = \"acme:bad\"\n[[rule]]\nids = [\"1.0\"]")
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        assert_eq!(
            kind(&builder.remove_trait_source("no-namespace").unwrap_err()),
            ErrorKind::InvalidArgument
        );
        let table = builtin_version_traits_toml();
        builder.replace_trait_table_toml(&table).unwrap();
        builder
            .add_trait_table_json(r#"{"id": "acme:fixes", "rule": [{"add": ["x"]}]}"#)
            .unwrap();
        builder
            .add_trait_table_toml("id = \"acme:fixes\"\n[[rule]]\nadd = [\"y\"]")
            .unwrap();
        let error = builder.build().unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        assert!(error.to_string().contains("acme:fixes"), "{error}");
    }
}
