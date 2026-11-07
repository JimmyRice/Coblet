use std::sync::Arc;

use coblet_catalog::{Catalog, CatalogContext, CatalogSources, Filter, MetaCache};
use coblet_mojang::{
    GameVersion, MojangVersions, TraitInput, TraitReport, TraitSources, fetch_version_json,
};
use coblet_runtime::EventBus;

use crate::{Coblet, Error};

/// Game version catalog: the Mojang manifest first, unless removed, then
/// the added sources.
pub(crate) struct VersionCatalog {
    pub(crate) catalog: Catalog<GameVersion>,
    pub(crate) sources: CatalogSources<GameVersion>,
    pub(crate) mojang: Option<Arc<MojangVersions>>,
    pub(crate) traits: Arc<TraitSources>,
}

pub(crate) fn version_catalog(
    meta: MetaCache,
    events: EventBus,
    sources: CatalogSources<GameVersion>,
    mojang: Option<Arc<MojangVersions>>,
    traits: TraitSources,
) -> VersionCatalog {
    let traits = Arc::new(traits);
    VersionCatalog {
        catalog: Catalog::new(meta)
            .with_sources(&sources)
            .with_stage(Arc::clone(&traits) as _)
            .with_events(events),
        sources,
        mojang,
        traits,
    }
}

impl Coblet {
    /// Lists game versions that match `filter`, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] when the manifest is unavailable and not cached.
    pub async fn versions(&self, filter: &Filter) -> Result<Vec<GameVersion>, Error> {
        Ok(self.inner.versions.catalog.list(filter).await?)
    }

    /// Returns the version with this ID.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownVersion`] when the manifest does not list it.
    pub async fn version(&self, id: &str) -> Result<GameVersion, Error> {
        self.versions(&Filter::new())
            .await?
            .into_iter()
            .find(|version| version.id.as_str() == id)
            .ok_or_else(|| Error::UnknownVersion(id.to_owned()))
    }

    /// Returns the latest release named by the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownVersion`] when the manifest names none.
    pub async fn latest_release(&self) -> Result<GameVersion, Error> {
        self.versions(&Filter::new())
            .await?
            .into_iter()
            .find(|version| version.latest_release)
            .ok_or_else(|| Error::UnknownVersion("latest release".to_owned()))
    }

    /// Returns the Mojang version source, for its manifest address; `None`
    /// when the builder removed or replaced `coblet:mojang`.
    #[must_use]
    pub fn mojang_versions(&self) -> Option<&Arc<MojangVersions>> {
        self.inner.versions.mojang.as_ref()
    }

    /// Returns the game version sources in the order they are asked.
    #[must_use]
    pub fn version_sources(&self) -> &CatalogSources<GameVersion> {
        &self.inner.versions.sources
    }

    /// Returns the version trait sources, in the order they run.
    #[must_use]
    pub fn trait_sources(&self) -> &TraitSources {
        &self.inner.versions.traits
    }

    /// Returns the traits of a version with its version JSON, and which
    /// trait source added or removed each.
    ///
    /// Trait sources that fail are skipped and reported as
    /// `version.trait-source-failed` warnings.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] when the version JSON is unavailable.
    pub async fn version_traits(&self, version: &GameVersion) -> Result<TraitReport, Error> {
        let cx = self.catalog_context();
        let document = fetch_version_json(&cx, version).await;
        self.forward_warnings(&cx);
        let input = TraitInput::listed(version).with_version(Arc::new(document?));
        Ok(self.traits_of(&cx, input, version).await)
    }

    /// Runs the trait sources for one version and publishes their failures.
    pub(crate) async fn traits_of(
        &self,
        cx: &CatalogContext,
        input: TraitInput,
        version: &GameVersion,
    ) -> TraitReport {
        let (report, failures) = self
            .inner
            .versions
            .traits
            .report(cx, input, version.declared_traits.clone())
            .await;
        self.forward_trait_failures(cx, &failures);
        report
    }

