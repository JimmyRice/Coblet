use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use coblet_catalog::{CatalogContext, CatalogError, MetaRequest};
use coblet_download::{Destination, DownloadTask, Downloader};
use coblet_platform::{JavaRequirement, JavaSource};
use coblet_runtime::{
    ArtifactKind, CancellationToken, Expectation, HttpUrl, ProcessSpawner, RelPath, Sources,
    Storage, TransferProgress,
};
use coblet_types::{FileHash, OsFamily, Sha1Digest};
use serde::Deserialize;

use crate::discover::runtime_executable;
use crate::{InstallSource, JavaError, JavaInstall, SearchPaths, discover, probe, select};

/// Address of the Mojang Java runtime index.
pub const RUNTIME_INDEX_URL: &str = "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

/// Parsed Mojang runtime index: platform key to component to builds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct RuntimeIndex(BTreeMap<String, BTreeMap<String, Vec<RuntimeBuild>>>);

/// One runtime build in the index.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RuntimeBuild {
    /// The file list of the build.
    pub manifest: ManifestRef,
    /// Version information.
    pub version: RuntimeVersion,
}

/// Reference to a runtime file list.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ManifestRef {
    /// SHA-1 of the list.
    pub sha1: Sha1Digest,
    /// Size of the list.
    pub size: u64,
    /// Address of the list.
    pub url: HttpUrl,
}

/// Version of a runtime build.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RuntimeVersion {
    /// Version name, such as `17.0.15` or `8u74-cacert462b08`.
    pub name: String,
}

impl RuntimeIndex {
    /// Parses the index.
    ///
    /// # Errors
    ///
    /// Returns the [`serde_json::Error`] of malformed documents.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Returns the current build of a component on a platform; empty lists count as missing.
    #[must_use]
    pub fn build(&self, platform: &str, component: &str) -> Option<&RuntimeBuild> {
        self.0.get(platform)?.get(component)?.first()
    }

    /// Iterates over the platform keys.
    pub fn platforms(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

/// File list of one runtime build.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RuntimeManifest {
    /// Entries by path inside the runtime directory.
    pub files: BTreeMap<String, RuntimeEntry>,
}

/// Entry of a runtime file list.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RuntimeEntry {
    /// A directory.
    Directory,
    /// A file; coblet downloads the uncompressed copy.
    File {
        /// Download copies.
        downloads: RuntimeDownloads,
        /// Whether the file is executable.
        #[serde(default)]
        executable: bool,
    },
    /// A symbolic link.
    Link {
        /// Link target, relative to the link's directory.
        target: String,
    },
}

/// Download copies of a runtime file.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RuntimeDownloads {
    /// The uncompressed file.
    pub raw: RuntimeFile,
}

/// One downloadable runtime file.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RuntimeFile {
    /// SHA-1 of the file.
    pub sha1: Sha1Digest,
    /// Size in bytes.
    pub size: u64,
    /// Address.
    pub url: HttpUrl,
}

fn key(path: &str) -> Result<RelPath, JavaError> {
    RelPath::new(path).map_err(|_| JavaError::Metadata(CatalogError::InvalidKey(path.to_owned())))
}

/// Joins a manifest path onto `root`, refusing absolute paths and `..`.
fn inside(root: &Path, relative: &str) -> Result<PathBuf, JavaError> {
    let safe = !relative.is_empty()
        && !relative.starts_with('/')
        && relative
            .split(['/', '\\'])
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(':'));
    if !safe {
        return Err(JavaError::InvalidManifest(format!(
            "path `{relative}` leaves the runtime directory"
        )));
    }
    Ok(root.join(relative))
}

/// Downloads Mojang Java runtimes into `<root>/<component>/<platform>/`.
#[derive(Clone)]
pub struct RuntimeInstaller {
    downloader: Arc<Downloader>,
    storage: Arc<dyn Storage>,
    root: PathBuf,
    index_url: HttpUrl,
}

impl std::fmt::Debug for RuntimeInstaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeInstaller")
            .field("root", &self.root)
            .field("index_url", &self.index_url)
            .finish_non_exhaustive()
    }
}

