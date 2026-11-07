use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use coblet_runtime::{BoxFuture, EntryKind, Extension, ProcessSpawner, RegistryError, Storage};
use coblet_types::{ExtensionId, OsFamily};

use crate::installer::{MARKER, Marker, inside};
use crate::{InstallSource, JavaInstall, probe};

/// Places the built-in [`JavaLocator`]s search.
///
/// [`SearchPaths::from_env`] reads them from the current process; tests and
/// applications can fill them in by hand.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchPaths {
    /// Executables configured by the application or the user, searched first.
    pub configured: Vec<PathBuf>,
    /// Value of `JAVA_HOME`.
    pub java_home: Option<PathBuf>,
    /// Directories of `PATH`.
    pub path: Vec<PathBuf>,
    /// The user's home directory, for SDKMAN! and per-user JDK folders.
    pub home: Option<PathBuf>,
    /// Directory holding the runtimes coblet installs, usually `data_root/runtimes`.
    pub runtimes: Option<PathBuf>,
    /// Folders holding one Java per child, each with the path from the child
    /// to the Java home, such as another launcher's runtime folder.
    pub directories: Vec<(PathBuf, String)>,
}

impl SearchPaths {
    /// Reads `JAVA_HOME`, `PATH` and the home directory of the current process.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            configured: Vec::new(),
            java_home: std::env::var_os("JAVA_HOME").map(PathBuf::from),
            path: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            home: std::env::home_dir(),
            runtimes: None,
            directories: Vec::new(),
        }
    }

    /// Adds a configured executable.
    #[must_use]
    pub fn with_configured(mut self, executable: impl Into<PathBuf>) -> Self {
        self.configured.push(executable.into());
        self
    }

    /// Sets the directory of the runtimes coblet installs.
    #[must_use]
    pub fn with_runtimes(self, runtimes: impl Into<PathBuf>) -> Self {
        Self {
            runtimes: Some(runtimes.into()),
            ..self
        }
    }

    /// Adds a folder holding one Java per child; `home` leads from each
    /// child to its Java home, such as `Contents/Home` on macOS or `""`.
    #[must_use]
    pub fn with_directory(mut self, dir: impl Into<PathBuf>, home: impl Into<String>) -> Self {
        self.directories.push((dir.into(), home.into()));
        self
    }
}

/// Returns the name of the `java` executable on a platform.
#[must_use]
pub fn executable_name(os: &OsFamily) -> &'static str {
    if *os == OsFamily::Windows {
        "java.exe"
    } else {
        "java"
    }
}

/// Directories holding one JDK per child on each platform.
fn system_directories(os: &OsFamily, home: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
    let mut dirs: Vec<(PathBuf, &'static str)> = Vec::new();
    match os {
        OsFamily::MacOs => {
            dirs.push(("/Library/Java/JavaVirtualMachines".into(), "Contents/Home"));
            if let Some(home) = home {
                dirs.push((
                    home.join("Library/Java/JavaVirtualMachines"),
                    "Contents/Home",
                ));
            }
        }
        OsFamily::Windows => {
            for vendor in [
                "Java",
                "Eclipse Adoptium",
                "Zulu",
                "Microsoft",
                "Amazon Corretto",
                "BellSoft",
            ] {
                dirs.push((PathBuf::from(r"C:\Program Files").join(vendor), ""));
            }
        }
        _ => {
            for dir in [
                "/usr/lib/jvm",
                "/usr/lib64/jvm",
                "/usr/java",
                "/opt/java",
                "/usr/local/jdk",
            ] {
                dirs.push((dir.into(), ""));
            }
        }
    }
    dirs
}

async fn is_file(storage: &dyn Storage, path: &Path) -> bool {
    matches!(
        storage.metadata(path).await,
        Ok(Some(metadata)) if matches!(metadata.kind, EntryKind::File | EntryKind::Symlink)
    )
}

async fn children(storage: &dyn Storage, dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = storage
        .read_dir(dir)
        .await
        .map(|entries| {
            entries
                .into_iter()
                .filter(|entry| matches!(entry.kind, EntryKind::Directory | EntryKind::Symlink))
                .map(|entry| entry.path)
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths
}

/// Returns the `java` executable inside a Mojang runtime directory.
pub(crate) async fn runtime_executable(
    storage: &dyn Storage,
    dir: &Path,
    os: &OsFamily,
) -> Option<PathBuf> {
    let name = executable_name(os);
    for relative in ["bin", "jre.bundle/Contents/Home/bin"] {
        let candidate = dir.join(relative).join(name);
        if is_file(storage, &candidate).await {
            return Some(candidate);
        }
    }
    None
}

/// A `java` executable a [`JavaLocator`] found, not yet run.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JavaCandidate {
    /// The executable.
    pub executable: PathBuf,
    /// Where it was found.
    pub source: InstallSource,
}

impl JavaCandidate {
    /// Describes an executable found by a locator.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, source: InstallSource) -> Self {
        Self {
            executable: executable.into(),
            source,
        }
    }
}

/// What a [`JavaLocator`] may use: file access and the operating system.
#[derive(Clone, Copy)]
pub struct LocateContext<'a> {
    /// File access.
    pub storage: &'a dyn Storage,
    /// Operating system of the machine.
    pub os: &'a OsFamily,
}