    /// Fetches the version manifest again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Catalog`] when the manifest cannot be fetched.
    pub async fn refresh_versions(&self) -> Result<(), Error> {
        Ok(self.inner.versions.catalog.refresh().await?)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use coblet_catalog::{CatalogContext, CatalogError, CatalogPage, CatalogQuery, CatalogSource};
    use coblet_mojang::{
        FnTraitSource, GameVersion, TRAIT_SOURCE_FAILED, TraitError, TraitTable, VersionTrait,
        VersionTraits,
    };
    use coblet_runtime::{
        BoxFuture, Event, Extension, HttpUrl, MemoryBody, Received, Request, Response, Transport,
        TransportError,
    };
    use coblet_types::{
        Arch, ExtensionId, GameVersionId, OsFamily, Platform, Sha1Digest, VersionType,
    };

    use crate::{Coblet, CobletBuilder, Error, WithRoot};

    /// Serves the Mojang fixtures by file name.
    struct Fixtures;

    impl Transport for Fixtures {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let url = request.url().as_str().to_owned();
            Box::pin(async move {
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

    /// Lists one experimental version that declares a trait of its own.
    struct Experiments(ExtensionId);

    impl Extension for Experiments {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Experiments"
        }
    }

    impl CatalogSource<GameVersion> for Experiments {
        fn fetch<'a>(
            &'a self,
            _cx: &'a CatalogContext,
            _query: &'a CatalogQuery,
        ) -> BoxFuture<'a, Result<CatalogPage<GameVersion>, CatalogError>> {
            Box::pin(async {
                let version = GameVersion::new(
                    GameVersionId::new("1.16_combat-6").unwrap(),
                    VersionType::Experiment,
                    HttpUrl::new("https://meta.example/1.16_combat-6.json").unwrap(),
                    Sha1Digest::from_hex("a9993e364706816aba3e25717850c26c9cd0d89d").unwrap(),
                    "2020-08-14T12:00:00Z".parse().unwrap(),
                )
                .with_traits([VersionTrait::from("acme:declared")].into_iter().collect());
                Ok(CatalogPage::new(vec![version]))
            })
        }
    }

    const FIXES: &str = r#"
        id = "acme:fixes"
        name = "Acme fixes"

        [[rule]]
        add = ["acme:offline-skins"]
        released_before = "2014-04-09T07:52:06Z"

        [[rule]]
        remove = ["legacy-macos-rendering"]
        ids = ["b1.7.3"]

        [[rule]]
        add = ["acme:combat"]
        sources = ["acme:experiments"]
    "#;

    /// Adds Java options for versions with `acme:offline-skins`.
    #[cfg(feature = "launch")]
    const OFFLINE_SKINS_PROFILE: &str = r#"
        id = "acme:offline-skins"

        [match]
        version_traits = ["acme:offline-skins"]

        [jvm]
        append = ["-Dacme.skins=offline"]
    "#;

    fn builder(root: &Path) -> CobletBuilder<WithRoot> {
        Coblet::builder()
            .data_root(root)
            .transport(Arc::new(Fixtures))
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
    }

    fn names(traits: &VersionTraits) -> Vec<&str> {
        traits.iter().map(VersionTrait::as_str).collect()
    }

    fn id(text: &str) -> ExtensionId {
        ExtensionId::new(text).unwrap()
    }

    #[tokio::test]
    async fn trait_tables_tag_versions_from_every_source() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(dir.path())
            .version_source(Arc::new(Experiments(id("acme:experiments"))))
            .trait_table_toml(FIXES)
            .build()
            .unwrap();
        let ids: Vec<&str> = coblet
            .trait_sources()
            .iter()
            .map(|source| source.id().as_str())
            .collect();
        assert_eq!(ids, ["coblet:version-traits", "acme:fixes"]);

        let beta = coblet.version("b1.7.3").await.unwrap();
        assert_eq!(beta.source, Some(id("coblet:mojang")));
        assert_eq!(
            names(&beta.traits),
            ["legacy-online-services", "acme:offline-skins"]
        );
        let experiment = coblet.version("1.16_combat-6").await.unwrap();
        assert_eq!(experiment.source, Some(id("acme:experiments")));
        assert_eq!(names(&experiment.traits), ["acme:combat", "acme:declared"]);

        let report = coblet.version_traits(&beta).await.unwrap();
        assert_eq!(
            names(&report.traits),
            ["jre-legacy", "legacy-online-services", "acme:offline-skins"]
        );
        let changes: Vec<String> = report.changes.iter().map(ToString::to_string).collect();
        assert_eq!(
            changes,
            [
                "added jre-legacy by coblet:version-traits",
                "added legacy-online-services by coblet:version-traits",
                "added legacy-macos-rendering by coblet:version-traits",
                "removed legacy-macos-rendering by acme:fixes",
                "added acme:offline-skins by acme:fixes",
            ]
        );
    }

