use coblet_java::{
    InstallSource, JavaError, JavaInstall, JavaInstaller, JavaLocators, JavaPackage, JavaProviders,
    JavaQuery, discover, probe,
};
use coblet_runtime::{CancellationToken, OperationId, TransferKind, TransferProgress};
use coblet_types::ExtensionId;

use crate::{Coblet, Error};

impl Coblet {
    /// Returns the Java providers, asked in order when no installed Java
    /// fits: the built-in ones and those added with
    /// [`CobletBuilder::java_provider`](crate::CobletBuilder::java_provider).
    #[must_use]
    pub fn java_providers(&self) -> &JavaProviders {
        self.inner.java.providers()
    }

    /// Returns the places searched for installed Java, in order.
    #[must_use]
    pub fn java_locators(&self) -> &JavaLocators {
        &self.inner.java_locators
    }

    /// Returns the installer that puts runtimes under `data_root/runtimes`.
    #[must_use]
    pub fn java_installer(&self) -> &JavaInstaller {
        &self.inner.java
    }

    /// Finds and probes every installed Java the locators know of.
    pub async fn java_installs(&self) -> Vec<JavaInstall> {
        let inner = &self.inner;
        discover(
            &*inner.storage,
            &*inner.spawner,
            inner.platform.os(),
            &inner.java_locators,
        )
        .await
    }

    /// Returns a query for a Java major version on this platform.
    #[must_use]
    pub fn java_query(&self, major: u32) -> JavaQuery {
        let platform = &self.inner.platform;
        JavaQuery::new(platform.os().clone(), platform.arch().clone(), major)
    }

    /// Asks every Java provider, in order, for packages matching a query;
    /// a failing provider is reported in place of its packages.
    pub async fn java_packages(
        &self,
        query: &JavaQuery,
    ) -> Vec<(ExtensionId, Result<Vec<JavaPackage>, JavaError>)> {
        let cx = self.catalog_context();
        let answers = self.inner.java.packages(&cx, query).await;
        self.forward_warnings(&cx);
        answers
    }

    /// Installs a package of a Java provider and probes the result.
    ///
    /// Download progress is published as
    /// [`TransferKind::JavaRuntime`] events of `op`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Java`] when the download, unpacking or probe fails.
    pub async fn install_java(
        &self,
        provider: &ExtensionId,
        package: &JavaPackage,
        op: OperationId,
        cancel: &CancellationToken,
    ) -> Result<JavaInstall, Error> {
        let inner = &self.inner;
        let cx = self.catalog_context();
        let progress = TransferProgress::new();
        let install = inner.java.install(
            &cx,
            provider,
            package,
            inner.platform.os(),
            cancel,
            Some(&progress),
        );
        let executable = progress
            .track(Some(&inner.events), op, TransferKind::JavaRuntime, install)
            .await;
        self.forward_warnings(&cx);
        let source = InstallSource::Provider {
            provider: provider.clone(),
            package: package.id.clone(),
        };
        Ok(probe(&*inner.spawner, &executable?, source).await?)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_runtime::{
        BoxFuture, MemoryBody, Request, Response, Transport, TransportError, UserAgent,
    };
    use coblet_types::{Arch, OsFamily, Platform};

    use crate::{Coblet, CobletBuilder, Error, WithRoot};

    /// Answers every request with 404.
    struct Offline;

    impl Transport for Offline {
        fn send(&self, _request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(async { Ok(Response::new(404, Vec::new(), MemoryBody::new(Vec::new()))) })
        }
    }

    const ACME: &str = r#"
        id = "acme:java"
        name = "Acme Java"
        [packages]
        format = "static"
        [[packages.list]]
        id = "acme-17-macos-aarch64"
        version = "17.0.12"
        major = 17
        os = "macos"
        arch = "aarch64"
        url = "https://java.acme.test/17.tar.gz"
        hash = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    "#;

    fn builder(root: &std::path::Path) -> CobletBuilder<WithRoot> {
        Coblet::builder()
            .data_root(root)
            .user_agent(UserAgent::new("coblet-tests/0").unwrap())
            .transport(Arc::new(Offline))
            .platform(Platform::new(OsFamily::MacOs, Arch::Aarch64))
    }

    fn ids<'a>(list: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
        list.collect()
    }

    #[tokio::test]
    async fn providers_can_be_removed_and_added_as_data() {
        let root = tempfile::tempdir().unwrap();
        let coblet = builder(root.path())
            .without_java_provider("coblet:mojang".parse().unwrap())
            .java_provider_toml(ACME)
            .build()
            .unwrap();
        let providers = coblet.java_providers();
        assert_eq!(
            ids(providers.iter().map(|provider| provider.id().as_str())),
            ["coblet:adoptium", "coblet:zulu", "acme:java"]
        );

        let answers = coblet.java_packages(&coblet.java_query(17)).await;
        assert_eq!(answers.len(), 3);
        let (id, packages) = answers.last().unwrap();
        assert_eq!(id.as_str(), "acme:java");
        let packages = packages.as_ref().unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].id, "acme-17-macos-aarch64");
        // The offline transport makes the API providers fail without
        // hiding the others.
        assert!(answers[0].1.is_err());
    }

    #[test]
    fn duplicate_and_malformed_providers_fail_the_build() {
        let root = tempfile::tempdir().unwrap();
        let duplicate = builder(root.path())
            .java_provider_toml(ACME)
            .java_provider_toml(ACME)
            .build();
        assert!(matches!(duplicate, Err(Error::Registry(_))));
        let malformed = builder(root.path()).java_provider_toml("id = 3").build();
        assert!(matches!(malformed, Err(Error::Java(_))));
    }

    #[test]
    fn locators_can_be_removed() {
        let root = tempfile::tempdir().unwrap();
        let coblet = builder(root.path())
            .without_java_locator("coblet:path".parse().unwrap())
            .java_directory(root.path().join("other-launcher"), "")
            .build()
            .unwrap();
        let locators = ids(coblet
            .java_locators()
            .iter()
            .map(|locator| locator.id().as_str()));
        assert!(!locators.contains(&"coblet:path"));
        assert!(locators.contains(&"coblet:directories"));
        assert_eq!(
            coblet.java_installer().root(),
            coblet.data_root().join("runtimes")
        );
    }
}
