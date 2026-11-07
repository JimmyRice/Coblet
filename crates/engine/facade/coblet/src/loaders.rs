use std::path::PathBuf;
use std::sync::{Arc, PoisonError};

use coblet_catalog::{Catalog, Filter};
use coblet_launch::{GameSession, LaunchRequest};
use coblet_loader::{InstallRecipe, LoaderError, LoaderRegistry, LoaderVersion};
use coblet_types::{ExtensionId, GameVersionId};

use crate::{Coblet, Error};

impl Coblet {
    /// Returns the Mod Loaders: those added with
    /// [`CobletBuilder::loader`](crate::CobletBuilder::loader) and its relatives.
    #[must_use]
    pub fn loaders(&self) -> &LoaderRegistry {
        &self.inner.loaders
    }

    /// Returns the version catalog of a Mod Loader, kept for the life of this
    /// `Coblet`; [`Catalog::refresh`] fetches it again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Loader`] when no loader has this ID.
    pub fn loader_catalog(
        &self,
        loader: &ExtensionId,
    ) -> Result<Arc<Catalog<LoaderVersion>>, Error> {
        let mut catalogs = self
            .inner
            .loader_catalogs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(catalog) = catalogs.get(loader) {
            return Ok(Arc::clone(catalog));
        }
        let catalog = Arc::new(
            self.inner
                .loaders
                .catalog(loader, self.inner.meta.clone())?
                .with_events(self.inner.events.clone()),
        );
        catalogs.insert(loader.clone(), Arc::clone(&catalog));
        Ok(catalog)
    }

    /// Lists the versions of a Mod Loader for a game version, newest first
    /// where the source gives release times and in source order otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Loader`] for an unknown loader and [`Error::Catalog`]
    /// when no source answers.
    pub async fn loader_versions(
        &self,
        loader: &ExtensionId,
        game: &GameVersionId,
    ) -> Result<Vec<LoaderVersion>, Error> {
        let catalog = self.loader_catalog(loader)?;
        Ok(catalog
            .list(&Filter::new().compatible_with(game.clone()))
            .await?)
    }

    /// Returns the version to preselect: the one the loader recommends,
    /// otherwise the newest release, otherwise the first; `None` when the
    /// loader has no version for the game version.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`loader_versions`](Self::loader_versions).
    pub async fn recommended_loader_version(
        &self,
        loader: &ExtensionId,
        game: &GameVersionId,
    ) -> Result<Option<LoaderVersion>, Error> {
        let catalog = self.loader_catalog(loader)?;
        let versions = catalog
            .list(&Filter::new().compatible_with(game.clone()))
            .await?;
        Ok(catalog.recommended(&versions).cloned())
    }

    /// Finds a Mod Loader version by its version string.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Loader`] with [`LoaderError::UnknownVersion`] when
    /// the loader does not list it, and the errors of
    /// [`loader_versions`](Self::loader_versions).
    pub async fn loader_version(
        &self,
        loader: &ExtensionId,
        game: &GameVersionId,
        version: &str,
    ) -> Result<LoaderVersion, Error> {
        self.loader_versions(loader, game)
            .await?
            .into_iter()
            .find(|candidate| candidate.id == version)
            .ok_or_else(|| {
                LoaderError::UnknownVersion {
                    loader: loader.clone(),
                    game: game.clone(),
                    version: version.to_owned(),
                }
                .into()
            })
    }

    /// Asks a Mod Loader what installing a version adds to an Instance.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Loader`] when the loader is unknown, fails or returns
    /// a recipe without its own component.
    pub async fn loader_recipe(
        &self,
        loader: &ExtensionId,
        game: &GameVersionId,
        version: &LoaderVersion,
    ) -> Result<InstallRecipe, Error> {
        let cx = self.catalog_context();
        let recipe = self.inner.loaders.install(&cx, loader, game, version).await;
        self.forward_warnings(&cx);
        Ok(recipe?)
    }

