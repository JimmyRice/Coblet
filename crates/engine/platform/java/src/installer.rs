use std::path::{Path, PathBuf};
use std::sync::Arc;

use coblet_catalog::{CatalogContext, MetaRequest};
use coblet_download::{Destination, DownloadTask, Downloader};
use coblet_runtime::{
    ArtifactKind, CancellationToken, Expectation, HttpUrl, RelPath, Sources, Storage,
    TransferProgress,
};
use coblet_types::{Arch, ExtensionId, FileHash, OsFamily};
use serde::{Deserialize, Serialize};

use crate::archive::{Entry, unpack};
use crate::discover::{executable_name, runtime_executable};
use crate::{
    ArchiveFormat, JavaDownload, JavaError, JavaPackage, JavaProviders, JavaQuery, ProviderContext,
    RuntimeEntry, RuntimeManifest,
};

/// Name of the file that records which provider installed a runtime.
pub(crate) const MARKER: &str = "coblet-java.json";

/// What [`MARKER`] records about an installed runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Marker {
    pub(crate) provider: ExtensionId,
    pub(crate) package: String,
    pub(crate) version: String,
    pub(crate) major: u32,
    pub(crate) arch: Arch,
    /// The `java` executable, relative to the install directory.
    pub(crate) executable: String,
}

/// Downloads and unpacks the packages of [`JavaProviders`] into
/// `<root>/<namespace>-<name>/<package id>/`.
///
/// The providers only describe packages; this installer checks every hash,
/// keeps paths inside the install directory and writes the result in
/// place only once it is complete.
#[derive(Clone)]
pub struct JavaInstaller {
    downloader: Arc<Downloader>,
    storage: Arc<dyn Storage>,
    root: PathBuf,
    providers: JavaProviders,
}

impl std::fmt::Debug for JavaInstaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JavaInstaller")
            .field("root", &self.root)
            .field("providers", &self.providers)
            .finish_non_exhaustive()
    }
}

