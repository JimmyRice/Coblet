use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use coblet_catalog::{CatalogContext, MetaRequest};
use coblet_platform::JavaRequirement;
use coblet_runtime::{ArtifactKind, BoxFuture, Extension, HttpUrl, RegistryError, RelPath};
use coblet_types::{Arch, ExtensionId, FileHash, OsFamily};
use serde::{Deserialize, Serialize};

use crate::JavaError;

/// Source of downloadable Java runtimes, such as Mojang's runtime index or
/// a vendor like Adoptium.
///
/// A provider only describes packages; the engine downloads, checks and
/// unpacks them, so a third-party provider does no IO of its own beyond
/// reading metadata through its [`ProviderContext`]. Built-in and
/// third-party providers implement this same trait and live in the same
/// [`JavaProviders`] list.
///
/// # Examples
///
/// ```
/// use coblet_java::{ArchiveFormat, JavaError, JavaPackage, JavaProvider, JavaQuery, ProviderContext};
/// use coblet_runtime::{BoxFuture, Extension, HttpUrl};
/// use coblet_types::{ExtensionId, FileHash};
///
/// struct Acme(ExtensionId);
///
/// impl Extension for Acme {
///     fn id(&self) -> &ExtensionId { &self.0 }
///     fn display_name(&self) -> &str { "Acme Java" }
/// }
///
/// impl JavaProvider for Acme {
///     fn packages<'a>(&'a self, _: &'a ProviderContext<'a>, query: &'a JavaQuery)
///         -> BoxFuture<'a, Result<Vec<JavaPackage>, JavaError>> {
///         Box::pin(async move {
///             let failed = |error: String| JavaError::provider(&self.0, error);
///             let url = HttpUrl::new(format!("https://java.acme.test/{}.zip", query.major))
///                 .map_err(|error| failed(error.to_string()))?;
///             let hash: FileHash = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
///                 .parse()
///                 .map_err(|error: coblet_types::FileHashError| failed(error.to_string()))?;
///             let package = JavaPackage::archive(format!("acme-{}", query.major), "1.0", query.major,
///                 query.arch.clone(), url, hash, ArchiveFormat::Zip);
///             Ok(vec![package])
///         })
///     }
/// }
/// # let _ = Acme(ExtensionId::new("acme:java").unwrap());
/// ```
pub trait JavaProvider: Extension {
    /// Lists the packages that meet `query`, best first; an empty list
    /// means the provider has nothing for this platform and version.
    fn packages<'a>(
        &'a self,
        cx: &'a ProviderContext<'a>,
        query: &'a JavaQuery,
    ) -> BoxFuture<'a, Result<Vec<JavaPackage>, JavaError>>;
}

/// The Java runtime a launch or a user asks a [`JavaProvider`] for.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaQuery {
    /// Operating system the runtime runs on.
    pub os: OsFamily,
    /// Architecture the runtime must be built for.
    pub arch: Arch,
    /// Java major version, such as 8 or 21.
    pub major: u32,
    /// Mojang runtime component named by the version metadata, such as
    /// `jre-legacy`; other providers may ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// Mojang runtime platform key chosen by a Platform Profile, such as
    /// `mac-os`; other providers may ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_platform: Option<String>,
}

impl JavaQuery {
    /// Asks for a Java major version on a platform.
    #[must_use]
    pub fn new(os: OsFamily, arch: Arch, major: u32) -> Self {
        Self {
            os,
            arch,
            major,
            component: None,
            runtime_platform: None,
        }
    }

    /// Asks for what a launch requires.
    #[must_use]
    pub fn for_requirement(os: OsFamily, requirement: &JavaRequirement) -> Self {
        Self {
            os,
            arch: requirement.arch.clone(),
            major: requirement.major,
            component: requirement.component.clone(),
            runtime_platform: requirement.runtime_platform.clone(),
        }
    }
}

/// One downloadable Java runtime, as a [`JavaProvider`] describes it.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaPackage {
    /// Identifier, unique within the provider and stable across runs, such
    /// as `jre-legacy-mac-os`; it names the install directory.
    pub id: String,
    /// Full version, such as `17.0.12+7`.
    pub version: String,
    /// Java major version.
    pub major: u32,
    /// Architecture the runtime is built for.
    pub arch: Arch,
    /// How to get it.
    pub download: JavaDownload,
}