impl fmt::Debug for LocateContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocateContext")
            .field("os", self.os)
            .finish_non_exhaustive()
    }
}

/// Place to look for installed Java, such as `PATH`, SDKMAN! or another
/// launcher's runtime folder.
///
/// Locators only list executables; the engine runs each one to read its
/// version and architecture and drops duplicates. The built-in search
/// places are locators too, so a launcher can add, replace or remove any
/// of them in [`JavaLocators`].
pub trait JavaLocator: Extension {
    /// Lists `java` executables, best first; missing places are not errors.
    fn locate<'a>(&'a self, cx: &'a LocateContext<'a>) -> BoxFuture<'a, Vec<JavaCandidate>>;
}

/// IDs of the built-in locators, in search order.
pub const BUILTIN_LOCATORS: [&str; 7] = [
    "coblet:configured",
    "coblet:runtimes",
    "coblet:java-home",
    "coblet:path",
    "coblet:sdkman",
    "coblet:system",
    "coblet:directories",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Configured,
    Runtimes,
    JavaHome,
    Path,
    Sdkman,
    System,
    Directories,
}

/// One of the built-in search places, reading its paths from [`SearchPaths`].
struct Builtin {
    id: ExtensionId,
    place: Place,
    search: Arc<SearchPaths>,
}

impl Extension for Builtin {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        match self.place {
            Place::Configured => "Configured Java",
            Place::Runtimes => "Installed runtimes",
            Place::JavaHome => "JAVA_HOME",
            Place::Path => "PATH",
            Place::Sdkman => "SDKMAN!",
            Place::System => "System JDK folders",
            Place::Directories => "Added folders",
        }
    }
}

impl JavaLocator for Builtin {
    fn locate<'a>(&'a self, cx: &'a LocateContext<'a>) -> BoxFuture<'a, Vec<JavaCandidate>> {
        Box::pin(async move {
            let (storage, os) = (cx.storage, cx.os);
            let name = executable_name(os);
            let search = &*self.search;
            let mut found = Vec::new();
            match self.place {
                Place::Configured => {
                    for path in &search.configured {
                        if is_file(storage, path).await {
                            found.push(JavaCandidate::new(path, InstallSource::Configured));
                        }
                    }
                }
                Place::Runtimes => {
                    let Some(runtimes) = &search.runtimes else {
                        return found;
                    };
                    for group in children(storage, runtimes).await {
                        let group_name = group
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        for dir in children(storage, &group).await {
                            if let Some((path, source)) =
                                installed_runtime(storage, &dir, os, &group_name).await
                            {
                                found.push(JavaCandidate::new(path, source));
                            }
                        }
                    }
                }
                Place::JavaHome => {
                    if let Some(home) = &search.java_home {
                        let path = home.join("bin").join(name);
                        if is_file(storage, &path).await {
                            found.push(JavaCandidate::new(path, InstallSource::JavaHome));
                        }
                    }
                }
                Place::Path => {
                    for dir in &search.path {
                        let path = dir.join(name);
                        if is_file(storage, &path).await {
                            found.push(JavaCandidate::new(path, InstallSource::Path));
                        }
                    }
                }
                Place::Sdkman => {
                    let Some(home) = &search.home else {
                        return found;
                    };
                    for jdk in children(storage, &home.join(".sdkman/candidates/java")).await {
                        if jdk.file_name().is_some_and(|name| name == "current") {
                            continue;
                        }
                        let path = jdk.join("bin").join(name);
                        if is_file(storage, &path).await {
                            found.push(JavaCandidate::new(path, InstallSource::Sdkman));
                        }
                    }
                }
                Place::System => {
                    for (dir, inner) in system_directories(os, search.home.as_deref()) {
                        for jdk in children(storage, &dir).await {
                            let path = jdk.join(inner).join("bin").join(name);
                            if is_file(storage, &path).await {
                                found
                                    .push(JavaCandidate::new(path, InstallSource::SystemDirectory));
                            }
                        }
                    }
                }
                Place::Directories => {
                    for (dir, inner) in &search.directories {
                        for jdk in children(storage, dir).await {
                            let path = jdk.join(inner).join("bin").join(name);
                            if is_file(storage, &path).await {
                                let source = InstallSource::Directory(dir.clone());
                                found.push(JavaCandidate::new(path, source));
                            }
                        }
                    }
                }
            }
            found
        })
    }
}

