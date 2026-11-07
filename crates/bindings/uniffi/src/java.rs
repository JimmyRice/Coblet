use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use coblet::catalog::CatalogContext;
use coblet::java::{
    ArchiveFormat, InstallSource, JavaCandidate, JavaDownload, JavaError, JavaInstall, JavaPackage,
    JavaQuery, LocateContext, ProviderContext,
};
use coblet::runtime::{BoxFuture, CancellationToken, Extension, HttpUrl, OperationId};
use coblet::types::{ExtensionId, FileHash};

use crate::{Coblet, CobletError, ErrorKind};

fn invalid(message: impl Into<String>) -> CobletError {
    CobletError::new(ErrorKind::InvalidArgument, message)
}

pub(crate) fn extension_id(what: &str, id: &str) -> Result<ExtensionId, CobletError> {
    ExtensionId::new(id).map_err(|error| invalid(format!("{what} id: {error}")))
}

/// Java provider or locator known to a [`Coblet`](crate::Coblet).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JavaExtensionInfo {
    /// ID, such as `coblet:adoptium`.
    pub id: String,
    /// Name shown to users.
    pub name: String,
}

fn info(extension: &dyn Extension) -> JavaExtensionInfo {
    JavaExtensionInfo {
        id: extension.id().as_str().to_owned(),
        name: extension.display_name().to_owned(),
    }
}

/// Installed Java whose version was probed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JavaInstallInfo {
    /// The `java` executable.
    pub executable: String,
    /// `java.home` as reported by the runtime.
    pub home: String,
    /// `java.version`, such as `1.8.0_74` or `17.0.21`.
    pub version: String,
    /// Major version, such as 8 or 17.
    pub major: u32,
    /// Architecture the runtime was built for.
    pub arch: String,
    /// `java.vendor`, when reported.
    pub vendor: Option<String>,
    /// Where it was found: `java-home`, `path`, `sdkman`, `system`,
    /// `configured`, `directory`, `mojang-runtime`, `locator`, `provider`
    /// or `other`.
    pub source: String,
    /// The folder, runtime component, locator or provider behind `source`, when there is one.
    pub source_id: Option<String>,
    /// Package ID, for a runtime a Java provider installed.
    pub package: Option<String>,
}

impl From<&JavaInstall> for JavaInstallInfo {
    fn from(install: &JavaInstall) -> Self {
        let (source, source_id, package) = match &install.source {
            InstallSource::JavaHome => ("java-home", None, None),
            InstallSource::Path => ("path", None, None),
            InstallSource::Sdkman => ("sdkman", None, None),
            InstallSource::SystemDirectory => ("system", None, None),
            InstallSource::Configured => ("configured", None, None),
            InstallSource::Directory(dir) => ("directory", Some(path(dir)), None),
            InstallSource::MojangRuntime { component } => {
                ("mojang-runtime", Some(component.clone()), None)
            }
            InstallSource::Located { locator } => ("locator", Some(locator.to_string()), None),
            InstallSource::Provider { provider, package } => (
                "provider",
                Some(provider.to_string()),
                Some(package.clone()),
            ),
            _ => ("other", None, None),
        };
        Self {
            executable: path(&install.executable),
            home: path(&install.home),
            version: install.version.clone(),
            major: install.major,
            arch: install.arch.as_str().to_owned(),
            vendor: install.vendor.clone(),
            source: source.to_owned(),
            source_id,
            package,
        }
    }
}

fn path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A Java runtime a provider can install.
///
/// Every package carries a hash: the engine installs nothing it cannot check.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JavaPackageInfo {
    /// Identifier, unique within the provider and stable across runs; a
    /// plain file name that names the install directory.
    pub id: String,
    /// Full version, such as `17.0.12+7`.
    pub version: String,
    /// Java major version.
    pub major: u32,
    /// Architecture the runtime is built for, such as `aarch64`.
    pub arch: String,
    /// Address of the archive or of the Mojang-style file list.
    pub url: String,
    /// Hash of what `url` points at, such as `sha256:9f86…`.
    pub hash: String,
    /// `zip`, `tar.gz`, or `manifest` for a Mojang-style file list.
    pub format: String,
    /// Download size in bytes, when known.
    #[uniffi(default = None)]
    pub size: Option<u64>,
    /// Java home inside the archive, such as `jdk-17.0.12+7-jre`; found by
    /// searching when absent.
    #[uniffi(default = None)]
    pub home: Option<String>,
}