impl JavaPackage {
    /// Describes a runtime packed in one archive.
    #[must_use]
    pub fn archive(
        id: impl Into<String>,
        version: impl Into<String>,
        major: u32,
        arch: Arch,
        url: HttpUrl,
        hash: FileHash,
        format: ArchiveFormat,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            major,
            arch,
            download: JavaDownload::Archive {
                url,
                hash,
                size: None,
                format,
                home: None,
            },
        }
    }

    /// Describes a runtime listed file by file in a Mojang-style manifest.
    #[must_use]
    pub fn manifest(
        id: impl Into<String>,
        version: impl Into<String>,
        major: u32,
        arch: Arch,
        url: HttpUrl,
        hash: FileHash,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            major,
            arch,
            download: JavaDownload::Manifest {
                url,
                hash,
                size: None,
            },
        }
    }

    /// Sets the download size in bytes.
    #[must_use]
    pub fn with_size(mut self, bytes: u64) -> Self {
        match &mut self.download {
            JavaDownload::Archive { size, .. } | JavaDownload::Manifest { size, .. } => {
                *size = Some(bytes);
            }
        }
        self
    }

    /// Names the Java home inside an archive, such as
    /// `jdk-17.0.12+7-jre/Contents/Home`; found by searching when not set.
    #[must_use]
    pub fn with_home(mut self, path: impl Into<String>) -> Self {
        if let JavaDownload::Archive { home, .. } = &mut self.download {
            *home = Some(path.into());
        }
        self
    }
}

/// How a [`JavaPackage`] is downloaded. Every form carries a hash: the
/// engine installs nothing it cannot check.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum JavaDownload {
    /// One zip or tar.gz archive.
    Archive {
        /// Address of the archive.
        url: HttpUrl,
        /// Hash of the archive.
        hash: FileHash,
        /// Size in bytes, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        /// Archive format.
        format: ArchiveFormat,
        /// Java home inside the archive; found by searching when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
    },
    /// A file list in the format of Mojang's runtime manifests, each file
    /// downloaded on its own.
    Manifest {
        /// Address of the file list.
        url: HttpUrl,
        /// Hash of the file list.
        hash: FileHash,
        /// Size in bytes, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
}

/// Format of a Java runtime archive.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArchiveFormat {
    /// A zip file.
    #[serde(rename = "zip")]
    Zip,
    /// A gzip-compressed tar file.
    #[serde(rename = "tar.gz")]
    TarGz,
}

impl fmt::Display for ArchiveFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
        })
    }
}

/// What a [`JavaProvider`] may use while listing packages: cached metadata
/// requests on behalf of one provider.
///
/// Requests go through the metadata cache, so mirror rules, fallback to
/// the original address and offline caching apply to third-party providers
/// as to built-in ones. Cache keys are kept apart per provider under
/// `java/<namespace>/<name>/`.
#[derive(Debug)]
pub struct ProviderContext<'a> {
    catalog: &'a CatalogContext,
    provider: &'a ExtensionId,
}

impl<'a> ProviderContext<'a> {
    /// Creates a context for one provider.
    #[must_use]
    pub fn new(catalog: &'a CatalogContext, provider: &'a ExtensionId) -> Self {
        Self { catalog, provider }
    }

    /// Returns the provider the requests are made for.
    #[must_use]
    pub fn provider(&self) -> &ExtensionId {
        self.provider
    }

    /// Returns the catalog context, for requests that need more control,
    /// such as an expected hash.
    #[must_use]
    pub fn catalog(&self) -> &CatalogContext {
        self.catalog
    }

    /// Returns the cache key of a document of this provider.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Provider`] when `name` is not a safe relative path.
    pub fn cache_key(&self, name: &str) -> Result<RelPath, JavaError> {
        RelPath::new(format!(
            "java/{}/{}/{name}",
            self.provider.namespace(),
            self.provider.name()
        ))
        .map_err(|error| JavaError::provider(self.provider, format!("cache key {name}: {error}")))
    }

    /// Fetches a document, cached under [`cache_key`](Self::cache_key)`(name)`.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Provider`] for a malformed address or key and
    /// [`JavaError::Metadata`] when the document is unavailable.
    pub async fn fetch(&self, url: &str, name: &str) -> Result<Bytes, JavaError> {
        let address = HttpUrl::new(url)
            .map_err(|error| JavaError::provider(self.provider, format!("{url}: {error}")))?;
        let request =
            MetaRequest::new(address, self.cache_key(name)?).with_kind(ArtifactKind::Metadata);
        Ok(self.catalog.fetch(&request).await?)
    }

    /// Fetches and parses a JSON document, like [`fetch`](Self::fetch).
    ///
    /// # Errors
    ///
    /// Returns the errors of [`fetch`](Self::fetch), and
    /// [`JavaError::Provider`] when the document does not parse.
    pub async fn fetch_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        name: &str,
    ) -> Result<T, JavaError> {
        let body = self.fetch(url, name).await?;
        serde_json::from_slice(&body)
            .map_err(|error| JavaError::provider(self.provider, format!("{url}: {error}")))
    }
}

/// Definitions of the built-in data providers, in the order they are asked.
const BUILTIN_DATA: [&str; 2] = [
    include_str!("../data/providers/adoptium.toml"),
    include_str!("../data/providers/zulu.toml"),
];