impl JavaInstaller {
    /// Creates an installer without providers; `root` is usually `data_root/runtimes`.
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
            providers: JavaProviders::new(),
        }
    }

    /// Uses these providers, asked in order.
    #[must_use]
    pub fn with_providers(self, providers: JavaProviders) -> Self {
        Self { providers, ..self }
    }

    /// Returns the providers.
    #[must_use]
    pub fn providers(&self) -> &JavaProviders {
        &self.providers
    }

    /// Returns the directory runtimes are installed into.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns where a package of a provider is installed.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Provider`] when the package ID is not a plain
    /// file name of letters, digits, `.`, `_`, `+` and `-`.
    pub fn package_dir(
        &self,
        provider: &ExtensionId,
        package: &JavaPackage,
    ) -> Result<PathBuf, JavaError> {
        let id = &package.id;
        let plain = !id.is_empty()
            && !id.starts_with('.')
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
        if !plain {
            return Err(JavaError::provider(
                provider,
                format!("package id `{id}` is not a plain file name"),
            ));
        }
        Ok(self
            .root
            .join(format!("{}-{}", provider.namespace(), provider.name()))
            .join(id))
    }

    /// Asks every provider for packages, in order; a failing provider is
    /// reported in place of its packages.
    pub async fn packages(
        &self,
        catalog: &CatalogContext,
        query: &JavaQuery,
    ) -> Vec<(ExtensionId, Result<Vec<JavaPackage>, JavaError>)> {
        let mut answers = Vec::with_capacity(self.providers.len());
        for provider in self.providers.iter() {
            let cx = ProviderContext::new(catalog, provider.id());
            let packages = provider.packages(&cx, query).await;
            answers.push((provider.id().clone(), packages));
        }
        answers
    }

    /// Installs a package of a provider and returns its `java` executable.
    ///
    /// A manifest package is downloaded file by file and resumes where an
    /// interrupted install stopped; an archive is unpacked next to its
    /// final place and moved there once complete.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Download`] when a file cannot be fetched or does
    /// not match its hash, [`JavaError::InvalidManifest`] for an unusable
    /// file list or archive, and [`JavaError::Storage`] when writing fails.
    pub async fn install(
        &self,
        catalog: &CatalogContext,
        provider: &ExtensionId,
        package: &JavaPackage,
        os: &OsFamily,
        cancel: &CancellationToken,
        progress: Option<&TransferProgress>,
    ) -> Result<PathBuf, JavaError> {
        let dir = self.package_dir(provider, package)?;
        tracing::info!(provider = %provider, package = %package.id, "installing java runtime");
        let executable = match &package.download {
            JavaDownload::Manifest { url, hash, .. } => {
                let key = cache_key(provider, &package.id)?;
                self.install_manifest(catalog, url, hash, key, &dir, os, cancel, progress)
                    .await?
            }
            JavaDownload::Archive {
                url,
                hash,
                size,
                format,
                home,
            } => {
                let archive = Archive {
                    url,
                    hash,
                    size: *size,
                    format: *format,
                    home: home.as_deref(),
                };
                self.install_archive(&archive, &dir, os, cancel, progress)
                    .await?
            }
        };
        let relative = executable
            .strip_prefix(&dir)
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let marker = Marker {
            provider: provider.clone(),
            package: package.id.clone(),
            version: package.version.clone(),
            major: package.major,
            arch: package.arch.clone(),
            executable: relative,
        };
        let json = serde_json::to_vec_pretty(&marker)
            .map_err(|error| JavaError::InvalidManifest(error.to_string()))?;
        self.storage
            .write_atomic(&dir.join(MARKER), json.into())
            .await?;
        Ok(executable)
    }

    /// Installs a Mojang-style file list into `dir`.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal step that only splits up install"
    )]
    pub(crate) async fn install_manifest(
        &self,
        catalog: &CatalogContext,
        url: &HttpUrl,
        hash: &FileHash,
        key: RelPath,
        dir: &Path,
        os: &OsFamily,
        cancel: &CancellationToken,
        progress: Option<&TransferProgress>,
    ) -> Result<PathBuf, JavaError> {
        let request = MetaRequest::new(url.clone(), key).with_hash(hash.clone());
        let bytes = catalog.fetch(&request).await?;
        let manifest: RuntimeManifest = serde_json::from_slice(&bytes)
            .map_err(|error| JavaError::InvalidManifest(error.to_string()))?;
        self.storage.create_dir_all(dir).await?;
        let mut tasks = Vec::new();
        let mut paths = Vec::new();
        let mut executables = Vec::new();
        for (relative, entry) in &manifest.files {
            let path = inside(dir, relative)?;
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
                let link = inside(dir, relative)?;
                if self.storage.metadata(&link).await?.is_none() {
                    if let Some(parent) = link.parent() {
                        self.storage.create_dir_all(parent).await?;
                    }
                    self.storage.symlink(Path::new(target), &link).await?;
                }
            }
        }
        runtime_executable(self.storage.as_ref(), dir, os)
            .await
            .ok_or_else(|| JavaError::InvalidManifest(format!("no java executable in {url}")))
    }

    async fn install_archive(
        &self,
        archive: &Archive<'_>,
        dir: &Path,
        os: &OsFamily,
        cancel: &CancellationToken,
        progress: Option<&TransferProgress>,
    ) -> Result<PathBuf, JavaError> {
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent = dir.parent().unwrap_or(&self.root);
        let file = parent.join(format!(".{name}.{}", archive.format));
        let task = DownloadTask::new(
            Sources::single(archive.url.clone()),
            Expectation::hash(archive.hash.clone(), archive.size),
            Destination::Path(file.clone()),
            ArtifactKind::Runtime,
        );
        let mut results = self
            .downloader
            .download_all_with_progress(vec![task], cancel, progress)
            .await;
        if let Some(Err(source)) = results.pop() {
            return Err(JavaError::Download {
                path: archive.url.to_string(),
                source,
            });
        }
        let bytes = self.storage.read(&file).await?;
        let entries = unpack(bytes, archive.format)
            .map_err(|error| JavaError::InvalidManifest(format!("{}: {error}", archive.url)))?;

        let partial = parent.join(format!(".{name}.partial"));
        if self.storage.metadata(&partial).await?.is_some() {
            self.storage.remove_dir_all(&partial).await?;
        }
        self.write_entries(&partial, entries).await?;
        let found = match archive.home {
            Some(home) => {
                let executable = inside(&partial, home)?
                    .join("bin")
                    .join(executable_name(os));
                self.storage
                    .metadata(&executable)
                    .await?
                    .map(|_| executable)
            }
            None => find_executable(self.storage.as_ref(), &partial, os).await,
        };
        let Some(found) = found else {
            self.storage.remove_dir_all(&partial).await?;
            return Err(JavaError::InvalidManifest(format!(
                "no java executable in {}",
                archive.url
            )));
        };
        let relative = found.strip_prefix(&partial).unwrap_or(&found).to_path_buf();
        if self.storage.metadata(dir).await?.is_some() {
            self.storage.remove_dir_all(dir).await?;
        }
        self.storage.rename(&partial, dir).await?;
        self.storage.remove_file(&file).await?;
        Ok(dir.join(relative))
    }

    async fn write_entries(&self, dir: &Path, entries: Vec<Entry>) -> Result<(), JavaError> {
        let storage = self.storage.as_ref();
        storage.create_dir_all(dir).await?;
        let mut hard_links = Vec::new();
        for entry in entries {
            match entry {
                Entry::Directory(path) => storage.create_dir_all(&dir.join(path)).await?,
                Entry::File {
                    path,
                    content,
                    executable,
                } => {
                    let path = dir.join(path);
                    if let Some(parent) = path.parent() {
                        storage.create_dir_all(parent).await?;
                    }
                    storage.write_atomic(&path, content).await?;
                    if executable {
                        storage.set_executable(&path).await?;
                    }
                }
                Entry::Symlink { path, target } => {
                    let path = dir.join(path);
                    if let Some(parent) = path.parent() {
                        storage.create_dir_all(parent).await?;
                    }
                    storage.symlink(&target, &path).await?;
                }
                Entry::HardLink { path, target } => hard_links.push((path, target)),
            }
        }
        for (path, target) in hard_links {
            storage.copy(&dir.join(target), &dir.join(path)).await?;
        }
        Ok(())
    }
}

