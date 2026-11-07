use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};

use coblet::catalog::{CatalogContext, Timestamp};
use coblet::loader::{InstallRecipe, LoaderError, LoaderVersion};
use coblet::platform::EffectiveLaunchProfile;
use coblet::runtime::{BoxFuture, Extension};
use coblet::types::{ExtensionId, GameVersionId, VersionType};

use crate::{Coblet, CobletError, ErrorKind};

fn invalid(message: impl Into<String>) -> CobletError {
    CobletError::new(ErrorKind::InvalidArgument, message)
}

pub(crate) fn loader_id(id: &str) -> Result<ExtensionId, CobletError> {
    ExtensionId::new(id).map_err(|error| invalid(format!("loader id: {error}")))
}

pub(crate) fn game_id(game: &str) -> Result<GameVersionId, CobletError> {
    game.parse()
        .map_err(|error| invalid(format!("game version: {error}")))
}

/// Mod Loader known to a [`Coblet`](crate::Coblet).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LoaderInfo {
    /// Loader ID, such as `acme:loader`.
    pub id: String,
    /// Name shown to users.
    pub name: String,
    /// UID of the component the loader installs, such as `net.acme.loader`.
    pub component_uid: String,
}

/// Version of a Mod Loader for one game version.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LoaderVersionInfo {
    /// Version string.
    pub id: String,
    /// Game version the entry is for; absent when it fits every version.
    #[uniffi(default = None)]
    pub game: Option<String>,
    /// Release channel, such as `release` or `beta`.
    #[uniffi(default = "release")]
    pub version_type: String,
    /// Release time in RFC 3339, when known.
    #[uniffi(default = None)]
    pub released: Option<String>,
    /// Whether the loader recommends this version for the game version.
    #[uniffi(default = false)]
    pub recommended: bool,
    /// Data the loader needs later to install this version, as a JSON
    /// object, such as an installer address; kept verbatim.
    #[uniffi(default = None)]
    pub data_json: Option<String>,
    /// ID of the source that listed the version: the loader's own ID or
    /// that of an added version source. Ignored when a source returns it.
    #[uniffi(default = None)]
    pub source: Option<String>,
}

impl From<&LoaderVersion> for LoaderVersionInfo {
    fn from(version: &LoaderVersion) -> Self {
        Self {
            id: version.id.clone(),
            game: version.game.as_ref().map(|game| game.as_str().to_owned()),
            version_type: version.version_type.as_str().to_owned(),
            released: version.released.map(|time| time.to_string()),
            recommended: version.recommended,
            data_json: (!version.data.is_empty())
                .then(|| serde_json::to_string(&version.data).unwrap_or_default()),
            source: version.source.as_ref().map(|id| id.as_str().to_owned()),
        }
    }
}

impl LoaderVersionInfo {
    pub(crate) fn into_version(self) -> Result<LoaderVersion, String> {
        let mut version =
            LoaderVersion::new(self.id).with_type(VersionType::from(self.version_type));
        if let Some(game) = self.game {
            version = version.for_game(game.parse().map_err(|error| format!("game: {error}"))?);
        }
        if let Some(released) = self.released {
            version = version.released_at(
                Timestamp::parse(&released).map_err(|error| format!("released: {error}"))?,
            );
        }
        if self.recommended {
            version = version.recommended();
        }
        if let Some(data) = self.data_json {
            let data: BTreeMap<String, serde_json::Value> =
                serde_json::from_str(&data).map_err(|error| format!("data: {error}"))?;
            for (key, value) in data {
                version = version.with_data(key, value);
            }
        }
        Ok(version)
    }
}

/// What a resolved launch would run: the result of a game version with a
/// Mod Loader on top, before files are downloaded.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LaunchPlan {
    /// Main class.
    pub main_class: String,
    /// Maven coordinates of the libraries on the classpath, in order.
    pub classpath: Vec<String>,
    /// JVM arguments with their `${...}` placeholders.
    pub jvm_args: Vec<String>,
    /// Game arguments with their `${...}` placeholders.
    pub game_args: Vec<String>,
    /// Java major version the launch needs.
    pub java_major: u32,
    /// Report of every change made while adapting the version.
    pub report: String,
}

