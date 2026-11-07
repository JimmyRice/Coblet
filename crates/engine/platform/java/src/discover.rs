use std::collections::HashSet;
use std::path::{Path, PathBuf};

use coblet_runtime::{EntryKind, ProcessSpawner, Storage};
use coblet_types::OsFamily;

use crate::installer::{MARKER, Marker, inside};
use crate::{InstallSource, JavaInstall, probe};

/// Places searched for Java installations.
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
    /// Directory holding Mojang runtimes installed by coblet, usually `data_root/runtimes`.
    pub runtimes: Option<PathBuf>,
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
        }
    }

    /// Adds a configured executable.
    #[must_use]
    pub fn with_configured(mut self, executable: impl Into<PathBuf>) -> Self {
        self.configured.push(executable.into());
        self
    }

    /// Sets the directory of installed Mojang runtimes.
    #[must_use]
    pub fn with_runtimes(self, runtimes: impl Into<PathBuf>) -> Self {
        Self {
            runtimes: Some(runtimes.into()),
            ..self
        }
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

/// Lists existing `java` executables in search order, without running them.
pub async fn candidates(
    storage: &dyn Storage,
    os: &OsFamily,
    search: &SearchPaths,
) -> Vec<(PathBuf, InstallSource)> {
    let name = executable_name(os);
    let mut found: Vec<(PathBuf, InstallSource)> = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |found: &mut Vec<_>, path: PathBuf, source: InstallSource| {
        if seen.insert(path.clone()) {
            found.push((path, source));
        }
    };

    for path in &search.configured {
        if is_file(storage, path).await {
            push(&mut found, path.clone(), InstallSource::Configured);
        }
    }
    if let Some(runtimes) = &search.runtimes {
        for group in children(storage, runtimes).await {
            let group_name = group
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            for dir in children(storage, &group).await {
                if let Some((path, source)) =
                    installed_runtime(storage, &dir, os, &group_name).await
                {
                    push(&mut found, path, source);
                }
            }
        }
    }
    if let Some(home) = &search.java_home {
        let path = home.join("bin").join(name);
        if is_file(storage, &path).await {
            push(&mut found, path, InstallSource::JavaHome);
        }
    }
    for dir in &search.path {
        let path = dir.join(name);
        if is_file(storage, &path).await {
            push(&mut found, path, InstallSource::Path);
        }
    }
    if let Some(home) = &search.home {
        for jdk in children(storage, &home.join(".sdkman/candidates/java")).await {
            if jdk.file_name().is_some_and(|name| name == "current") {
                continue;
            }
            let path = jdk.join("bin").join(name);
            if is_file(storage, &path).await {
                push(&mut found, path, InstallSource::Sdkman);
            }
        }
    }
    for (dir, inner) in system_directories(os, search.home.as_deref()) {
        for jdk in children(storage, &dir).await {
            let path = jdk.join(inner).join("bin").join(name);
            if is_file(storage, &path).await {
                push(&mut found, path, InstallSource::SystemDirectory);
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
    search: &SearchPaths,
) -> Vec<JavaInstall> {
    let mut installs: Vec<JavaInstall> = Vec::new();
    for (path, source) in candidates(storage, os, search).await {
        match probe(spawner, &path, source).await {
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