/// An archive download, borrowed from [`JavaDownload::Archive`].
struct Archive<'a> {
    url: &'a HttpUrl,
    hash: &'a FileHash,
    size: Option<u64>,
    format: ArchiveFormat,
    home: Option<&'a str>,
}

fn cache_key(provider: &ExtensionId, package: &str) -> Result<RelPath, JavaError> {
    RelPath::new(format!(
        "java/{}/{}/manifest-{package}.json",
        provider.namespace(),
        provider.name()
    ))
    .map_err(|error| JavaError::provider(provider, error.to_string()))
}

/// Looks for `bin/java` in an unpacked archive: at the top, in the macOS
/// bundle layout, and one folder down, where most archives put the runtime.
async fn find_executable(storage: &dyn Storage, dir: &Path, os: &OsFamily) -> Option<PathBuf> {
    let name = executable_name(os);
    let mut homes = vec![dir.to_path_buf(), dir.join("Contents/Home")];
    if let Ok(children) = storage.read_dir(dir).await {
        let mut children: Vec<PathBuf> = children.into_iter().map(|entry| entry.path).collect();
        children.sort();
        for child in children {
            homes.push(child.join("Contents/Home"));
            homes.push(child.join("jre.bundle/Contents/Home"));
            homes.push(child);
        }
    }
    for home in homes {
        let executable = home.join("bin").join(name);
        if matches!(storage.metadata(&executable).await, Ok(Some(_))) {
            return Some(executable);
        }
    }
    None
}