    /// Resolves a game version with a Mod Loader version, prepares its files
    /// and starts it.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`version`](Self::version),
    /// [`loader_version`](Self::loader_version),
    /// [`loader_recipe`](Self::loader_recipe) and
    /// [`resolve_with`](Self::resolve_with), and [`Error::Launch`] for the
    /// launch step that failed.
    pub async fn launch_with_loader(
        &self,
        version_id: &str,
        loader: &ExtensionId,
        loader_version: &str,
        game_directory: impl Into<PathBuf>,
        request: LaunchRequest,
    ) -> Result<GameSession, Error> {
        let version = self.version(version_id).await?;
        let chosen = self
            .loader_version(loader, &version.id, loader_version)
            .await?;
        let recipe = self.loader_recipe(loader, &version.id, &chosen).await?;
        let profile = self.resolve_with(&version, &recipe.components).await?;
        self.launch_profile(profile, game_directory, request).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use coblet_launch::{GameLayout, command_profile};
    use coblet_launch_command::{JavaExecutable, LaunchAuth, LaunchCommand, LaunchOptions};
    use coblet_loader::{InstallRecipe, LoaderContext, LoaderError, LoaderVersion, ModLoader};
    use coblet_platform::{Component, ComponentPatch, Requirement};
    use coblet_runtime::{
        BoxFuture, Extension, MemoryBody, Request, Response, Transport, TransportError,
    };
    use coblet_types::{Arch, ExtensionId, GameVersionId, OsFamily, Platform, PlayerName};

    use crate::{Coblet, Error};

    /// Serves the Mojang fixtures by file name and the fake loader's listing.
    struct Fixtures;

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
                if url == "https://meta.fake.test/versions/1.20.1" {
                    let body = r#"[{ "version": "2.0", "stable": false }, { "version": "1.4.2", "stable": true }]"#;
                    return Ok(Response::new(200, Vec::new(), MemoryBody::new(body)));
                }
                let name = url.rsplit('/').next().unwrap_or_default();
                let dir =
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../fixtures/mojang");
                Ok(match std::fs::read(dir.join(name)) {
                    Ok(bytes) => Response::new(200, Vec::new(), MemoryBody::new(bytes)),
                    Err(_) => Response::new(404, Vec::new(), MemoryBody::new(Vec::new())),
                })
            })
        }
    }

    const FAKE_DATA: &str = r#"
        id = "test:fake-data"
        name = "Fake Loader"
        uid = "test.fake.loader"
        [versions]
        format = "json"
        url = "https://meta.fake.test/versions/{game}"
        id = "/version"
        stable = "/stable"
        [profile]
        format = "static"
        [[profile.components]]
        uid = "test.fake.loader"
        version = "{version}"
        requires = [{ uid = "net.minecraft", equals = "{game}" }]
        mainClass = "test.fake.Knot"
        "+tweakers" = ["test.fake.Tweaker"]
        libraries = [{ name = "test.fake:fake-loader:{version}", url = "https://maven.fake.test/" }]
    "#;

    /// Code form of the fake loader, registered over a data loader of the same ID.
    struct FakeCode(ExtensionId);

    impl Extension for FakeCode {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Fake Loader (code)"
        }
    }

    impl ModLoader for FakeCode {
        fn component_uid(&self) -> &'static str {
            "test.fake.loader"
        }

        fn versions<'a>(
            &'a self,
            _cx: &'a LoaderContext<'a>,
            _game: &'a GameVersionId,
        ) -> BoxFuture<'a, Result<Vec<LoaderVersion>, LoaderError>> {
            Box::pin(async { Ok(vec![LoaderVersion::new("9.0").recommended()]) })
        }

        fn install<'a>(
            &'a self,
            _cx: &'a LoaderContext<'a>,
            game: &'a GameVersionId,
            version: &'a LoaderVersion,
        ) -> BoxFuture<'a, Result<InstallRecipe, LoaderError>> {
            Box::pin(async move {
                let mut patch = ComponentPatch::new("test.fake.loader", version.id.clone());
                patch
                    .requires
                    .push(Requirement::new("net.minecraft").equals(game.as_str()));
                patch.main_class = Some("test.fake.CodeMain".to_owned());
                Ok(InstallRecipe::new(vec![Component::new(patch)]))
            })
        }
    }

    fn builder(root: &std::path::Path) -> crate::CobletBuilder<crate::WithRoot> {
        Coblet::builder()
            .data_root(root)
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::Linux, Arch::X86_64))
    }

    #[tokio::test]
    async fn selects_installs_and_builds_the_command_of_a_data_loader() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(dir.path()).loader_toml(FAKE_DATA).build().unwrap();
        let id = ExtensionId::new("test:fake-data").unwrap();
        let game: GameVersionId = "1.20.1".parse().unwrap();

        let versions = coblet.loader_versions(&id, &game).await.unwrap();
        assert_eq!(versions.len(), 2);
        let chosen = coblet
            .recommended_loader_version(&id, &game)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chosen.id, "1.4.2");
        assert!(matches!(
            coblet.loader_version(&id, &game, "0.1").await,
            Err(Error::Loader(LoaderError::UnknownVersion { .. }))
        ));

        let recipe = coblet.loader_recipe(&id, &game, &chosen).await.unwrap();
        let version = coblet.version("1.20.1").await.unwrap();
        let profile = coblet
            .resolve_with(&version, &recipe.components)
            .await
            .unwrap();
        let layout = GameLayout::new(dir.path(), dir.path().join("game"));
        let options = LaunchOptions::new(
            JavaExecutable::new("/jdk/bin/java"),
            LaunchAuth::offline(PlayerName::new("Steve").unwrap()),
            dir.path().join("game"),
        )
        .with_assets_root(dir.path().join("assets"))
        .with_natives_directory(dir.path().join("natives"))
        .with_library_directory(dir.path().join("libraries"));
        let command =
            LaunchCommand::from_profile(&command_profile(&profile, &layout), &options).unwrap();
        let args: Vec<String> = command
            .args()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let main = args.iter().position(|arg| arg == "test.fake.Knot").unwrap();
        let classpath = &args[args.iter().position(|arg| arg == "-cp").unwrap() + 1];
        assert!(classpath.contains("test/fake/fake-loader/1.4.2/fake-loader-1.4.2.jar"));
        assert!(
            args[main..]
                .windows(2)
                .any(|pair| pair == ["--tweakClass", "test.fake.Tweaker"])
        );
        assert_eq!(
            coblet.loaders().detect(&[coblet_platform::ComponentRef {
                uid: "test.fake.loader".to_owned(),
                version: "1.4.2".to_owned(),
            }])[0]
                .loader,
            id
        );
    }

    #[tokio::test]
    async fn loaders_follow_the_registry_rules() {
        let dir = tempfile::tempdir().unwrap();
        let id = ExtensionId::new("test:fake-data").unwrap();
        let duplicate = builder(dir.path())
            .loader_toml(FAKE_DATA)
            .loader(Arc::new(FakeCode(id.clone())))
            .build()
            .unwrap_err();
        assert!(matches!(duplicate, Error::Registry(_)), "{duplicate}");

        let replaced = builder(dir.path())
            .loader_toml(FAKE_DATA)
            .replace_loader(Arc::new(FakeCode(id.clone())))
            .build()
            .unwrap();
        let game: GameVersionId = "1.20.1".parse().unwrap();
        let chosen = replaced
            .recommended_loader_version(&id, &game)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chosen.id, "9.0");
        let recipe = replaced.loader_recipe(&id, &game, &chosen).await.unwrap();
        let version = replaced.version("1.20.1").await.unwrap();
        let profile = replaced
            .resolve_with(&version, &recipe.components)
            .await
            .unwrap();
        assert_eq!(profile.main_class, "test.fake.CodeMain");

        let wrong_game = replaced.version("1.12.2").await.unwrap();
        let error = replaced
            .resolve_with(&wrong_game, &recipe.components)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "component test.fake.loader requires net.minecraft 1.20.1, found 1.12.2"
        );

        let bad = builder(dir.path())
            .loader_toml("id = 1")
            .build()
            .unwrap_err();
        assert!(
            matches!(bad, Error::Loader(LoaderError::InvalidDefinition { .. })),
            "{bad}"
        );
        let orphan = builder(dir.path())
            .loader_version_source(
                ExtensionId::new("test:missing").unwrap(),
                Arc::new(NoVersions(ExtensionId::new("test:missing").unwrap())),
            )
            .build()
            .unwrap_err();
        assert!(
            matches!(orphan, Error::Loader(LoaderError::UnknownLoader(_))),
            "{orphan}"
        );
        assert!(matches!(
            replaced.loader_catalog(&ExtensionId::new("test:missing").unwrap()),
            Err(Error::Loader(LoaderError::UnknownLoader(_)))
        ));
    }

    struct NoVersions(ExtensionId);

    impl Extension for NoVersions {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "None"
        }
    }

    impl coblet_catalog::CatalogSource<LoaderVersion> for NoVersions {
        fn fetch<'a>(
            &'a self,
            _cx: &'a coblet_catalog::CatalogContext,
            _query: &'a coblet_catalog::CatalogQuery,
        ) -> BoxFuture<
            'a,
            Result<coblet_catalog::CatalogPage<LoaderVersion>, coblet_catalog::CatalogError>,
        > {
            Box::pin(async { Ok(coblet_catalog::CatalogPage::new(Vec::new())) })
        }
    }
}
