//! A Mod Loader defined only here, in code and as data, goes through the
//! same registry, listing, recipe and Compose path as a built-in one.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on any error"
)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use coblet_catalog::{CachePolicy, CatalogContext, Filter, MetaCache};
use coblet_loader::kit::{SplitRule, VersionJsonImport};
use coblet_loader::{
    DataLoader, InstallRecipe, LoaderContext, LoaderError, LoaderRegistry, LoaderVersion, ModLoader,
};
use coblet_mojang::{RawVersionJson, TraitTable};
use coblet_platform::{ProfileEngine, ResolvedProfile};
use coblet_runtime::{
    ArtifactKind, BoxFuture, Extension, MemoryBody, PrefixRewriter, Request, Response,
    TokioStorage, Transport, TransportError,
};
use coblet_types::{Arch, ExtensionId, GameVersionId, OsFamily, Platform};

/// Serves fixed documents by address and records what was asked.
struct Served {
    documents: BTreeMap<String, String>,
    asked: Mutex<Vec<String>>,
}

impl Transport for Served {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let url = request.url().as_str().to_owned();
        self.asked.lock().unwrap().push(url.clone());
        let found = self.documents.get(&url).cloned();
        Box::pin(async move {
            Ok(match found {
                Some(body) => Response::new(200, Vec::new(), MemoryBody::new(body)),
                None => Response::new(404, Vec::new(), MemoryBody::new("")),
            })
        })
    }
}

const VERSIONS: &str = r#"[
    { "version": "2.0.0-rc.1", "stable": false },
    { "version": "1.4.2", "stable": true },
    { "version": "1.4.1", "stable": true }
]"#;

const PROFILE_OBFUSCATED: &str = r#"{
    "id": "fake-loader-1.4.2-1.20.1", "inheritsFrom": "1.20.1",
    "mainClass": "test.fake.Knot",
    "arguments": { "jvm": ["-Dfake.side=client"], "game": [] },
    "libraries": [
        { "name": "test.fake:mappings:1.20.1", "url": "https://maven.fake.test/" },
        { "name": "test.fake:fake-loader:1.4.2", "url": "https://maven.fake.test/" },
        { "name": "com.google.guava:guava:33.0-fake", "url": "https://maven.fake.test/" }
    ]
}"#;

const PROFILE_PLAIN: &str = r#"{
    "id": "fake-loader-1.4.2-26.3", "inheritsFrom": "26.3",
    "mainClass": "test.fake.Knot",
    "libraries": [{ "name": "test.fake:fake-loader:1.4.2", "url": "https://maven.fake.test/" }]
}"#;

/// The fake loader written as code: it lists versions from its own server
/// and builds the recipe with the kit's profile import.
struct FakeLoader(ExtensionId);

impl Extension for FakeLoader {
    fn id(&self) -> &ExtensionId {
        &self.0
    }

    fn display_name(&self) -> &'static str {
        "Fake Loader"
    }
}

#[derive(serde::Deserialize)]
struct Listed {
    version: String,
    stable: bool,
}

impl ModLoader for FakeLoader {
    fn component_uid(&self) -> &'static str {
        "test.fake.loader"
    }

    fn versions<'a>(
        &'a self,
        cx: &'a LoaderContext<'a>,
        game: &'a GameVersionId,
    ) -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>> {
        Box::pin(async move {
            let listed: Vec<Listed> = cx
                .fetch_json(
                    &format!("https://meta.fake.test/{game}"),
                    &format!("{game}.json"),
                )
                .await?;
            let mut versions: Vec<LoaderVersion> = listed
                .into_iter()
                .map(|entry| {
                    let version = LoaderVersion::new(entry.version);
                    if entry.stable {
                        version
                    } else {
                        version.with_type("rc".into())
                    }
                })
                .collect();
            if let Some(first) = versions
                .iter_mut()
                .find(|version| version.version_type == coblet_types::VersionType::Release)
            {
                first.recommended = true;
            }
            Ok(versions)
        })
    }

    fn install<'a>(
        &'a self,
        cx: &'a LoaderContext<'a>,
        game: &'a GameVersionId,
        version: &'a LoaderVersion,
    ) -> BoxFuture<'a, Result<InstallRecipe, LoaderError>> {
        Box::pin(async move {
            let url = format!("https://meta.fake.test/{game}/{}/profile", version.id);
            let body = cx
                .fetch(&url, &format!("{game}/{}.json", version.id))
                .await?;
            let raw = RawVersionJson::from_slice(&body)
                .map_err(|error| LoaderError::invalid(cx.loader(), error.to_string()))?;
            let import = VersionJsonImport::new(self.component_uid(), &version.id)
                .named("Fake Loader")
                .with_split(SplitRule::new("test.fake:mappings:*", "test.fake.mappings"));
            Ok(InstallRecipe::new(import.components(raw)))
        })
    }
}

