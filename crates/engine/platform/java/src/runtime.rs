use std::collections::BTreeMap;

use coblet_catalog::CatalogContext;
use coblet_platform::{JavaRequirement, JavaSource};
use coblet_runtime::{CancellationToken, HttpUrl, ProcessSpawner, Storage, TransferProgress};
use coblet_types::{ExtensionId, OsFamily, Sha1Digest};
use serde::Deserialize;

use crate::{
    InstallSource, JavaError, JavaInstall, JavaInstaller, JavaPackage, JavaQuery, SearchPaths,
    discover, probe, select,
};

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

    /// Iterates over the current build of each component on a platform,
    /// skipping components without builds.
    pub fn builds<'a>(
        &'a self,
        platform: &str,
    ) -> impl Iterator<Item = (&'a str, &'a RuntimeBuild)> + 'a {
        self.0
            .get(platform)
            .into_iter()
            .flatten()
            .filter_map(|(component, builds)| Some((component.as_str(), builds.first()?)))
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
    /// Installer with the providers to ask; `None` disables downloads.
    pub installer: Option<&'a JavaInstaller>,
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

/// Finds an installed Java for a requirement, or installs one from the
/// first provider with a matching package.
///
/// Providers are asked in order; a provider that fails or offers nothing
/// usable is skipped, and the reasons of all of them end up in the error
/// when none succeeds.
///
/// # Errors
///
/// Returns [`JavaError::NotFound`] when nothing matches and no package can
/// be installed.
pub async fn find_or_install(
    cx: JavaContext<'_>,
    requirement: &JavaRequirement,
    cancel: &CancellationToken,
) -> Result<JavaInstall, JavaError> {
    let installs = discover(cx.storage, cx.spawner, cx.os, cx.search).await;
    if let Some(install) = select(&installs, requirement) {
        return Ok(install.clone());
    }
    let not_found = |reason: String| JavaError::NotFound {
        major: requirement.major,
        arch: requirement.arch.as_str().to_owned(),
        reason,
    };
    if requirement.source == JavaSource::SystemOnly {
        return Err(not_found(
            "the platform profile allows only an installed java".to_owned(),
        ));
    }
    let Some(installer) = cx.installer else {
        return Err(not_found("runtime downloads are disabled".to_owned()));
    };
    if installer.providers().is_empty() {
        return Err(not_found("no java provider is registered".to_owned()));
    }
    let query = JavaQuery::for_requirement(cx.os.clone(), requirement);
    let mut reasons = Vec::new();
    for (provider, answer) in installer.packages(cx.catalog, &query).await {
        let packages = match answer {
            Ok(packages) => packages,
            Err(error) => {
                reasons.push(format!("{provider}: {error}"));
                continue;
            }
        };
        let matching: Vec<_> = packages
            .into_iter()
            .filter(|package| {
                package.major == requirement.major && package.arch == requirement.arch
            })
            .collect();
        if matching.is_empty() {
            reasons.push(format!("{provider}: no matching package"));
        }
        for package in matching {
            match install_one(cx, installer, &provider, &package, requirement, cancel).await {
                Ok(install) => return Ok(install),
                Err(error) => {
                    tracing::warn!(%provider, package = %package.id, %error, "java install failed");
                    reasons.push(format!("{provider} {}: {error}", package.id));
                }
            }
        }
    }
    Err(not_found(reasons.join("; ")))
}