/// The Java locators of a launcher, searched in order.
///
/// [`builtin`](Self::builtin) lists the search places of [`BUILTIN_LOCATORS`];
/// [`register`](Self::register) appends a locator and refuses an ID that is
/// taken, [`replace`](Self::replace) swaps one in place and
/// [`remove`](Self::remove) drops one, for example to not search `PATH`.
#[derive(Clone, Default)]
pub struct JavaLocators {
    entries: Vec<Arc<dyn JavaLocator>>,
}

impl fmt::Debug for JavaLocators {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|locator| locator.id()))
            .finish()
    }
}

impl JavaLocators {
    /// Creates an empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the built-in locators, searching `search`.
    #[must_use]
    pub fn builtin(search: SearchPaths) -> Self {
        let search = Arc::new(search);
        let places = [
            Place::Configured,
            Place::Runtimes,
            Place::JavaHome,
            Place::Path,
            Place::Sdkman,
            Place::System,
            Place::Directories,
        ];
        let entries = BUILTIN_LOCATORS
            .iter()
            .zip(places)
            .map(|(id, place)| {
                let locator: Arc<dyn JavaLocator> = Arc::new(Builtin {
                    id: ExtensionId::new(*id).unwrap_or_else(|_| unreachable!()),
                    place,
                    search: Arc::clone(&search),
                });
                locator
            })
            .collect();
        Self { entries }
    }

    /// Appends a locator.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when another locator has the same ID.
    pub fn register(&mut self, locator: Arc<dyn JavaLocator>) -> Result<(), RegistryError> {
        if self.get(locator.id()).is_some() {
            return Err(RegistryError::Duplicate(locator.id().clone()));
        }
        self.entries.push(locator);
        Ok(())
    }

    /// Puts a locator in place of the one with the same ID, keeping its
    /// position, or appends it; returns the locator it replaced.
    pub fn replace(&mut self, locator: Arc<dyn JavaLocator>) -> Option<Arc<dyn JavaLocator>> {
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|known| known.id() == locator.id())
        {
            return Some(std::mem::replace(slot, locator));
        }
        self.entries.push(locator);
        None
    }

    /// Removes and returns a locator.
    pub fn remove(&mut self, id: &ExtensionId) -> Option<Arc<dyn JavaLocator>> {
        let index = self.entries.iter().position(|known| known.id() == id)?;
        Some(self.entries.remove(index))
    }

    /// Returns a locator by ID.
    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&Arc<dyn JavaLocator>> {
        self.entries.iter().find(|known| known.id() == id)
    }

    /// Iterates over the locators in search order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Arc<dyn JavaLocator>> {
        self.entries.iter()
    }
}

/// Lists existing `java` executables in search order, without running them;
/// an executable found twice keeps its first place.
pub async fn candidates(
    storage: &dyn Storage,
    os: &OsFamily,
    locators: &JavaLocators,
) -> Vec<JavaCandidate> {
    let cx = LocateContext { storage, os };
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for locator in locators.iter() {
        for candidate in locator.locate(&cx).await {
            if seen.insert(candidate.executable.clone()) {
                found.push(candidate);
            }
        }
    }
    found
}