/// Joins a manifest path onto `root`, refusing absolute paths and `..`.
pub(crate) fn inside(root: &Path, relative: &str) -> Result<PathBuf, JavaError> {
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_download::{Downloader, DownloaderConfig};
    use coblet_runtime::{
        BoxFuture, CancellationToken, HttpUrl, MemoryBody, Request, Response, StreamHasher,
        TokioStorage, Transport, TransportError,
    };
    use coblet_types::{Arch, ExtensionId, FileHash, HashAlgorithm, OsFamily};

    use super::{JavaInstaller, MARKER, Marker};
    use crate::{ArchiveFormat, JavaError, JavaPackage};

    const URL: &str = "https://java.example/jre.tar.gz";

    struct Serve(Vec<u8>);

    impl Transport for Serve {
        fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let body = self.0.clone();
            Box::pin(async move { Ok(Response::new(200, Vec::new(), MemoryBody::new(body))) })
        }
    }

    fn archive() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, mode, content) in [
            (
                "jdk-17-jre/Contents/Home/bin/java",
                0o755,
                &b"#!/bin/sh\n"[..],
            ),
            (
                "jdk-17-jre/Contents/Home/release",
                0o644,
                &b"JAVA_VERSION=17"[..],
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(mode);
            builder.append_data(&mut header, path, content).unwrap();
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&builder.into_inner().unwrap()).unwrap();
        encoder.finish().unwrap()
    }

    fn sha256(bytes: &[u8]) -> FileHash {
        let mut hasher = StreamHasher::new(&[HashAlgorithm::Sha256]);
        hasher.update(bytes);
        hasher
            .finish()
            .get(&HashAlgorithm::Sha256)
            .unwrap()
            .to_string()
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn installs_an_archive_and_records_its_provider() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = archive();
        let transport = Arc::new(Serve(bytes.clone()));
        let storage = Arc::new(TokioStorage::new());
        let downloader = Arc::new(Downloader::new(
            transport.clone(),
            storage.clone(),
            DownloaderConfig::default(),
        ));
        let catalog = CatalogContext::new(
            MetaCache::new(transport, storage.clone(), dir.path().join("meta")),
            CachePolicy::Revalidate,
        );
        let installer = JavaInstaller::new(downloader, storage, dir.path().join("runtimes"));
        let provider = ExtensionId::new("acme:java").unwrap();
        let package = JavaPackage::archive(
            "acme-17-jre",
            "17.0.12",
            17,
            Arch::Aarch64,
            HttpUrl::new(URL).unwrap(),
            sha256(&bytes),
            ArchiveFormat::TarGz,
        );
        let cancel = CancellationToken::new();

        let java = installer
            .install(
                &catalog,
                &provider,
                &package,
                &OsFamily::MacOs,
                &cancel,
                None,
            )
            .await
            .unwrap();
        let home = dir.path().join("runtimes/acme-java/acme-17-jre");
        assert_eq!(java, home.join("jdk-17-jre/Contents/Home/bin/java"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&java).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        let marker: Marker =
            serde_json::from_slice(&std::fs::read(home.join(MARKER)).unwrap()).unwrap();
        assert_eq!(marker.executable, "jdk-17-jre/Contents/Home/bin/java");
        assert_eq!(marker.provider, provider);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("runtimes/acme-java"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["acme-17-jre"]);

        // A wrong hash installs nothing.
        let mut tampered = package.clone();
        tampered.id = "acme-17-other".to_owned();
        if let crate::JavaDownload::Archive { hash, .. } = &mut tampered.download {
            *hash = sha256(b"something else");
        }
        let error = installer
            .install(
                &catalog,
                &provider,
                &tampered,
                &OsFamily::MacOs,
                &cancel,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, JavaError::Download { .. }), "{error}");
        assert!(!dir.path().join("runtimes/acme-java/acme-17-other").exists());

        let mut escaping = package;
        escaping.id = "../escape".to_owned();
        assert!(matches!(
            installer.package_dir(&provider, &escaping),
            Err(JavaError::Provider { .. })
        ));
    }
}