async fn install_one(
    cx: JavaContext<'_>,
    installer: &JavaInstaller,
    provider: &ExtensionId,
    package: &JavaPackage,
    requirement: &JavaRequirement,
    cancel: &CancellationToken,
) -> Result<JavaInstall, JavaError> {
    let executable = installer
        .install(cx.catalog, provider, package, cx.os, cancel, cx.progress)
        .await?;
    let install = probe(
        cx.spawner,
        &executable,
        InstallSource::Provider {
            provider: provider.clone(),
            package: package.id.clone(),
        },
    )
    .await?;
    if install.major != requirement.major || install.arch != requirement.arch {
        return Err(JavaError::provider(
            provider,
            format!("package {} turned out to be {install}", package.id),
        ));
    }
    Ok(install)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_download::{Downloader, DownloaderConfig};
    use coblet_platform::{JavaRequirement, JavaSource};
    use coblet_runtime::{
        BoxFuture, CancellationToken, HttpUrl, MemoryBody, Request, Response, StreamHasher,
        TokioSpawner, TokioStorage, Transport, TransportError,
    };
    use coblet_types::{Arch, ExtensionId, HashAlgorithm, OsFamily};

    use super::{RuntimeIndex, find_or_install};
    use crate::{
        InstallSource, JavaContext, JavaError, JavaInstaller, JavaProviders, MojangRuntimes,
        SearchPaths, candidates, discover,
    };

    /// Fake `java` that prints a property listing like a real one.
    fn fake_java(version: &str, arch: &str) -> String {
        format!(
            "#!/bin/sh\n\
         echo 'Property settings:' >&2\n\
         echo '    java.home = /fake/{version}' >&2\n\
         echo '    java.vendor = Fake' >&2\n\
         echo '    java.version = {version}' >&2\n\
         echo '    os.arch = {arch}' >&2\n"
        )
    }

    fn sha1(bytes: &[u8]) -> String {
        let mut hasher = StreamHasher::new(&[HashAlgorithm::Sha1]);
        hasher.update(bytes);
        hasher
            .finish()
            .get(&HashAlgorithm::Sha1)
            .unwrap()
            .to_string()[5..]
            .to_owned()
    }

    struct Files {
        routes: Vec<(String, Vec<u8>)>,
        requested: Mutex<Vec<String>>,
    }

    impl Transport for Files {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            self.requested.lock().unwrap().push(url.clone());
            let body = self
                .routes
                .iter()
                .find(|(route, _)| *route == url)
                .map(|(_, body)| body.clone());
            Box::pin(async move {
                Ok(match body {
                    Some(body) => Response::new(200, Vec::new(), MemoryBody::new(body)),
                    None => Response::new(404, Vec::new(), MemoryBody::new("")),
                })
            })
        }
    }

    const JAVA_URL: &str = "https://piston-data.mojang.com/v1/objects/java";
    const INDEX_URL: &str = "https://meta.example/all.json";
    const MANIFEST_URL: &str = "https://meta.example/manifest.json";

    fn routes() -> Vec<(String, Vec<u8>)> {
        let java = fake_java("1.8.0_74", "x86_64").into_bytes();
        let manifest = format!(
            r#"{{"files": {{
            "jre.bundle/Contents/Home/bin": {{"type": "directory"}},
            "jre.bundle/Contents/Home/bin/java": {{"type": "file", "executable": true,
                "downloads": {{"raw": {{"sha1": "{}", "size": {}, "url": "{JAVA_URL}"}}}}}},
            "jre.bundle/Contents/Home/bin/java-link": {{"type": "link", "target": "java"}}
        }}}}"#,
            sha1(&java),
            java.len()
        );
        let index = format!(
            r#"{{"mac-os": {{"jre-legacy": [{{"manifest": {{"sha1": "{}", "size": {}, "url": "{MANIFEST_URL}"}},
             "version": {{"name": "8u74"}}}}]}},
            "mac-os-arm64": {{"jre-legacy": []}}}}"#,
            sha1(manifest.as_bytes()),
            manifest.len()
        );
        vec![
            (INDEX_URL.to_owned(), index.into_bytes()),
            (MANIFEST_URL.to_owned(), manifest.into_bytes()),
            (JAVA_URL.to_owned(), java),
        ]
    }

    fn requirement(major: u32, arch: Arch, source: JavaSource) -> JavaRequirement {
        let mut requirement = coblet_platform_requirement();
        requirement.major = major;
        requirement.arch = arch;
        requirement.source = source;
        requirement
    }

    /// Builds a requirement through the public pipeline, since the struct is non-exhaustive.
    fn coblet_platform_requirement() -> JavaRequirement {
        use coblet_mojang::{RawVersionJson, VersionTraits};
        use coblet_platform::{ProfileEngine, ResolvedProfile};
        use coblet_types::Platform;
        let raw = RawVersionJson::from_slice(
        br#"{"id":"x","mainClass":"a.B","javaVersion":{"component":"jre-legacy","majorVersion":8}}"#,
    )
    .unwrap();
        let profile = ResolvedProfile::from_version(raw, VersionTraits::new()).unwrap();
        ProfileEngine::builtin()
            .resolve(&profile, &Platform::new(OsFamily::MacOs, Arch::Aarch64))
            .unwrap()
            .java
    }

    #[test]
    fn reads_the_index() {
        let index = RuntimeIndex::from_slice(&routes()[0].1).unwrap();
        assert_eq!(
            index.build("mac-os", "jre-legacy").unwrap().version.name,
            "8u74"
        );
        assert!(index.build("mac-os-arm64", "jre-legacy").is_none());
        assert_eq!(
            index.platforms().collect::<Vec<_>>(),
            ["mac-os", "mac-os-arm64"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn installs_a_runtime_and_selects_it() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Files {
            routes: routes(),
            requested: Mutex::default(),
        });
        let storage = Arc::new(TokioStorage::new());
        let downloader = Arc::new(Downloader::new(
            transport.clone(),
            storage.clone(),
            DownloaderConfig::default(),
        ));
        let mut providers = JavaProviders::new();
        providers
            .register(Arc::new(
                MojangRuntimes::new().with_index_url(HttpUrl::new(INDEX_URL).unwrap()),
            ))
            .unwrap();
        let installer =
            JavaInstaller::new(downloader, storage.clone(), dir.path().join("runtimes"))
                .with_providers(providers);
        let catalog = CatalogContext::new(
            MetaCache::new(transport.clone(), storage.clone(), dir.path().join("meta")),
            CachePolicy::Revalidate,
        );
        let search = SearchPaths::default().with_runtimes(dir.path().join("runtimes"));
        let spawner = TokioSpawner::new();
        let os = OsFamily::MacOs;
        let progress = coblet_runtime::TransferProgress::new();
        let cx = JavaContext {
            storage: storage.as_ref(),
            spawner: &spawner,
            os: &os,
            search: &search,
            installer: Some(&installer),
            catalog: &catalog,
            progress: Some(&progress),
        };
        let cancel = CancellationToken::new();

        // The requirement comes from the Rosetta profile: Java 8, x86_64, runtime `mac-os`.
        let wanted = coblet_platform_requirement();
        assert_eq!(wanted.runtime_platform.as_deref(), Some("mac-os"));
        let install = find_or_install(cx, &wanted, &cancel).await.unwrap();
        assert_eq!((install.major, install.arch.clone()), (8, Arch::X86_64));
        let mojang_source = InstallSource::Provider {
            provider: ExtensionId::new("coblet:mojang").unwrap(),
            package: "jre-legacy-mac-os".to_owned(),
        };
        assert_eq!(install.source, mojang_source);
        let link = dir.path().join(
            "runtimes/coblet-mojang/jre-legacy-mac-os/jre.bundle/Contents/Home/bin/java-link",
        );
        assert_eq!(
            std::fs::read_link(link).unwrap(),
            std::path::Path::new("java")
        );

        let counted = progress.snapshot();
        assert!(counted.files_total > 0, "{counted:?}");
        assert_eq!(counted.files_done, counted.files_total);
        assert_eq!(Some(counted.bytes_done), counted.bytes_total);

        // Second call finds the installed runtime without downloading.
        let requests = transport.requested.lock().unwrap().len();
        let again = find_or_install(cx, &wanted, &cancel).await.unwrap();
        assert_eq!(again.executable, install.executable);
        assert_eq!(again.source, mojang_source);
        assert_eq!(transport.requested.lock().unwrap().len(), requests);

        let system_only = requirement(17, Arch::Aarch64, JavaSource::SystemOnly);
        assert!(matches!(
            find_or_install(cx, &system_only, &cancel).await,
            Err(JavaError::NotFound { major: 17, .. })
        ));
        let mut arm = requirement(8, Arch::Aarch64, JavaSource::Any);
        arm.runtime_platform = Some("mac-os-arm64".to_owned());
        match find_or_install(cx, &arm, &cancel).await {
            Err(JavaError::NotFound { reason, .. }) => {
                assert_eq!(reason, "coblet:mojang: no matching package");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discovers_sdkman_java_home_and_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let write = |relative: &str, script: String| {
            let path = dir.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        write(
            "home/.sdkman/candidates/java/17.0.21-tem/bin/java",
            fake_java("17.0.21", "aarch64"),
        );
        write(
            "home/.sdkman/candidates/java/current/bin/java",
            fake_java("17.0.21", "aarch64"),
        );
        write("jdk21/bin/java", fake_java("21.0.8", "aarch64"));
        write("broken/bin/java", "#!/bin/sh\nexit 1\n".to_owned());
        let search = SearchPaths {
            java_home: Some(dir.path().join("jdk21")),
            path: vec![dir.path().join("broken/bin")],
            home: Some(dir.path().join("home")),
            ..SearchPaths::default()
        };
        let storage = TokioStorage::new();
        let found = candidates(&storage, &OsFamily::Linux, &search).await;
        let sources: Vec<_> = found.iter().map(|(_, source)| source.clone()).collect();
        assert_eq!(
            sources,
            [
                InstallSource::JavaHome,
                InstallSource::Path,
                InstallSource::Sdkman
            ]
        );
        let installs = discover(&storage, &TokioSpawner::new(), &OsFamily::Linux, &search).await;
        let versions: Vec<_> = installs.iter().map(|install| install.major).collect();
        assert_eq!(versions, [21, 17]);
        let wanted = requirement(17, Arch::Aarch64, JavaSource::Any);
        assert_eq!(
            crate::select(&installs, &wanted).unwrap().version,
            "17.0.21"
        );
        assert!(
            crate::select(&installs, &requirement(17, Arch::X86_64, JavaSource::Any)).is_none()
        );
    }
}
