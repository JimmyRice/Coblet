use coblet_catalog::{CatalogError, MetaRequest};
use coblet_runtime::{BoxFuture, Extension, HttpUrl, RelPath};
use coblet_types::{Arch, ExtensionId, FileHash, OsFamily};

use crate::{
    JavaError, JavaPackage, JavaProvider, JavaQuery, ProviderContext, RUNTIME_INDEX_URL,
    RuntimeIndex, major_version,
};

/// ID of the built-in provider of Mojang's Java runtimes.
pub const MOJANG_PROVIDER_ID: &str = "coblet:mojang";

/// The Java runtimes Mojang publishes for its launcher, as a
/// [`JavaProvider`].
///
/// The version metadata names a runtime component, such as `jre-legacy`,
/// and a Platform Profile may name the platform key, such as `mac-os` for
/// Rosetta; without them the provider picks the component whose version
/// has the requested major and the platform key of the requested
/// architecture.
#[derive(Clone, Debug)]
pub struct MojangRuntimes {
    id: ExtensionId,
    index_url: HttpUrl,
}

impl MojangRuntimes {
    /// Creates the provider with the official index address.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: ExtensionId::new(MOJANG_PROVIDER_ID).unwrap_or_else(|_| unreachable!()),
            index_url: HttpUrl::new(RUNTIME_INDEX_URL).unwrap_or_else(|_| unreachable!()),
        }
    }

    /// Uses another index address, such as a mirror.
    #[must_use]
    pub fn with_index_url(self, index_url: HttpUrl) -> Self {
        Self { index_url, ..self }
    }

    /// Fetches the runtime index.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Metadata`] when the index is unavailable or malformed.
    pub async fn index(&self, cx: &ProviderContext<'_>) -> Result<RuntimeIndex, JavaError> {
        let key = RelPath::new("mojang/java-runtime/all.json")
            .map_err(|error| JavaError::provider(&self.id, error.to_string()))?;
        let bytes = cx
            .catalog()
            .fetch(&MetaRequest::new(self.index_url.clone(), key))
            .await?;
        RuntimeIndex::from_slice(&bytes)
            .map_err(|error| CatalogError::parse(self.index_url.as_str(), error).into())
    }
}

impl Default for MojangRuntimes {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for MojangRuntimes {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &'static str {
        "Mojang"
    }
}

impl JavaProvider for MojangRuntimes {
    fn packages<'a>(
        &'a self,
        cx: &'a ProviderContext<'a>,
        query: &'a JavaQuery,
    ) -> BoxFuture<'a, Result<Vec<JavaPackage>, JavaError>> {
        Box::pin(async move {
            let Some(platform) = query
                .runtime_platform
                .clone()
                .or_else(|| platform_key(&query.os, &query.arch).map(str::to_owned))
            else {
                return Ok(Vec::new());
            };
            let index = self.index(cx).await?;
            let mut packages = Vec::new();
            for (component, build) in index.builds(&platform) {
                let major = major_version(&build.version.name);
                let wanted = match &query.component {
                    Some(named) => named == component,
                    None => major == Some(query.major),
                };
                if !wanted {
                    continue;
                }
                let arch = platform_arch(&platform).unwrap_or_else(|| query.arch.clone());
                packages.push(
                    JavaPackage::manifest(
                        format!("{component}-{platform}"),
                        build.version.name.clone(),
                        major.unwrap_or(query.major),
                        arch,
                        build.manifest.url.clone(),
                        FileHash::Sha1(build.manifest.sha1),
                    )
                    .with_size(build.manifest.size),
                );
            }
            Ok(packages)
        })
    }
}