/// The same loader written as data.
const FAKE_DATA: &str = r#"
    id = "test:fake-data"
    name = "Fake Loader (data)"
    uid = "test.fake.loader"

    [versions]
    format = "json"
    url = "https://meta.fake.test/{game}"
    id = "/version"
    stable = "/stable"

    [profile]
    format = "version-json"
    url = "https://meta.fake.test/{game}/{version}/profile"

    [[profile.split]]
    library = "test.fake:mappings:*"
    uid = "test.fake.mappings"
"#;

fn served() -> Arc<Served> {
    let documents = [
        ("https://mirror.fake.test/1.20.1", VERSIONS),
        ("https://mirror.fake.test/26.3", VERSIONS),
        (
            "https://mirror.fake.test/1.20.1/1.4.2/profile",
            PROFILE_OBFUSCATED,
        ),
        ("https://mirror.fake.test/26.3/1.4.2/profile", PROFILE_PLAIN),
    ];
    Arc::new(Served {
        documents: documents
            .into_iter()
            .map(|(url, body)| (url.to_owned(), body.to_owned()))
            .collect(),
        asked: Mutex::new(Vec::new()),
    })
}

fn meta(dir: &Path, transport: Arc<Served>) -> MetaCache {
    // Loader requests are mirrored, so every listing and profile proves its kind.
    let mirror = PrefixRewriter::new("https://meta.fake.test/", "https://mirror.fake.test/")
        .unwrap()
        .for_kinds([ArtifactKind::Loader]);
    MetaCache::new(transport, Arc::new(TokioStorage::new()), dir).with_rewriter(Arc::new(mirror))
}

fn base(game: &str) -> ResolvedProfile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../../../fixtures/mojang/{game}.json"));
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    ResolvedProfile::from_version(raw, traits).unwrap()
}

fn registry() -> LoaderRegistry {
    let mut loaders = LoaderRegistry::new();
    loaders
        .register(Arc::new(FakeLoader(
            ExtensionId::new("test:fake-loader").unwrap(),
        )))
        .unwrap();
    loaders
        .register(Arc::new(DataLoader::from_toml(FAKE_DATA).unwrap()))
        .unwrap();
    loaders
}

async fn select_install_and_compose(
    loaders: &LoaderRegistry,
    meta: &MetaCache,
    id: &str,
    game: &str,
) -> coblet_platform::EffectiveLaunchProfile {
    let id = ExtensionId::new(id).unwrap();
    let game: GameVersionId = game.parse().unwrap();
    let catalog = loaders.catalog(&id, meta.clone()).unwrap();
    let versions = catalog
        .list(&Filter::new().compatible_with(game.clone()))
        .await
        .unwrap();
    assert_eq!(versions.len(), 3);
    let chosen = catalog.recommended(&versions).unwrap();
    assert_eq!(chosen.id, "1.4.2");

    let cx = CatalogContext::new(meta.clone(), CachePolicy::PreferCache);
    let recipe = loaders.install(&cx, &id, &game, chosen).await.unwrap();
    let composed = base(game.as_str()).compose(&recipe.components).unwrap();
    assert_eq!(
        loaders.detect(&composed.components)[0].version,
        "1.4.2",
        "the stack is recognised as this loader"
    );
    ProfileEngine::builtin()
        .resolve(&composed, &Platform::new(OsFamily::Linux, Arch::X86_64))
        .unwrap()
}

