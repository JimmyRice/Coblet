use std::sync::{Arc, Mutex};

use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
use coblet_download::{Downloader, DownloaderConfig};
use coblet_platform::{JavaRequirement, JavaSource};
use coblet_runtime::{
    BoxFuture, CancellationToken, HttpUrl, MemoryBody, Request, Response, StreamHasher,
    TokioSpawner, TokioStorage, Transport, TransportError,
};
use coblet_types::{Arch, HashAlgorithm, OsFamily};

use super::{RuntimeIndex, RuntimeInstaller, find_or_install};
use crate::{InstallSource, JavaContext, JavaError, SearchPaths, candidates, discover};

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
    let installer = RuntimeInstaller::new(downloader, storage.clone(), dir.path().join("runtimes"))
        .with_index_url(HttpUrl::new(INDEX_URL).unwrap());
    let catalog = CatalogContext::new(
        MetaCache::new(transport.clone(), storage.clone(), dir.path().join("meta")),
        CachePolicy::Revalidate,
    );
    let search = SearchPaths::default().with_runtimes(dir.path().join("runtimes"));
    let spawner = TokioSpawner::new();
    let os = OsFamily::MacOs;
    let cx = JavaContext {
        storage: storage.as_ref(),
        spawner: &spawner,
        os: &os,
        search: &search,
        installer: Some(&installer),
        catalog: &catalog,
    };
    let cancel = CancellationToken::new();

    // The requirement comes from the Rosetta profile: Java 8, x86_64, runtime `mac-os`.
    let wanted = coblet_platform_requirement();
    assert_eq!(wanted.runtime_platform.as_deref(), Some("mac-os"));
    let install = find_or_install(cx, &wanted, &cancel).await.unwrap();
    assert_eq!((install.major, install.arch.clone()), (8, Arch::X86_64));
    assert_eq!(
        install.source,
        InstallSource::MojangRuntime {
            component: "jre-legacy".to_owned()
        }
    );
    let link = dir
        .path()
        .join("runtimes/jre-legacy/mac-os/jre.bundle/Contents/Home/bin/java-link");
    assert_eq!(
        std::fs::read_link(link).unwrap(),
        std::path::Path::new("java")
    );

    // Second call finds the installed runtime without downloading.
    let requests = transport.requested.lock().unwrap().len();
    let again = find_or_install(cx, &wanted, &cancel).await.unwrap();
    assert_eq!(again.executable, install.executable);
    assert_eq!(transport.requested.lock().unwrap().len(), requests);

    let system_only = requirement(17, Arch::Aarch64, JavaSource::SystemOnly);
    assert!(matches!(
        find_or_install(cx, &system_only, &cancel).await,
        Err(JavaError::NotFound { major: 17, .. })
    ));
    let mut arm = requirement(8, Arch::Aarch64, JavaSource::Any);
    arm.runtime_platform = Some("mac-os-arm64".to_owned());
    assert!(matches!(
        find_or_install(cx, &arm, &cancel).await,
        Err(JavaError::NotAvailable { .. })
    ));
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
    assert!(crate::select(&installs, &requirement(17, Arch::X86_64, JavaSource::Any)).is_none());
}