/// Returns Mojang's runtime platform key for a platform, such as `mac-os-arm64`.
fn platform_key(os: &OsFamily, arch: &Arch) -> Option<&'static str> {
    Some(match (os, arch) {
        (OsFamily::MacOs, Arch::X86_64) => "mac-os",
        (OsFamily::MacOs, Arch::Aarch64) => "mac-os-arm64",
        (OsFamily::Linux, Arch::X86_64) => "linux",
        (OsFamily::Linux, Arch::X86) => "linux-i386",
        (OsFamily::Windows, Arch::X86_64) => "windows-x64",
        (OsFamily::Windows, Arch::X86) => "windows-x86",
        (OsFamily::Windows, Arch::Aarch64) => "windows-arm64",
        _ => return None,
    })
}

/// Returns the architecture of a Mojang runtime platform key.
fn platform_arch(platform: &str) -> Option<Arch> {
    Some(match platform {
        "mac-os" | "linux" | "windows-x64" => Arch::X86_64,
        "mac-os-arm64" | "windows-arm64" => Arch::Aarch64,
        "linux-i386" | "windows-x86" => Arch::X86,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_runtime::{
        BoxFuture, Extension, HttpUrl, MemoryBody, Request, Response, TokioStorage, Transport,
        TransportError,
    };
    use coblet_types::{Arch, OsFamily};

    use super::MojangRuntimes;
    use crate::{JavaDownload, JavaProvider, JavaQuery, ProviderContext};

    const INDEX: &str = r#"{
        "mac-os": {
            "jre-legacy": [{ "manifest": { "sha1": "0000000000000000000000000000000000000001", "size": 10,
                "url": "https://meta.example/legacy.json" }, "version": { "name": "8u74" } }],
            "java-runtime-gamma": [{ "manifest": { "sha1": "0000000000000000000000000000000000000002", "size": 20,
                "url": "https://meta.example/gamma.json" }, "version": { "name": "17.0.15" } }]
        },
        "mac-os-arm64": { "jre-legacy": [] }
    }"#;

    struct Index;

    impl Transport for Index {
        fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(async { Ok(Response::new(200, Vec::new(), MemoryBody::new(INDEX))) })
        }
    }

    async fn packages(query: &JavaQuery) -> Vec<(String, u32, Arch)> {
        let dir = tempfile::tempdir().unwrap();
        let catalog = CatalogContext::new(
            MetaCache::new(Arc::new(Index), Arc::new(TokioStorage::new()), dir.path()),
            CachePolicy::Revalidate,
        );
        let provider = MojangRuntimes::new()
            .with_index_url(HttpUrl::new("https://meta.example/all.json").unwrap());
        let cx = ProviderContext::new(&catalog, provider.id());
        provider
            .packages(&cx, query)
            .await
            .unwrap()
            .into_iter()
            .map(|package| {
                assert!(matches!(package.download, JavaDownload::Manifest { .. }));
                (package.id, package.major, package.arch)
            })
            .collect()
    }

    #[tokio::test]
    async fn picks_the_named_component_or_the_major_version() {
        let mut named = JavaQuery::new(OsFamily::MacOs, Arch::X86_64, 8);
        named.component = Some("jre-legacy".to_owned());
        assert_eq!(
            packages(&named).await,
            [("jre-legacy-mac-os".to_owned(), 8, Arch::X86_64)]
        );

        let by_major = JavaQuery::new(OsFamily::MacOs, Arch::X86_64, 17);
        assert_eq!(
            packages(&by_major).await,
            [("java-runtime-gamma-mac-os".to_owned(), 17, Arch::X86_64)]
        );

        // Rosetta: an arm64 machine asks for the x86_64 platform key.
        let mut rosetta = JavaQuery::new(OsFamily::MacOs, Arch::X86_64, 8);
        rosetta.runtime_platform = Some("mac-os".to_owned());
        assert_eq!(packages(&rosetta).await.len(), 1);

        let arm = JavaQuery::new(OsFamily::MacOs, Arch::Aarch64, 8);
        assert_eq!(packages(&arm).await.len(), 0);
        let freebsd = JavaQuery::new(OsFamily::FreeBsd, Arch::X86_64, 17);
        assert_eq!(packages(&freebsd).await.len(), 0);
    }
}