impl RuntimeInstaller {
    /// Creates an installer with the official index address; `root` is usually `data_root/runtimes`.
    #[must_use]
    pub fn new(
        downloader: Arc<Downloader>,
        storage: Arc<dyn Storage>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            downloader,
            storage,
            root: root.into(),
            index_url: HttpUrl::new(RUNTIME_INDEX_URL).unwrap_or_else(|_| unreachable!()),
        }
    }

    /// Uses another index address, such as a mirror.
    #[must_use]
    pub fn with_index_url(self, index_url: HttpUrl) -> Self {
        Self { index_url, ..self }
    }

    /// Returns the directory runtimes are installed into.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Fetches the runtime index.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Metadata`] when the index is unavailable or malformed.
    pub async fn index(&self, cx: &CatalogContext) -> Result<RuntimeIndex, JavaError> {
        let request =
            MetaRequest::new(self.index_url.clone(), key("mojang/java-runtime/all.json")?);
        let bytes = cx.fetch(&request).await?;
        RuntimeIndex::from_slice(&bytes)
            .map_err(|error| CatalogError::parse(self.index_url.as_str(), error).into())
    }

    /// Installs a runtime and returns its `java` executable.
    ///
    /// Files already present with the right hash are kept, so an interrupted
    /// install resumes where it stopped. Each file and received chunk is
    /// counted in `progress`.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::NotAvailable`] when Mojang has no such runtime,
    /// and other [`JavaError`]s when fetching or writing fails.
    pub async fn install(
        &self,
        cx: &CatalogContext,
        platform: &str,
        component: &str,
        os: &OsFamily,
        cancel: &CancellationToken,
        progress: Option<&TransferProgress>,
    ) -> Result<PathBuf, JavaError> {
        let not_available = || JavaError::NotAvailable {
            platform: platform.to_owned(),
            component: component.to_owned(),
        };
        let index = self.index(cx).await?;
        let build = index.build(platform, component).ok_or_else(not_available)?;
        let request = MetaRequest::new(
            build.manifest.url.clone(),
            key(&format!("mojang/java-runtime/{}.json", build.manifest.sha1))?,
        )
        .with_hash(FileHash::Sha1(build.manifest.sha1));
        let bytes = cx.fetch(&request).await?;
        let manifest: RuntimeManifest = serde_json::from_slice(&bytes)
            .map_err(|error| JavaError::InvalidManifest(error.to_string()))?;

        let dir = inside(&self.root, &format!("{component}/{platform}"))?;
        self.storage.create_dir_all(&dir).await?;
        let mut tasks = Vec::new();
        let mut paths = Vec::new();
        let mut executables = Vec::new();
        for (relative, entry) in &manifest.files {
            let path = inside(&dir, relative)?;
            match entry {
                RuntimeEntry::Directory => self.storage.create_dir_all(&path).await?,
                RuntimeEntry::File {
                    downloads,
                    executable,
                } => {
                    let raw = &downloads.raw;
                    tasks.push(DownloadTask::new(
                        Sources::single(raw.url.clone()),
                        Expectation::hash(FileHash::Sha1(raw.sha1), Some(raw.size)),
                        Destination::Path(path.clone()),
                        ArtifactKind::Runtime,
                    ));
                    paths.push(relative.clone());
                    if *executable {
                        executables.push(path);
                    }
                }
                RuntimeEntry::Link { .. } => {}
            }
        }
        tracing::info!(
            platform,
            component,
            files = tasks.len(),
            "installing java runtime"
        );
        let results = self
            .downloader
            .download_all_with_progress(tasks, cancel, progress)
            .await;
        for (relative, result) in paths.into_iter().zip(results) {
            result.map_err(|source| JavaError::Download {
                path: relative,
                source,
            })?;
        }
        for path in executables {
            self.storage.set_executable(&path).await?;
        }
        for (relative, entry) in &manifest.files {
            if let RuntimeEntry::Link { target } = entry {
                let link = inside(&dir, relative)?;
                if self.storage.metadata(&link).await?.is_none() {
                    if let Some(parent) = link.parent() {
                        self.storage.create_dir_all(parent).await?;
                    }
                    self.storage.symlink(Path::new(target), &link).await?;
                }
            }
        }
        runtime_executable(self.storage.as_ref(), &dir, os)
            .await
            .ok_or_else(|| {
                JavaError::InvalidManifest(format!(
                    "no java executable in {component} for {platform}"
                ))
            })
    }
}

/// Everything [`find_or_install`] needs.
#[derive(Clone, Copy)]
pub struct JavaContext<'a> {
    /// File access.
    pub storage: &'a dyn Storage,
    /// Process spawning for probes.
    pub spawner: &'a dyn ProcessSpawner,
    /// Operating system of the machine.
    pub os: &'a OsFamily,
    /// Places to search.
    pub search: &'a SearchPaths,
    /// Runtime installer; `None` disables downloads.
    pub installer: Option<&'a RuntimeInstaller>,
    /// Metadata context for the runtime index.
    pub catalog: &'a CatalogContext,
    /// Counters for a runtime download, if one happens.
    pub progress: Option<&'a TransferProgress>,
}

impl std::fmt::Debug for JavaContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JavaContext")
            .field("os", self.os)
            .field("search", self.search)
            .finish_non_exhaustive()
    }
}

/// Finds an installed Java for a requirement, or installs the Mojang runtime it names.
///
/// # Errors
///
/// Returns [`JavaError::NotFound`] when nothing matches and no runtime can
/// be installed, or the error of a failed install.
pub async fn find_or_install(
    cx: JavaContext<'_>,
    requirement: &JavaRequirement,
    cancel: &CancellationToken,
) -> Result<JavaInstall, JavaError> {
    let installs = discover(cx.storage, cx.spawner, cx.os, cx.search).await;
    if let Some(install) = select(&installs, requirement) {
        return Ok(install.clone());
    }
    let not_found = |reason: &str| JavaError::NotFound {
        major: requirement.major,
        arch: requirement.arch.as_str().to_owned(),
        reason: reason.to_owned(),
    };
    if requirement.source == JavaSource::SystemOnly {
        return Err(not_found(
            "the platform profile allows only an installed java",
        ));
    }
    let (Some(platform), Some(component)) = (&requirement.runtime_platform, &requirement.component)
    else {
        return Err(not_found("mojang has no runtime for this platform"));
    };
    let Some(installer) = cx.installer else {
        return Err(not_found("runtime downloads are disabled"));
    };
    let executable = installer
        .install(cx.catalog, platform, component, cx.os, cancel, cx.progress)
        .await?;
    let install = probe(
        cx.spawner,
        &executable,
        InstallSource::MojangRuntime {
            component: component.clone(),
        },
    )
    .await?;
    if install.major != requirement.major || install.arch != requirement.arch {
        return Err(not_found(&format!("the installed runtime is {install}")));
    }
    Ok(install)
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