impl JavaPackageInfo {
    /// Describes a package; `None` for a download form this binding does not know yet.
    fn from_package(package: &JavaPackage) -> Option<Self> {
        let (url, hash, format, size, home) = match &package.download {
            JavaDownload::Archive {
                url,
                hash,
                size,
                format,
                home,
            } => {
                let format = match format {
                    ArchiveFormat::Zip => "zip",
                    ArchiveFormat::TarGz => "tar.gz",
                    _ => return None,
                };
                (url, hash, format, *size, home.clone())
            }
            JavaDownload::Manifest { url, hash, size } => (url, hash, "manifest", *size, None),
            _ => return None,
        };
        Some(Self {
            id: package.id.clone(),
            version: package.version.clone(),
            major: package.major,
            arch: package.arch.as_str().to_owned(),
            url: url.as_str().to_owned(),
            hash: hash.to_string(),
            format: format.to_owned(),
            size,
            home,
        })
    }

    fn into_package(self) -> Result<JavaPackage, String> {
        let arch = self
            .arch
            .parse()
            .map_err(|error| format!("arch: {error}"))?;
        let url = HttpUrl::new(&self.url).map_err(|error| format!("url: {error}"))?;
        let hash: FileHash = self
            .hash
            .parse()
            .map_err(|error| format!("hash: {error}"))?;
        let mut package = match self.format.as_str() {
            "manifest" => JavaPackage::manifest(self.id, self.version, self.major, arch, url, hash),
            "zip" | "tar.gz" => {
                let format = if self.format == "zip" {
                    ArchiveFormat::Zip
                } else {
                    ArchiveFormat::TarGz
                };
                JavaPackage::archive(self.id, self.version, self.major, arch, url, hash, format)
            }
            other => return Err(format!("unknown format `{other}`")),
        };
        if let Some(size) = self.size {
            package = package.with_size(size);
        }
        if let Some(home) = self.home {
            package = package.with_home(home);
        }
        Ok(package)
    }
}

/// The Java runtime a launch or a user asks a [`JavaProvider`] for.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JavaQueryInfo {
    /// Operating system, such as `macos`.
    pub os: String,
    /// Architecture, such as `aarch64`.
    pub arch: String,
    /// Java major version, such as 8 or 21.
    pub major: u32,
    /// Mojang runtime component named by the version, such as `jre-legacy`.
    #[uniffi(default = None)]
    pub component: Option<String>,
    /// Mojang runtime platform key chosen by a Platform Profile, such as `mac-os`.
    #[uniffi(default = None)]
    pub runtime_platform: Option<String>,
}

impl From<&JavaQuery> for JavaQueryInfo {
    fn from(query: &JavaQuery) -> Self {
        Self {
            os: query.os.as_str().to_owned(),
            arch: query.arch.as_str().to_owned(),
            major: query.major,
            component: query.component.clone(),
            runtime_platform: query.runtime_platform.clone(),
        }
    }
}

/// What one provider answered for a query.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JavaProviderAnswer {
    /// Provider ID.
    pub provider: String,
    /// Packages offered; empty when the provider failed.
    pub packages: Vec<JavaPackageInfo>,
    /// Why the provider failed, when it did.
    pub error: Option<String>,
}

/// Requests a Java provider written in Swift, Kotlin or Python makes.
///
/// Documents come through coblet's metadata cache, so mirror rules,
/// fallback to the original address and offline caching apply as they do
/// to built-in providers. Each provider's documents are cached apart.
#[derive(uniffi::Object)]
pub struct JavaProviderContext {
    catalog: CatalogContext,
    provider: ExtensionId,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for JavaProviderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JavaProviderContext")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl JavaProviderContext {
    /// Returns the ID of the provider the requests are made for.
    #[must_use]
    pub fn provider_id(&self) -> String {
        self.provider.as_str().to_owned()
    }