impl From<&EffectiveLaunchProfile> for LaunchPlan {
    fn from(profile: &EffectiveLaunchProfile) -> Self {
        Self {
            main_class: profile.main_class.clone(),
            classpath: profile
                .libraries
                .iter()
                .filter(|library| library.classpath.is_some())
                .map(|library| library.name.to_string())
                .collect(),
            jvm_args: profile
                .jvm_args
                .iter()
                .map(|arg| arg.value.clone())
                .collect(),
            game_args: profile
                .game_args
                .iter()
                .map(|arg| arg.value.clone())
                .collect(),
            java_major: profile.java.major,
            report: profile.explain(),
        }
    }
}

/// Requests a Mod Loader written in Swift, Kotlin or Python makes while
/// listing or installing.
///
/// Documents come through coblet's metadata cache as Mod Loader metadata,
/// so mirror rules, fallback to the original address and offline caching
/// apply as they do to built-in loaders. Each loader's documents are cached
/// apart from those of other loaders.
#[derive(uniffi::Object)]
pub struct LoaderContext {
    catalog: CatalogContext,
    loader: ExtensionId,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for LoaderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoaderContext")
            .field("loader", &self.loader)
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl LoaderContext {
    /// Returns the ID of the loader the requests are made for.
    #[must_use]
    pub fn loader_id(&self) -> String {
        self.loader.as_str().to_owned()
    }

    /// Fetches a document and caches it under `cache_name`, a relative path
    /// such as `versions/1.20.1.json`.
    ///
    /// The call blocks until the document is there; call it from the
    /// thread `versions` or `install` runs on.
    ///
    /// # Errors
    ///
    /// Fails for a malformed address or cache name and when the document is
    /// unavailable and not cached.
    pub fn fetch(self: Arc<Self>, url: String, cache_name: String) -> Result<Vec<u8>, CobletError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = Arc::clone(&self);
        self.runtime.spawn(async move {
            let cx = coblet::loader::LoaderContext::new(&context.catalog, &context.loader);
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

/// Mod Loader written in Swift, Kotlin or Python, registered with
/// `CobletBuilder.add_loader` like a built-in one.
///
/// Both methods run on a worker thread where blocking is fine; they fetch
/// documents with the [`LoaderContext`] they are given. `install` returns
/// the install recipe as JSON: `{"components": [...]}` with components in
/// the Prism / MultiMC component format, the loader's own component among
/// them.
#[uniffi::export(foreign)]
pub trait ModLoader: Send + Sync {
    /// Namespaced ID, such as `acme:loader`.
    fn id(&self) -> String;

    /// Name shown to users.
    fn display_name(&self) -> String;

    /// UID of the component the loader installs, such as `net.acme.loader`.
    fn component_uid(&self) -> String;

    /// Lists the loader's versions for a game version.
    ///
    /// # Errors
    ///
    /// Return an error when the versions cannot be listed.
    fn versions(
        &self,
        cx: Arc<LoaderContext>,
        game: String,
    ) -> Result<Vec<LoaderVersionInfo>, CobletError>;

    /// Returns the install recipe of a version as JSON.
    ///
    /// # Errors
    ///
    /// Return an error when the version cannot be installed.
    fn install(
        &self,
        cx: Arc<LoaderContext>,
        game: String,
        version: LoaderVersionInfo,
    ) -> Result<String, CobletError>;
}

/// Adapts a [`ModLoader`] from foreign code to the engine's trait.
pub(crate) struct ForeignLoader {
    id: ExtensionId,
    name: String,
    uid: String,
    loader: Arc<dyn ModLoader>,
}

impl ForeignLoader {
    pub(crate) fn new(loader: Arc<dyn ModLoader>) -> Result<Self, CobletError> {
        Ok(Self {
            id: loader_id(&loader.id())?,
            name: loader.display_name(),
            uid: loader.component_uid(),
            loader,
        })
    }

    /// Runs a call of the foreign loader on a blocking thread with its own
    /// context, then hands the context's warnings to `cx`.
    async fn call<T: Send + 'static>(
        &self,
        cx: &coblet::loader::LoaderContext<'_>,
        body: impl FnOnce(&dyn ModLoader, Arc<LoaderContext>) -> Result<T, CobletError> + Send + 'static,
    ) -> Result<T, LoaderError> {
        let context = Arc::new(LoaderContext {
            catalog: CatalogContext::new(cx.catalog().meta().clone(), cx.catalog().policy()),
            loader: self.id.clone(),
            runtime: tokio::runtime::Handle::current(),
        });
        let loader = Arc::clone(&self.loader);
        let given = Arc::clone(&context);
        let result = tokio::task::spawn_blocking(move || body(loader.as_ref(), given)).await;
        for warning in context.catalog.take_warnings() {
            cx.catalog().warn(warning);
        }
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(LoaderError::failed(&self.id, error.to_string())),
            Err(error) => Err(LoaderError::failed(
                &self.id,
                format!("the loader panicked: {error}"),
            )),
        }
    }
}

impl Extension for ForeignLoader {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl coblet::loader::ModLoader for ForeignLoader {
    fn component_uid(&self) -> &str {
        &self.uid
    }

    fn versions<'a>(
        &'a self,
        cx: &'a coblet::loader::LoaderContext<'a>,
        game: &'a GameVersionId,
    ) -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>> {
        Box::pin(async move {
            let game = game.as_str().to_owned();
            let listed = self
                .call(cx, move |loader, context| loader.versions(context, game))
                .await?;
            listed
                .into_iter()
                .map(|version| {
                    let id = version.id.clone();
                    version.into_version().map_err(|message| {
                        LoaderError::invalid(&self.id, format!("version {id}: {message}"))
                    })
                })
                .collect()
        })
    }

    fn install<'a>(
        &'a self,
        cx: &'a coblet::loader::LoaderContext<'a>,
        game: &'a GameVersionId,
        version: &'a LoaderVersion,
    ) -> BoxFuture<'a, Result<InstallRecipe, LoaderError>> {
        Box::pin(async move {
            let game = game.as_str().to_owned();
            let info = LoaderVersionInfo::from(version);
            let json = self
                .call(cx, move |loader, context| {
                    loader.install(context, game, info)
                })
                .await?;
            InstallRecipe::from_json(json.as_bytes())
                .map_err(|error| LoaderError::invalid(&self.id, format!("install recipe: {error}")))
        })
    }
}