/// Reads one runtime directory under the runtimes root: one with a marker
/// was installed by a Java provider, one without is a Mojang runtime from
/// before providers, named after its component folder.
async fn installed_runtime(
    storage: &dyn Storage,
    dir: &Path,
    os: &OsFamily,
    group: &str,
) -> Option<(PathBuf, InstallSource)> {
    if let Ok(bytes) = storage.read(&dir.join(MARKER)).await {
        let marker: Marker = serde_json::from_slice(&bytes)
            .inspect_err(
                |error| tracing::debug!(%error, dir = %dir.display(), "unreadable java marker"),
            )
            .ok()?;
        let path = inside(dir, &marker.executable).ok()?;
        return is_file(storage, &path).await.then(|| {
            let source = InstallSource::Provider {
                provider: marker.provider,
                package: marker.package,
            };
            (path, source)
        });
    }
    let path = runtime_executable(storage, dir, os).await?;
    let source = InstallSource::MojangRuntime {
        component: group.to_owned(),
    };
    Some((path, source))
}

/// Finds and probes every Java installation, dropping duplicates of the same `java.home`.
///
/// Executables that fail to run are skipped and logged.
pub async fn discover(
    storage: &dyn Storage,
    spawner: &dyn ProcessSpawner,
    os: &OsFamily,
    locators: &JavaLocators,
) -> Vec<JavaInstall> {
    let mut installs: Vec<JavaInstall> = Vec::new();
    for candidate in candidates(storage, os, locators).await {
        match probe(spawner, &candidate.executable, candidate.source).await {
            Ok(install) => {
                let duplicate = installs.iter().any(|known| {
                    known.home == install.home
                        && known.version == install.version
                        && known.arch == install.arch
                });
                if !duplicate {
                    installs.push(install);
                }
            }
            Err(error) => tracing::debug!(%error, "skipping java candidate"),
        }
    }
    installs
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use coblet_runtime::{BoxFuture, Extension, TokioStorage};
    use coblet_types::{ExtensionId, OsFamily};

    use super::{
        BUILTIN_LOCATORS, JavaCandidate, JavaLocator, JavaLocators, LocateContext, SearchPaths,
        candidates,
    };
    use crate::InstallSource;

    struct Fixed(ExtensionId, PathBuf);

    impl Extension for Fixed {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &'static str {
            "Fixed"
        }
    }

    impl JavaLocator for Fixed {
        fn locate<'a>(&'a self, _: &'a LocateContext<'a>) -> BoxFuture<'a, Vec<JavaCandidate>> {
            let source = InstallSource::Located {
                locator: self.0.clone(),
            };
            Box::pin(async move { vec![JavaCandidate::new(&self.1, source)] })
        }
    }

    fn touch(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[tokio::test]
    async fn searches_added_folders_and_third_party_locators_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let other_launcher = dir.path().join("other/runtimes");
        touch(&other_launcher.join("java-17/bin/java"));
        touch(&dir.path().join("path/java"));
        let search = SearchPaths {
            path: vec![dir.path().join("path")],
            ..SearchPaths::default()
        }
        .with_directory(&other_launcher, "");
        let mut locators = JavaLocators::builtin(search);
        let ids: Vec<_> = locators
            .iter()
            .map(|locator| locator.id().as_str())
            .collect();
        assert_eq!(ids, BUILTIN_LOCATORS);

        let custom = ExtensionId::new("acme:registry").unwrap();
        let registry_java = dir.path().join("registry/bin/java");
        locators
            .register(Arc::new(Fixed(custom.clone(), registry_java.clone())))
            .unwrap();
        assert!(
            locators
                .register(Arc::new(Fixed(custom.clone(), registry_java.clone())))
                .is_err()
        );
        locators.remove(&ExtensionId::new("coblet:path").unwrap());

        let storage = TokioStorage::new();
        let found = candidates(&storage, &OsFamily::Linux, &locators).await;
        let sources: Vec<_> = found.into_iter().map(|found| found.source).collect();
        assert_eq!(
            sources,
            [
                InstallSource::Directory(other_launcher),
                InstallSource::Located { locator: custom },
            ]
        );
    }
}