    #[cfg(feature = "launch")]
    #[tokio::test]
    async fn profiles_match_traits_from_added_sources() {
        let dir = tempfile::tempdir().unwrap();
        let profile = || {
            Arc::new(
                coblet_platform::DataProfile::from_toml(
                    OFFLINE_SKINS_PROFILE,
                    coblet_platform::ProfileLayer::ThirdParty,
                )
                .unwrap(),
            )
        };
        let plain = builder(dir.path())
            .platform_profile(profile())
            .build()
            .unwrap();
        let version = plain.version("b1.7.3").await.unwrap();
        let resolved = plain.resolve(&version).await.unwrap();
        assert!(!resolved.profiles.contains(&id("acme:offline-skins")));

        let extended = builder(dir.path())
            .platform_profile(profile())
            .trait_table_toml(FIXES)
            .build()
            .unwrap();
        let version = extended.version("b1.7.3").await.unwrap();
        let resolved = extended.resolve(&version).await.unwrap();
        assert!(
            resolved.profiles.contains(&id("acme:offline-skins")),
            "{}",
            resolved.explain()
        );
        assert!(
            resolved
                .traits
                .contains(&VersionTrait::from("acme:offline-skins"))
        );
        assert!(
            !resolved
                .traits
                .contains(&VersionTrait::LegacyMacosRendering)
        );
    }

    #[tokio::test]
    async fn the_builtin_table_can_be_removed_or_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let without = builder(dir.path())
            .without_trait_source(id("coblet:version-traits"))
            .build()
            .unwrap();
        assert!(without.trait_sources().is_empty());
        let beta = without.version("b1.7.3").await.unwrap();
        assert!(beta.traits.is_empty());
        assert!(
            without
                .version_traits(&beta)
                .await
                .unwrap()
                .traits
                .is_empty()
        );

        let replaced = builder(dir.path())
            .trait_table_toml(FIXES)
            .replace_trait_source(Arc::new(
                TraitTable::from_toml(
                    "id = \"coblet:version-traits\"\n[[rule]]\nadd = [\"acme:everything\"]",
                )
                .unwrap(),
            ))
            .build()
            .unwrap();
        let ids: Vec<&str> = replaced
            .trait_sources()
            .iter()
            .map(|source| source.id().as_str())
            .collect();
        assert_eq!(ids, ["coblet:version-traits", "acme:fixes"]);
        let beta = replaced.version("b1.7.3").await.unwrap();
        assert_eq!(
            names(&beta.traits),
            ["acme:everything", "acme:offline-skins"]
        );
    }

    #[tokio::test]
    async fn failing_sources_are_skipped_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let coblet = builder(dir.path())
            .trait_source(Arc::new(FnTraitSource::new(
                id("acme:broken"),
                "Broken",
                |_| Err(TraitError::failed("no data")),
            )))
            .build()
            .unwrap();
        let mut events = coblet.subscribe();
        let beta = coblet.version("b1.7.3").await.unwrap();
        assert_eq!(names(&beta.traits), ["legacy-online-services"]);
        let warning = loop {
            match events.recv().await {
                Some(Received::Event(Event::Warning(warning)))
                    if warning.code == TRAIT_SOURCE_FAILED =>
                {
                    break warning;
                }
                Some(_) => {}
                None => panic!("no warning"),
            }
        };
        assert_eq!(
            warning.message,
            "trait source `acme:broken` failed: no data"
        );
        let report = coblet.version_traits(&beta).await.unwrap();
        assert!(report.traits.contains(&VersionTrait::JreLegacy));
    }

    #[test]
    fn build_reports_bad_tables_and_duplicate_ids() {
        let error = builder(Path::new("/tmp/x"))
            .trait_table_toml("id = \"acme:bad\"\n[[rule]]\nids = [\"1.0\"]")
            .build()
            .unwrap_err();
        assert!(matches!(error, Error::Traits(_)), "{error}");
        assert!(error.to_string().contains("rule 1"), "{error}");
        let error = builder(Path::new("/tmp/x"))
            .trait_table_json(r#"{"id": "acme:fixes", "rule": [{"add": ["x"]}]}"#)
            .trait_table_toml(FIXES)
            .build()
            .unwrap_err();
        assert!(matches!(error, Error::Registry(_)), "{error}");
    }
}