    /// Fetches a document and caches it under `cache_name`, a relative path
    /// such as `17/macos-aarch64.json`.
    ///
    /// The call blocks until the document is there; call it from the
    /// thread `packages` runs on.
    ///
    /// # Errors
    ///
    /// Fails for a malformed address or cache name and when the document is
    /// unavailable and not cached.
    pub fn fetch(self: Arc<Self>, url: String, cache_name: String) -> Result<Vec<u8>, CobletError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = Arc::clone(&self);
        self.runtime.spawn(async move {
            let cx = ProviderContext::new(&context.catalog, &context.provider);
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

/// Java provider written in Swift, Kotlin or Python, registered with
/// `CobletBuilder.add_java_provider` like a built-in one.
///
/// `packages` runs on a worker thread where blocking is fine; it fetches
/// documents with the [`JavaProviderContext`] it is given and only
/// describes runtimes: the engine downloads, checks and unpacks them.
#[uniffi::export(foreign)]
pub trait JavaProvider: Send + Sync {
    /// Namespaced ID, such as `acme:jdk`.
    fn id(&self) -> String;

    /// Name shown to users.
    fn display_name(&self) -> String;

    /// Lists the packages that fit a query; an empty list when none do.
    ///
    /// # Errors
    ///
    /// Return an error when the packages cannot be listed.
    fn packages(
        &self,
        cx: Arc<JavaProviderContext>,
        query: JavaQueryInfo,
    ) -> Result<Vec<JavaPackageInfo>, CobletError>;
}

/// Adapts a [`JavaProvider`] from foreign code to the engine's trait.
pub(crate) struct ForeignJavaProvider {
    id: ExtensionId,
    name: String,
    provider: Arc<dyn JavaProvider>,
}

impl ForeignJavaProvider {
    pub(crate) fn new(provider: Arc<dyn JavaProvider>) -> Result<Self, CobletError> {
        Ok(Self {
            id: extension_id("java provider", &provider.id())?,
            name: provider.display_name(),
            provider,
        })
    }
}

impl Extension for ForeignJavaProvider {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl coblet::java::JavaProvider for ForeignJavaProvider {
    fn packages<'a>(
        &'a self,
        cx: &'a ProviderContext<'a>,
        query: &'a JavaQuery,
    ) -> BoxFuture<'a, Result<Vec<JavaPackage>, JavaError>> {
        Box::pin(async move {
            let catalog = cx.catalog();
            let context = Arc::new(JavaProviderContext {
                catalog: CatalogContext::new(catalog.meta().clone(), catalog.policy()),
                provider: self.id.clone(),
                runtime: tokio::runtime::Handle::current(),
            });
            let provider = Arc::clone(&self.provider);
            let given = Arc::clone(&context);
            let query = JavaQueryInfo::from(query);
            let result = tokio::task::spawn_blocking(move || provider.packages(given, query)).await;
            for warning in context.catalog.take_warnings() {
                catalog.warn(warning);
            }
            let listed = match result {
                Ok(Ok(listed)) => listed,
                Ok(Err(error)) => return Err(JavaError::provider(&self.id, error.to_string())),
                Err(error) => {
                    return Err(JavaError::provider(
                        &self.id,
                        format!("the provider panicked: {error}"),
                    ));
                }
            };
            listed
                .into_iter()
                .map(|package| {
                    let id = package.id.clone();
                    package.into_package().map_err(|message| {
                        JavaError::provider(&self.id, format!("package {id}: {message}"))
                    })
                })
                .collect()
        })
    }
}

/// Place to look for installed Java, written in Swift, Kotlin or Python,
/// registered with `CobletBuilder.add_java_locator`.
///
/// `locate` runs on a worker thread where blocking is fine. It only lists
/// executables; the engine runs each one to read its version.
#[uniffi::export(foreign)]
pub trait JavaLocator: Send + Sync {
    /// Namespaced ID, such as `acme:other-launcher`.
    fn id(&self) -> String;

    /// Name shown to users.
    fn display_name(&self) -> String;

    /// Lists `java` executables, best first, for an operating system such
    /// as `macos`; missing places are not errors.
    fn locate(&self, os: String) -> Vec<String>;
}

/// Adapts a [`JavaLocator`] from foreign code to the engine's trait.
pub(crate) struct ForeignJavaLocator {
    id: ExtensionId,
    name: String,
    locator: Arc<dyn JavaLocator>,
}

impl ForeignJavaLocator {
    pub(crate) fn new(locator: Arc<dyn JavaLocator>) -> Result<Self, CobletError> {
        Ok(Self {
            id: extension_id("java locator", &locator.id())?,
            name: locator.display_name(),
            locator,
        })
    }
}

impl Extension for ForeignJavaLocator {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl coblet::java::JavaLocator for ForeignJavaLocator {
    fn locate<'a>(&'a self, cx: &'a LocateContext<'a>) -> BoxFuture<'a, Vec<JavaCandidate>> {
        Box::pin(async move {
            let locator = Arc::clone(&self.locator);
            let os = cx.os.as_str().to_owned();
            match tokio::task::spawn_blocking(move || locator.locate(os)).await {
                Ok(paths) => paths
                    .into_iter()
                    .map(|executable| {
                        let source = InstallSource::Located {
                            locator: self.id.clone(),
                        };
                        JavaCandidate::new(PathBuf::from(executable), source)
                    })
                    .collect(),
                Err(error) => {
                    tracing::warn!(locator = %self.id, %error, "java locator panicked");
                    Vec::new()
                }
            }
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Coblet {
    /// Lists the Java providers, asked in order when no installed Java fits.
    #[must_use]
    pub fn java_providers(&self) -> Vec<JavaExtensionInfo> {
        self.inner
            .java_providers()
            .iter()
            .map(|provider| info(provider.as_ref()))
            .collect()
    }

    /// Lists the places searched for installed Java, in order.
    #[must_use]
    pub fn java_locators(&self) -> Vec<JavaExtensionInfo> {
        self.inner
            .java_locators()
            .iter()
            .map(|locator| info(locator.as_ref()))
            .collect()
    }

    /// Finds and probes every installed Java the locators know of.
    pub async fn java_installs(&self) -> Vec<JavaInstallInfo> {
        self.inner
            .java_installs()
            .await
            .iter()
            .map(JavaInstallInfo::from)
            .collect()
    }

    /// Asks every Java provider, in order, for packages of a Java major
    /// version for this platform.
    pub async fn java_packages(&self, major: u32) -> Vec<JavaProviderAnswer> {
        let query = self.inner.java_query(major);
        self.inner
            .java_packages(&query)
            .await
            .into_iter()
            .map(|(provider, answer)| {
                let (packages, error) = match answer {
                    Ok(packages) => (
                        packages
                            .iter()
                            .filter_map(JavaPackageInfo::from_package)
                            .collect(),
                        None,
                    ),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                JavaProviderAnswer {
                    provider: provider.as_str().to_owned(),
                    packages,
                    error,
                }
            })
            .collect()
    }

    /// Installs a package of a Java provider, as listed by `java_packages`,
    /// and probes the result. Progress arrives as Java runtime transfers of
    /// `operation_id`, from `generate_operation_id`; random when absent.
    ///
    /// # Errors
    ///
    /// Fails for a malformed provider ID or package, and when the download,
    /// unpacking or probe fails.
    pub async fn install_java(
        &self,
        provider: String,
        package: JavaPackageInfo,
        operation_id: Option<String>,
    ) -> Result<JavaInstallInfo, CobletError> {
        let provider = extension_id("java provider", &provider)?;
        let package = package
            .into_package()
            .map_err(|message| invalid(format!("java package: {message}")))?;
        let op = match operation_id {
            Some(id) => crate::events::operation_id(&id)?,
            None => OperationId::random(),
        };
        let install = self
            .inner
            .install_java(&provider, &package, op, &CancellationToken::new())
            .await?;
        Ok(JavaInstallInfo::from(&install))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{JavaLocator, JavaPackageInfo, JavaProvider, JavaProviderContext, JavaQueryInfo};
    use crate::{CobletBuilder, CobletError, ErrorKind, PlatformInfo};

    const HASH: &str = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    struct Acme;

    impl JavaProvider for Acme {
        fn id(&self) -> String {
            "acme:jdk".to_owned()
        }

        fn display_name(&self) -> String {
            "Acme JDK".to_owned()
        }

        fn packages(
            &self,
            cx: Arc<JavaProviderContext>,
            query: JavaQueryInfo,
        ) -> Result<Vec<JavaPackageInfo>, CobletError> {
            assert_eq!(cx.provider_id(), "acme:jdk");
            Ok(vec![JavaPackageInfo {
                id: format!("acme-{}-{}-{}", query.major, query.os, query.arch),
                version: format!("{}.0.1", query.major),
                major: query.major,
                arch: query.arch,
                url: "https://jdk.acme.test/jdk.zip".to_owned(),
                hash: HASH.to_owned(),
                format: "zip".to_owned(),
                size: Some(42),
                home: None,
            }])
        }
    }

    struct Elsewhere;

    impl JavaLocator for Elsewhere {
        fn id(&self) -> String {
            "acme:elsewhere".to_owned()
        }

        fn display_name(&self) -> String {
            "Elsewhere".to_owned()
        }

        fn locate(&self, _os: String) -> Vec<String> {
            vec!["/nowhere/bin/java".to_owned()]
        }
    }

    fn builder(root: &std::path::Path) -> Arc<CobletBuilder> {
        let builder = CobletBuilder::new(
            root.to_string_lossy().into_owned(),
            "coblet-uniffi-test/1.0".to_owned(),
        )
        .unwrap();
        builder
            .set_platform(PlatformInfo {
                os: "macos".to_owned(),
                arch: "aarch64".to_owned(),
                os_version: None,
                libc: None,
                traits: Vec::new(),
            })
            .unwrap();
        // Keep the tests offline: no built-in provider is asked.
        for id in ["coblet:mojang", "coblet:adoptium", "coblet:zulu"] {
            builder.remove_java_provider(id).unwrap();
        }
        builder
    }

    fn kind(error: &CobletError) -> ErrorKind {
        let CobletError::Failed { kind, .. } = error;
        *kind
    }

    #[tokio::test]
    async fn foreign_providers_and_locators_take_part() {
        let root = tempfile::tempdir().unwrap();
        let builder = builder(root.path());
        builder.add_java_provider(Arc::new(Acme)).unwrap();
        builder.add_java_locator(Arc::new(Elsewhere)).unwrap();
        builder.remove_java_locator("coblet:path").unwrap();
        let coblet = builder.build().unwrap();

        let providers: Vec<_> = coblet.java_providers().into_iter().map(|p| p.id).collect();
        assert_eq!(providers, ["acme:jdk"]);
        let locators: Vec<_> = coblet.java_locators().into_iter().map(|l| l.id).collect();
        assert!(locators.contains(&"acme:elsewhere".to_owned()));
        assert!(!locators.contains(&"coblet:path".to_owned()));

        let answers = coblet.java_packages(17).await;
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].error, None);
        let package = &answers[0].packages[0];
        assert_eq!(package.id, "acme-17-macos-aarch64");
        assert_eq!((package.format.as_str(), package.size), ("zip", Some(42)));
        assert_eq!(package.hash, HASH);
    }

    #[tokio::test]
    async fn malformed_definitions_and_packages_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let builder = builder(root.path());
        let error = builder.add_java_provider_toml("id = 3").unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
        assert!(builder.remove_java_provider("not an id").is_err());

        let coblet = builder.build().unwrap();
        let package = JavaPackageInfo {
            id: "acme".to_owned(),
            version: "17".to_owned(),
            major: 17,
            arch: "aarch64".to_owned(),
            url: "https://jdk.acme.test/jdk.rar".to_owned(),
            hash: HASH.to_owned(),
            format: "rar".to_owned(),
            size: None,
            home: None,
        };
        let error = coblet
            .install_java("acme:jdk".to_owned(), package, None)
            .await
            .unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    }

    #[test]
    fn duplicate_providers_fail_the_build() {
        let root = tempfile::tempdir().unwrap();
        let builder = builder(root.path());
        builder.add_java_provider(Arc::new(Acme)).unwrap();
        builder.add_java_provider(Arc::new(Acme)).unwrap();
        let error = builder.build().unwrap_err();
        assert_eq!(kind(&error), ErrorKind::InvalidArgument);
    }
}
