use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use coblet_catalog::{CatalogContext, MetaRequest};
use coblet_download::{Destination, DownloadTask, Downloader, Priority, VerifyMode};
use coblet_mojang::{AssetIndex, Download};
use coblet_platform::{EffectiveLaunchProfile, FileSource, LibraryFile};
use coblet_runtime::{
    ArtifactKind, CancellationToken, EntryKind, Expectation, HttpUrl, RelPath, Sources, Storage,
    TransferProgress,
};
use coblet_types::{FileHash, Sha1Digest};

use crate::{GameLayout, LaunchError};

/// Game files that must be present before launch.
#[derive(Debug, Default)]
pub(crate) struct FileSet {
    pub(crate) tasks: Vec<DownloadTask>,
    /// Libraries without an address; they must already exist.
    pub(crate) existing: Vec<PathBuf>,
}

fn url(text: &str) -> Result<HttpUrl, LaunchError> {
    HttpUrl::new(text).map_err(|error| LaunchError::InvalidMetadata(format!("{text}: {error}")))
}

fn sha1_task(
    (address, sha1, size): (&str, Sha1Digest, u64),
    dest: PathBuf,
    kind: ArtifactKind,
) -> Result<DownloadTask, LaunchError> {
    Ok(DownloadTask::new(
        Sources::single(url(address)?),
        Expectation::hash(FileHash::Sha1(sha1), Some(size)),
        Destination::Path(dest),
        kind,
    ))
}

fn fields(download: &Download) -> (&str, Sha1Digest, u64) {
    (&download.url, download.sha1, download.size)
}

/// Reads the SHA-1 a Maven repository publishes next to a file.
async fn sidecar_sha1(
    catalog: &CatalogContext,
    file_url: &str,
    relative: &str,
) -> Result<Sha1Digest, LaunchError> {
    let key = RelPath::new(format!("sidecar/{relative}.sha1"))
        .map_err(|error| LaunchError::InvalidMetadata(format!("{relative}: {error}")))?;
    let body = catalog
        .fetch(
            // The sidecar sits next to the library, so library mirrors apply.
            &MetaRequest::new(url(&format!("{file_url}.sha1"))?, key)
                .with_kind(coblet_runtime::ArtifactKind::Library),
        )
        .await?;
    let text = String::from_utf8_lossy(&body);
    let hex = text.split_whitespace().next().unwrap_or_default();
    Sha1Digest::from_hex(hex)
        .map_err(|error| LaunchError::InvalidMetadata(format!("{file_url}.sha1: {error}")))
}

impl FileSet {
    /// Lists the client jar, libraries, native jars, asset index and log configuration.
    pub(crate) async fn for_profile(
        profile: &EffectiveLaunchProfile,
        layout: &GameLayout,
        catalog: &CatalogContext,
    ) -> Result<Self, LaunchError> {
        let mut set = Self::default();
        if let Some(client) = &profile.client {
            set.tasks.push(
                sha1_task(
                    fields(client),
                    layout.client_jar(&profile.game_version),
                    ArtifactKind::ClientJar,
                )?
                .with_priority(Priority::High),
            );
        }
        let mut seen = BTreeSet::new();
        let libraries = profile
            .classpath()
            .map(|file| (file, ArtifactKind::Library))
            .chain(
                profile
                    .native_jars()
                    .map(|jar| (&jar.file, ArtifactKind::Natives)),
            );
        for (file, kind) in libraries {
            if seen.insert(file.path.clone()) {
                set.add_library(file, kind, layout, catalog).await?;
            }
        }
        if let Some(index) = &profile.asset_index {
            set.tasks.push(
                sha1_task(
                    (&index.url, index.sha1, index.size),
                    layout.asset_index(&index.id),
                    ArtifactKind::Asset,
                )?
                .with_priority(Priority::High),
            );
        }
        if let Some(logging) = &profile.logging {
            let file = &logging.file;
            set.tasks.push(sha1_task(
                (&file.url, file.sha1, file.size),
                layout.log_config(&file.id),
                ArtifactKind::Other("log-config".to_owned()),
            )?);
        }
        Ok(set)
    }

    async fn add_library(
        &mut self,
        file: &LibraryFile,
        kind: ArtifactKind,
        layout: &GameLayout,
        catalog: &CatalogContext,
    ) -> Result<(), LaunchError> {
        let dest = layout.library(&file.path);
        let (address, expect) = match &file.source {
            FileSource::Download { url, .. } if url.is_empty() => {
                self.existing.push(dest);
                return Ok(());
            }
            FileSource::Download { url, sha1, size } => (
                url.clone(),
                match sha1 {
                    Some(sha1) => Expectation::hash(FileHash::Sha1(*sha1), *size),
                    None => Expectation::Unverified { size: *size },
                },
            ),
            FileSource::Sidecar { url } => {
                let sha1 = sidecar_sha1(catalog, url, &file.path).await?;
                (url.clone(), Expectation::hash(FileHash::Sha1(sha1), None))
            }
            FileSource::Existing => {
                self.existing.push(dest);
                return Ok(());
            }
            _ => {
                return Err(LaunchError::InvalidMetadata(format!(
                    "unknown source for {}",
                    file.path
                )));
            }
        };
        self.tasks.push(DownloadTask::new(
            Sources::single(url(&address)?),
            expect,
            Destination::Path(dest),
            kind,
        ));
        Ok(())
    }
}