/// A Mod Loader version to launch with.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LoaderChoice {
    /// Loader ID, such as `acme:loader`.
    pub loader: String,
    /// Version of the loader, as listed by `Coblet.loader_versions`.
    pub version: String,
}

impl Coblet {
    /// Resolves a version, with the components of a Mod Loader version on
    /// top when one is chosen.
    pub(crate) async fn resolve_choice(
        &self,
        version_id: &str,
        loader: Option<&LoaderChoice>,
    ) -> Result<EffectiveLaunchProfile, CobletError> {
        let version = self.inner.version(version_id).await?;
        let Some(choice) = loader else {
            return Ok(self.inner.resolve(&version).await?);
        };
        let id = loader_id(&choice.loader)?;
        let chosen = self
            .inner
            .loader_version(&id, &version.id, &choice.version)
            .await?;
        let recipe = self.inner.loader_recipe(&id, &version.id, &chosen).await?;
        Ok(self
            .inner
            .resolve_with(&version, &recipe.components)
            .await?)
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Coblet {
    /// Lists the Mod Loaders: the ones added to the builder, by ID.
    #[must_use]
    pub fn loaders(&self) -> Vec<LoaderInfo> {
        self.inner
            .loaders()
            .iter()
            .map(|loader| LoaderInfo {
                id: loader.id().as_str().to_owned(),
                name: loader.display_name().to_owned(),
                component_uid: loader.component_uid().to_owned(),
            })
            .collect()
    }

    /// Lists the versions of a Mod Loader for a game version.
    ///
    /// # Errors
    ///
    /// Fails for an unknown loader or game version and when no source answers.
    pub async fn loader_versions(
        &self,
        loader: String,
        game: String,
    ) -> Result<Vec<LoaderVersionInfo>, CobletError> {
        let versions = self
            .inner
            .loader_versions(&loader_id(&loader)?, &game_id(&game)?)
            .await?;
        Ok(versions.iter().map(LoaderVersionInfo::from).collect())
    }

    /// Returns the version to preselect: the one the loader recommends,
    /// otherwise the newest release; absent when the loader has no version
    /// for the game version.
    ///
    /// # Errors
    ///
    /// Fails like [`loader_versions`](Self::loader_versions).
    pub async fn recommended_loader_version(
        &self,
        loader: String,
        game: String,
    ) -> Result<Option<LoaderVersionInfo>, CobletError> {
        let version = self
            .inner
            .recommended_loader_version(&loader_id(&loader)?, &game_id(&game)?)
            .await?;
        Ok(version.as_ref().map(LoaderVersionInfo::from))
    }

    /// Returns the install recipe of a Mod Loader version as JSON:
    /// `{"components": [...]}` in the Prism / MultiMC component format.
    ///
    /// # Errors
    ///
    /// Fails for an unknown loader or version and when the loader fails.
    pub async fn loader_recipe(
        &self,
        loader: String,
        game: String,
        version: String,
    ) -> Result<String, CobletError> {
        let id = loader_id(&loader)?;
        let game = game_id(&game)?;
        let chosen = self.inner.loader_version(&id, &game, &version).await?;
        let recipe = self.inner.loader_recipe(&id, &game, &chosen).await?;
        serde_json::to_string(&recipe)
            .map_err(|error| CobletError::new(ErrorKind::Other, error.to_string()))
    }

    /// Resolves a version for this platform, with a Mod Loader on top when
    /// one is chosen, without downloading or starting anything.
    ///
    /// # Errors
    ///
    /// Fails for unknown versions and loaders, unmet component requirements
    /// and versions that cannot run on this platform.
    pub async fn plan_launch(
        &self,
        version_id: String,
        loader: Option<LoaderChoice>,
    ) -> Result<LaunchPlan, CobletError> {
        let profile = self.resolve_choice(&version_id, loader.as_ref()).await?;
        Ok(LaunchPlan::from(&profile))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use coblet::runtime::{
        ArtifactKind, BoxFuture, MemoryBody, PrefixRewriter, Request, Response, Transport,
        TransportError,
    };
    use coblet::types::{Arch, OsFamily, Platform};

    use super::{LoaderChoice, LoaderContext, LoaderVersionInfo, ModLoader};
    use crate::{Coblet, CobletBuilder, CobletError, ErrorKind};

    /// Serves the Mojang fixtures by file name and the fake listing from
    /// the mirror only, and records every address asked for.
    #[derive(Default)]
    struct Fixtures(Mutex<Vec<String>>);

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            self.0.lock().unwrap().push(url.clone());
            Box::pin(async move {
                if url == "https://mirror.fake.test/meta/versions/1.20.1" {
                    let body = r#"[{ "version": "2.0", "stable": false }, { "version": "1.4.2", "stable": true }]"#;
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

    /// A Mod Loader as foreign code would write it: listing over the
    /// context, recipe as JSON.
    struct Fake;

    impl ModLoader for Fake {
        fn id(&self) -> String {
            "test:fake-foreign".to_owned()
        }

        fn display_name(&self) -> String {
            "Fake Loader".to_owned()
        }

        fn component_uid(&self) -> String {
            "test.fake.loader".to_owned()
        }

        fn versions(
            &self,
            cx: Arc<LoaderContext>,
            game: String,
        ) -> Result<Vec<LoaderVersionInfo>, CobletError> {
            assert_eq!(cx.loader_id(), "test:fake-foreign");
            let text = cx.fetch_text(
                format!("https://meta.fake.test/versions/{game}"),
                format!("versions/{game}.json"),
            )?;
            let listed: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
            let mut recommended = false;
            Ok(listed
                .iter()
                .map(|entry| {
                    let stable = entry["stable"].as_bool().unwrap();
                    let pick = stable && !recommended;
                    recommended |= pick;
                    LoaderVersionInfo {
                        id: entry["version"].as_str().unwrap().to_owned(),
                        game: Some(game.clone()),
                        version_type: if stable { "release" } else { "beta" }.to_owned(),
                        released: None,
                        recommended: pick,
                        data_json: Some(r#"{"main":"test.fake.Knot"}"#.to_owned()),
                        source: None,
                    }
                })
                .collect())
        }

        fn install(
            &self,
            _cx: Arc<LoaderContext>,
            game: String,
            version: LoaderVersionInfo,
        ) -> Result<String, CobletError> {
            let data: serde_json::Value =
                serde_json::from_str(version.data_json.as_deref().unwrap()).unwrap();
            Ok(serde_json::json!({ "components": [{
                "uid": "test.fake.loader",
                "version": version.id,
                "requires": [{ "uid": "net.minecraft", "equals": game }],
                "mainClass": data["main"],
                "+tweakers": ["test.fake.Tweaker"],
                "libraries": [{
                    "name": format!("test.fake:fake-loader:{}", version.id),
                    "url": "https://maven.fake.test/"
                }]
            }]})
            .to_string())
        }
    }

    /// A loader whose calls fail, or that returns a recipe without its component.
    struct Broken;

    impl ModLoader for Broken {
        fn id(&self) -> String {
            "test:broken".to_owned()
        }

        fn display_name(&self) -> String {
            "Broken".to_owned()
        }

        fn component_uid(&self) -> String {
            "test.broken".to_owned()
        }

        fn versions(
            &self,
            _cx: Arc<LoaderContext>,
            _game: String,
        ) -> Result<Vec<LoaderVersionInfo>, CobletError> {
            Ok(vec![LoaderVersionInfo {
                id: "1.0".to_owned(),
                game: None,
                version_type: "release".to_owned(),
                released: None,
                recommended: false,
                data_json: None,
                source: None,
            }])
        }

        fn install(
            &self,
            _cx: Arc<LoaderContext>,
            _game: String,
            _version: LoaderVersionInfo,
        ) -> Result<String, CobletError> {
            Err(CobletError::new(ErrorKind::Network, "installer offline"))
        }
    }

    fn kind(error: &CobletError) -> ErrorKind {
        let CobletError::Failed { kind, .. } = error;
        *kind
    }

    fn coblet(root: &std::path::Path, transport: Arc<Fixtures>) -> Coblet {
        let mirror =
            PrefixRewriter::new("https://meta.fake.test/", "https://mirror.fake.test/meta/")
                .unwrap()
                .for_kinds([ArtifactKind::Loader]);
        let inner = coblet::Coblet::builder()
            .data_root(root)
            .transport(transport)
            .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
            .url_rewriter(Arc::new(mirror))
            .loader(Arc::new(super::ForeignLoader::new(Arc::new(Fake)).unwrap()))
            .loader(Arc::new(
                super::ForeignLoader::new(Arc::new(Broken)).unwrap(),
            ))
            .build()
            .unwrap();
        Coblet::from_inner(inner)
    }

    #[tokio::test]
    async fn foreign_loaders_list_install_and_plan_through_mirrors() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(Fixtures::default());
        let coblet = coblet(dir.path(), Arc::clone(&transport));
        let id = "test:fake-foreign".to_owned();
        let game = "1.20.1".to_owned();

        let loaders = coblet.loaders();
        let fake = loaders.iter().find(|loader| loader.id == id).unwrap();
        assert_eq!(fake.component_uid, "test.fake.loader");
        assert_eq!(fake.name, "Fake Loader");

        let versions = coblet
            .loader_versions(id.clone(), game.clone())
            .await
            .unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version_type, "beta");
        assert!(versions[1].recommended);
        let chosen = coblet
            .recommended_loader_version(id.clone(), game.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chosen.id, "1.4.2");
        assert_eq!(
            chosen.data_json.as_deref(),
            Some(r#"{"main":"test.fake.Knot"}"#)
        );
        assert!(
            transport
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|url| url == "https://mirror.fake.test/meta/versions/1.20.1")
        );

        let recipe = coblet
            .loader_recipe(id.clone(), game.clone(), "1.4.2".to_owned())
            .await
            .unwrap();
        assert!(recipe.contains(r#""uid":"test.fake.loader""#), "{recipe}");

        let plan = coblet
            .plan_launch(
                game.clone(),
                Some(LoaderChoice {
                    loader: id.clone(),
                    version: "1.4.2".to_owned(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(plan.main_class, "test.fake.Knot");
        assert!(
            plan.classpath
                .iter()
                .any(|name| name == "test.fake:fake-loader:1.4.2")
        );
        assert!(
            plan.game_args
                .windows(2)
                .any(|pair| pair == ["--tweakClass", "test.fake.Tweaker"])
        );
        let vanilla = coblet.plan_launch(game, None).await.unwrap();
        assert_eq!(vanilla.main_class, "net.minecraft.client.main.Main");
        assert_eq!(vanilla.java_major, 17);
    }

    #[tokio::test]
    async fn loader_failures_have_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = coblet(dir.path(), Arc::new(Fixtures::default()));
        let game = "1.20.1".to_owned();

        let error = coblet
            .loader_versions("test:missing".to_owned(), game.clone())
            .await
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::NotFound);
        let error = coblet
            .loader_recipe(
                "test:fake-foreign".to_owned(),
                game.clone(),
                "0.1".to_owned(),
            )
            .await
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::NotFound);
        let error = coblet
            .loader_recipe("test:broken".to_owned(), game.clone(), "1.0".to_owned())
            .await
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::Other);
        assert!(error.to_string().contains("installer offline"), "{error}");
        let error = coblet
            .plan_launch(
                game,
                Some(LoaderChoice {
                    loader: "no namespace".to_owned(),
                    version: "1.0".to_owned(),
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    }

    const FAKE_DATA: &str = r#"
        id = "test:fake-data"
        name = "Fake Loader"
        uid = "test.fake.loader"
        [versions]
        format = "static"
        list = [{ id = "1.0" }]
        [profile]
        format = "static"
        [[profile.components]]
        uid = "test.fake.loader"
        version = "{version}"
    "#;

    struct BadId;

    impl ModLoader for BadId {
        fn id(&self) -> String {
            "no namespace".to_owned()
        }

        fn display_name(&self) -> String {
            String::new()
        }

        fn component_uid(&self) -> String {
            String::new()
        }

        fn versions(
            &self,
            _cx: Arc<LoaderContext>,
            _game: String,
        ) -> Result<Vec<LoaderVersionInfo>, CobletError> {
            Ok(Vec::new())
        }

        fn install(
            &self,
            _cx: Arc<LoaderContext>,
            _game: String,
            _version: LoaderVersionInfo,
        ) -> Result<String, CobletError> {
            Ok(String::new())
        }
    }

    #[test]
    fn the_builder_keeps_the_registry_rules() {
        let dir = tempfile::tempdir().unwrap();
        let builder = CobletBuilder::new(
            dir.path().to_string_lossy().into_owned(),
            "CobletTest/1.0 (test@example.com)".to_owned(),
        )
        .unwrap();
        builder.add_loader_toml(FAKE_DATA).unwrap();
        let error = builder.add_loader_toml(FAKE_DATA).unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        assert!(error.to_string().contains("already registered"), "{error}");
        builder
            .replace_loader_toml(&FAKE_DATA.replace("Fake Loader", "Patched Loader"))
            .unwrap();
        let error = builder.add_loader_json(r#"{"id": "test:x"}"#).unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        let error = builder.add_loader(Arc::new(BadId)).unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        builder.add_loader(Arc::new(Fake)).unwrap();
        builder.replace_loader(Arc::new(Fake)).unwrap();

        let coblet = builder.build().unwrap();
        let names: Vec<(String, String)> = coblet
            .loaders()
            .into_iter()
            .map(|loader| (loader.id, loader.name))
            .collect();
        assert_eq!(
            names,
            [
                ("test:fake-data".to_owned(), "Patched Loader".to_owned()),
                ("test:fake-foreign".to_owned(), "Fake Loader".to_owned()),
            ]
        );
        let error = builder.add_loader_toml(FAKE_DATA).unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidState);
    }
}
