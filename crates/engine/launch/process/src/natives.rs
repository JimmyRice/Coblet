use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use coblet_platform::NativeJar;
use coblet_runtime::Storage;

use crate::{GameLayout, LaunchError};

/// Extracts the native jars into `dir`, replacing what an earlier launch left there.
pub(crate) async fn extract_natives<'a>(
    storage: &dyn Storage,
    layout: &GameLayout,
    jars: impl Iterator<Item = &'a NativeJar>,
    dir: &Path,
) -> Result<(), LaunchError> {
    if storage.metadata(dir).await?.is_some() {
        storage.remove_dir_all(dir).await?;
    }
    storage.create_dir_all(dir).await?;
    let mut seen = std::collections::BTreeSet::new();
    for jar in jars.filter(|jar| seen.insert(jar.file.path.as_str())) {
        let path = layout.library(&jar.file.path);
        let bytes = storage.read(&path).await?;
        let files = unpack(bytes, &jar.exclude).map_err(|message| LaunchError::Natives {
            jar: path.clone(),
            message,
        })?;
        for (relative, content) in files {
            let target = dir.join(&relative);
            if let Some(parent) = target.parent() {
                storage.create_dir_all(parent).await?;
            }
            storage.write_atomic(&target, content).await?;
        }
    }
    Ok(())
}

/// Reads every file of a jar except excluded prefixes and unsafe names.
fn unpack(bytes: Bytes, exclude: &[String]) -> Result<Vec<(PathBuf, Bytes)>, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|error| error.to_string())?
            .into_owned();
        if exclude
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
        {
            continue;
        }
        let Some(relative) = entry.enclosed_name() else {
            tracing::warn!(name, "skipping native file with an unsafe name");
            continue;
        };
        let mut content = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry
            .read_to_end(&mut content)
            .map_err(|error| format!("{name}: {error}"))?;
        files.push((relative, Bytes::from(content)));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use bytes::Bytes;
    use zip::write::SimpleFileOptions;

    use super::unpack;

    pub(crate) fn jar(entries: &[(&str, &[u8])]) -> Bytes {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            if name.ends_with('/') {
                writer
                    .add_directory(*name, SimpleFileOptions::default())
                    .unwrap();
            } else {
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(content).unwrap();
            }
        }
        Bytes::from(writer.finish().unwrap().into_inner())
    }

    #[test]
    fn skips_excluded_directories_and_unsafe_names() {
        let bytes = jar(&[
            ("META-INF/", b""),
            ("META-INF/MANIFEST.MF", b"x"),
            ("liblwjgl.dylib", b"native"),
            ("../evil.so", b"bad"),
            ("sub/libopenal.dylib", b"al"),
        ]);
        let files = unpack(bytes, &["META-INF/".to_owned()]).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|(path, _)| path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(names, ["liblwjgl.dylib", "sub/libopenal.dylib"]);
        assert_eq!(&files[0].1[..], b"native");
    }
}