fn classpath(profile: &coblet_platform::EffectiveLaunchProfile) -> Vec<&str> {
    profile.classpath().map(|file| file.path.as_str()).collect()
}

#[tokio::test]
async fn code_and_data_loaders_take_the_same_path() {
    let dir = tempfile::tempdir().unwrap();
    let transport = served();
    let meta = meta(dir.path(), Arc::clone(&transport));
    let loaders = registry();

    for id in ["test:fake-loader", "test:fake-data"] {
        let profile = select_install_and_compose(&loaders, &meta, id, "1.20.1").await;
        assert_eq!(profile.main_class, "test.fake.Knot", "{id}");
        let path = classpath(&profile);
        assert!(
            path.contains(&"test/fake/mappings/1.20.1/mappings-1.20.1.jar"),
            "{id}"
        );
        assert!(
            path.contains(&"test/fake/fake-loader/1.4.2/fake-loader-1.4.2.jar"),
            "{id}"
        );
        let guava: Vec<&&str> = path
            .iter()
            .filter(|jar| jar.starts_with("com/google/guava/guava/"))
            .collect();
        assert_eq!(
            guava,
            [&"com/google/guava/guava/33.0-fake/guava-33.0-fake.jar"],
            "the loader's guava replaces the game's: {id}"
        );
        let uids = profile_uids(&loaders, &meta, id, "1.20.1").await;
        assert_eq!(
            uids,
            [
                "net.minecraft",
                "org.lwjgl3",
                "test.fake.mappings",
                "test.fake.loader"
            ]
        );

        let plain = select_install_and_compose(&loaders, &meta, id, "26.3").await;
        assert!(
            classpath(&plain)
                .iter()
                .all(|jar| !jar.contains("mappings")),
            "26.3 metadata has no mappings, so no mapping component: {id}"
        );
        assert_eq!(
            profile_uids(&loaders, &meta, id, "26.3").await,
            ["net.minecraft", "org.lwjgl3", "test.fake.loader"]
        );
    }
    assert!(
        transport
            .asked
            .lock()
            .unwrap()
            .iter()
            .all(|url| url.starts_with("https://mirror.fake.test/")),
        "every loader request went through the mirror rule"
    );
}

async fn profile_uids(
    loaders: &LoaderRegistry,
    meta: &MetaCache,
    id: &str,
    game: &str,
) -> Vec<String> {
    let id = ExtensionId::new(id).unwrap();
    let game: GameVersionId = game.parse().unwrap();
    let cx = CatalogContext::new(meta.clone(), CachePolicy::PreferCache);
    let recipe = loaders
        .install(&cx, &id, &game, &LoaderVersion::new("1.4.2"))
        .await
        .unwrap();
    let composed = base(game.as_str()).compose(&recipe.components).unwrap();
    composed
        .components
        .iter()
        .map(|component| component.uid.clone())
        .collect()
}

#[tokio::test]
async fn a_third_party_loader_replaces_a_built_in_one_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let meta = meta(dir.path(), served());
    let mut loaders = LoaderRegistry::new();
    let builtin =
        DataLoader::from_toml(&FAKE_DATA.replace("test:fake-data", "coblet:fake-builtin")).unwrap();
    loaders.register(Arc::new(builtin)).unwrap();
    let id = ExtensionId::new("coblet:fake-builtin").unwrap();
    assert!(
        loaders.register(Arc::new(FakeLoader(id.clone()))).is_err(),
        "registering never overrides"
    );
    let old = loaders.replace(Arc::new(FakeLoader(id.clone()))).unwrap();
    assert_eq!(old.display_name(), "Fake Loader (data)");
    assert_eq!(loaders.get(&id).unwrap().display_name(), "Fake Loader");

    let profile =
        select_install_and_compose(&loaders, &meta, "coblet:fake-builtin", "1.20.1").await;
    assert_eq!(profile.main_class, "test.fake.Knot");
}