/// Downloads missing or damaged files; `Strict` rehashes files that look
/// unchanged. Every file is counted in `progress`.
pub(crate) async fn fetch_all(
    downloader: &Downloader,
    tasks: Vec<DownloadTask>,
    mode: VerifyMode,
    cancel: &CancellationToken,
    progress: &TransferProgress,
) -> Result<(), LaunchError> {
    if mode == VerifyMode::Strict {
        for task in &tasks {
            if let Destination::Path(dest) = &task.dest
                && !downloader
                    .verify(dest, &task.expect, VerifyMode::Strict)
                    .await?
                && downloader.storage().metadata(dest).await?.is_some()
            {
                downloader.storage().remove_file(dest).await?;
            }
        }
    }
    let dests: Vec<PathBuf> = tasks
        .iter()
        .map(|task| match &task.dest {
            Destination::Path(path) => path.clone(),
            _ => PathBuf::new(),
        })
        .collect();
    let results = downloader
        .download_all_with_progress(tasks, cancel, Some(progress))
        .await;
    if cancel.is_cancelled() {
        return Err(LaunchError::Cancelled);
    }
    let mut failures = dests
        .into_iter()
        .zip(results)
        .filter_map(|(path, result)| result.err().map(|error| (path, error)));
    if let Some((path, first)) = failures.next() {
        return Err(LaunchError::Download {
            count: failures.count() + 1,
            path,
            first,
        });
    }
    Ok(())
}

/// Lists the object files of an asset index.
pub(crate) fn asset_tasks(
    index: &AssetIndex,
    layout: &GameLayout,
) -> Result<Vec<DownloadTask>, LaunchError> {
    let mut seen = BTreeSet::new();
    index
        .objects
        .values()
        .filter(|object| seen.insert(object.hash))
        .map(|object| {
            Ok(DownloadTask::new(
                Sources::single(url(&object.url())?),
                Expectation::hash(FileHash::Sha1(object.hash), Some(object.size)),
                Destination::Path(layout.asset_object(&object.path())),
                ArtifactKind::Asset,
            )
            .with_priority(Priority::Low))
        })
        .collect()
}

/// Joins an asset name such as `sound/step/grass1.ogg` under `base`, refusing escapes.
fn named_path(base: &Path, name: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// Returns where an index wants its files by name, if anywhere: the
/// `resources/` directory for the oldest versions, `assets/virtual/<id>/` for
/// virtual indexes.
pub(crate) fn by_name_directory(
    index: &AssetIndex,
    layout: &GameLayout,
    id: &str,
) -> Option<PathBuf> {
    if index.map_to_resources {
        Some(layout.resources())
    } else if index.is_virtual {
        Some(layout.virtual_assets(id))
    } else {
        None
    }
}

/// Copies objects to `base/<name>` for indexes that ask for files by name.
pub(crate) async fn copy_by_name(
    storage: &dyn Storage,
    index: &AssetIndex,
    layout: &GameLayout,
    base: &Path,
) -> Result<(), LaunchError> {
    for (name, object) in &index.objects {
        let Some(target) = named_path(base, name) else {
            tracing::warn!(name, "skipping asset with an unsafe name");
            continue;
        };
        if let Some(existing) = storage.metadata(&target).await?
            && existing.kind == EntryKind::File
            && existing.len == object.size
        {
            continue;
        }
        if let Some(parent) = target.parent() {
            storage.create_dir_all(parent).await?;
        }
        storage
            .copy(&layout.asset_object(&object.path()), &target)
            .await?;
    }
    Ok(())
}

/// Checks that libraries without an address are on disk.
pub(crate) async fn check_existing(
    storage: &dyn Storage,
    paths: &[PathBuf],
) -> Result<(), LaunchError> {
    for path in paths {
        if storage.metadata(path).await?.is_none() {
            return Err(LaunchError::MissingLibrary(path.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::named_path;

    #[test]
    fn refuses_escaping_asset_names() {
        let base = Path::new("/r");
        assert_eq!(
            named_path(base, "sound/a.ogg").unwrap(),
            Path::new("/r/sound/a.ogg")
        );
        assert!(named_path(base, "../x").is_none());
        assert!(named_path(base, "a//b").is_none());
        assert!(named_path(base, "a\\b").is_none());
    }
}