/// The Java providers of a launcher, asked in order when no installed Java
/// fits.
///
/// Order matters: the first provider with a matching package installs it.
/// [`register`](Self::register) appends and refuses an ID that is taken;
/// [`replace`](Self::replace) swaps a provider in place, for example a
/// built-in one with a patched copy, and [`remove`](Self::remove) drops one,
/// for example to never download from Mojang.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_java::{JavaError, JavaPackage, JavaProvider, JavaProviders, JavaQuery, ProviderContext};
/// use coblet_runtime::{BoxFuture, Extension};
/// use coblet_types::ExtensionId;
///
/// struct Empty(ExtensionId);
///
/// impl Extension for Empty {
///     fn id(&self) -> &ExtensionId { &self.0 }
///     fn display_name(&self) -> &str { "Empty" }
/// }
///
/// impl JavaProvider for Empty {
///     fn packages<'a>(&'a self, _: &'a ProviderContext<'a>, _: &'a JavaQuery)
///         -> BoxFuture<'a, Result<Vec<JavaPackage>, JavaError>> {
///         Box::pin(async { Ok(Vec::new()) })
///     }
/// }
///
/// let mut providers = JavaProviders::new();
/// providers.register(Arc::new(Empty(ExtensionId::new("acme:first")?)))?;
/// providers.register(Arc::new(Empty(ExtensionId::new("acme:second")?)))?;
/// assert!(providers.register(Arc::new(Empty(ExtensionId::new("acme:first")?))).is_err());
/// providers.remove(&ExtensionId::new("acme:first")?);
/// let ids: Vec<_> = providers.iter().map(|provider| provider.id().as_str()).collect();
/// assert_eq!(ids, ["acme:second"]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Default)]
pub struct JavaProviders {
    entries: Vec<Arc<dyn JavaProvider>>,
}

impl fmt::Debug for JavaProviders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|provider| provider.id()))
            .finish()
    }
}

impl JavaProviders {
    /// Creates an empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the list of built-in providers, in this order: Mojang's
    /// runtimes, then Eclipse Temurin from Adoptium and Azul Zulu for the
    /// platforms and versions Mojang does not ship.
    #[must_use]
    pub fn builtin() -> Self {
        let mut entries: Vec<Arc<dyn JavaProvider>> = vec![Arc::new(crate::MojangRuntimes::new())];
        for definition in BUILTIN_DATA {
            match crate::DataJavaProvider::from_toml(definition) {
                Ok(provider) => entries.push(Arc::new(provider)),
                Err(error) => tracing::error!(%error, "a built-in java provider is invalid"),
            }
        }
        Self { entries }
    }

    /// Appends a provider.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when another provider has the same ID.
    pub fn register(&mut self, provider: Arc<dyn JavaProvider>) -> Result<(), RegistryError> {
        if self.get(provider.id()).is_some() {
            return Err(RegistryError::Duplicate(provider.id().clone()));
        }
        self.entries.push(provider);
        Ok(())
    }

    /// Puts a provider in place of the one with the same ID, keeping its
    /// position, or appends it; returns the provider it replaced.
    pub fn replace(&mut self, provider: Arc<dyn JavaProvider>) -> Option<Arc<dyn JavaProvider>> {
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|known| known.id() == provider.id())
        {
            return Some(std::mem::replace(slot, provider));
        }
        self.entries.push(provider);
        None
    }

    /// Removes and returns a provider.
    pub fn remove(&mut self, id: &ExtensionId) -> Option<Arc<dyn JavaProvider>> {
        let index = self.entries.iter().position(|known| known.id() == id)?;
        Some(self.entries.remove(index))
    }

    /// Returns a provider by ID.
    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&Arc<dyn JavaProvider>> {
        self.entries.iter().find(|known| known.id() == id)
    }

    /// Iterates over the providers in the order they are asked.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Arc<dyn JavaProvider>> {
        self.entries.iter()
    }

    /// Returns the number of providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use coblet_runtime::HttpUrl;
    use coblet_types::Arch;

    use super::{ArchiveFormat, JavaDownload, JavaPackage};

    #[test]
    fn builtin_providers_parse_in_order() {
        let ids: Vec<_> = super::JavaProviders::builtin()
            .iter()
            .map(|provider| provider.id().as_str().to_owned())
            .collect();
        assert_eq!(ids, ["coblet:mojang", "coblet:adoptium", "coblet:zulu"]);
    }

    #[test]
    fn packages_round_trip_through_json() {
        let package = JavaPackage::archive(
            "temurin-17-jre",
            "17.0.12+7",
            17,
            Arch::Aarch64,
            HttpUrl::new("https://java.example/jre.tar.gz").unwrap(),
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
                .parse()
                .unwrap(),
            ArchiveFormat::TarGz,
        )
        .with_size(42)
        .with_home("jdk-17.0.12+7-jre/Contents/Home");
        let json = serde_json::to_value(&package).unwrap();
        assert_eq!(json["download"]["type"], "archive");
        assert_eq!(json["download"]["format"], "tar.gz");
        assert_eq!(json["arch"], "aarch64");
        let back: JavaPackage = serde_json::from_value(json).unwrap();
        assert_eq!(back, package);
        assert!(matches!(
            back.download,
            JavaDownload::Archive { size: Some(42), .. }
        ));
    }
}
