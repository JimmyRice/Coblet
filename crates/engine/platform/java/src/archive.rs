//! Reading Java runtime archives into memory, refusing entries that would
//! leave the install directory and archives that unpack to more than
//! [`MAX_UNPACKED`].

use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use bytes::Bytes;

use crate::ArchiveFormat;

/// Most bytes an archive may unpack to; a JDK unpacks to about 300 MiB.
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;

/// Most entries an archive may have; a JDK has a few thousand.
const MAX_ENTRIES: usize = 100_000;

/// One unpacked entry, with a path relative to the install directory.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// A directory.
    Directory(PathBuf),
    /// A file and whether it is executable.
    File {
        path: PathBuf,
        content: Bytes,
        executable: bool,
    },
    /// A symbolic link, with its target as written.
    Symlink { path: PathBuf, target: PathBuf },
    /// A hard link to an earlier entry.
    HardLink { path: PathBuf, target: PathBuf },
}

/// Unpacks an archive.
pub(crate) fn unpack(bytes: Bytes, format: ArchiveFormat) -> Result<Vec<Entry>, String> {
    match format {
        ArchiveFormat::Zip => unpack_zip(bytes),
        ArchiveFormat::TarGz => unpack_tar_gz(bytes),
    }
}

/// Returns `name` as a relative path made of normal components only.
fn relative(name: &str) -> Result<PathBuf, String> {
    let mut path = PathBuf::new();
    for component in Path::new(name).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return Err(format!("entry `{name}` leaves the install directory")),
        }
    }
    if name.contains('\\') || name.contains(':') {
        return Err(format!("entry `{name}` leaves the install directory"));
    }
    Ok(path)
}

/// Checks that a symbolic link at `link` pointing to `target` stays inside.
fn contained_link(link: &Path, target: &Path) -> Result<(), String> {
    let mut depth = link.components().count().saturating_sub(1);
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => {
                return Err(format!(
                    "link `{}` points outside the install directory",
                    link.display()
                ));
            }
        }
    }
    Ok(())
}

/// Counts unpacked bytes against [`MAX_UNPACKED`].
struct Budget(u64);

impl Budget {
    fn take(&mut self, size: u64) -> Result<(), String> {
        self.0 = self.0.saturating_add(size);
        if self.0 > MAX_UNPACKED {
            return Err(format!(
                "archive unpacks to more than {} MiB",
                MAX_UNPACKED / 1024 / 1024
            ));
        }
        Ok(())
    }
}

fn read_limited(reader: impl Read, size: u64, budget: &mut Budget) -> Result<Bytes, String> {
    budget.take(size)?;
    let mut content = Vec::with_capacity(usize::try_from(size).unwrap_or(0).min(64 << 20));
    reader
        .take(size.saturating_add(1))
        .read_to_end(&mut content)
        .map_err(|error| error.to_string())?;
    if content.len() as u64 != size {
        return Err("entry size does not match its header".to_owned());
    }
    Ok(Bytes::from(content))
}

fn unpack_zip(bytes: Bytes) -> Result<Vec<Entry>, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
    if archive.len() > MAX_ENTRIES {
        return Err(format!("archive has more than {MAX_ENTRIES} entries"));
    }
    let mut budget = Budget(0);
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry
            .name()
            .map_err(|error| error.to_string())?
            .into_owned();
        let path = relative(&name)?;
        if path.as_os_str().is_empty() {
            continue;
        }
        let mode = entry.unix_mode().unwrap_or(0);
        if entry.is_dir() {
            entries.push(Entry::Directory(path));
        } else if mode & 0o170_000 == 0o120_000 {
            let size = entry.size();
            let target = read_limited(&mut entry, size, &mut budget)?;
            let target = PathBuf::from(String::from_utf8_lossy(&target).into_owned());
            contained_link(&path, &target)?;
            entries.push(Entry::Symlink { path, target });
        } else {
            let size = entry.size();
            let content = read_limited(&mut entry, size, &mut budget)?;
            entries.push(Entry::File {
                path,
                content,
                executable: mode & 0o111 != 0,
            });
        }
    }
    Ok(entries)
}

fn unpack_tar_gz(bytes: Bytes) -> Result<Vec<Entry>, String> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut budget = Budget(0);
    let mut entries = Vec::new();
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        if entries.len() >= MAX_ENTRIES {
            return Err(format!("archive has more than {MAX_ENTRIES} entries"));
        }
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let path = relative(&name)?;
        if path.as_os_str().is_empty() {
            continue;
        }
        let kind = entry.header().entry_type();
        let link_target = || -> Result<PathBuf, String> {
            Ok(PathBuf::from(
                String::from_utf8_lossy(
                    &entry
                        .link_name_bytes()
                        .ok_or_else(|| format!("link `{name}` has no target"))?,
                )
                .into_owned(),
            ))
        };
        if kind.is_dir() {
            entries.push(Entry::Directory(path));
        } else if kind.is_symlink() {
            let target = link_target()?;
            contained_link(&path, &target)?;
            entries.push(Entry::Symlink { path, target });
        } else if kind.is_hard_link() {
            let target = relative(&link_target()?.to_string_lossy())?;
            entries.push(Entry::HardLink { path, target });
        } else if kind.is_file() {
            let mode = entry.header().mode().unwrap_or(0);
            let size = entry.size();
            let content = read_limited(&mut entry, size, &mut budget)?;
            entries.push(Entry::File {
                path,
                content,
                executable: mode & 0o111 != 0,
            });
        }
        // Other entry types, such as PAX headers and devices, carry no files.
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use bytes::Bytes;

    use super::{Entry, unpack};
    use crate::ArchiveFormat;

    fn tar_gz(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Bytes {
        let mut builder = tar::Builder::new(Vec::new());
        build(&mut builder);
        let tar = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar).unwrap();
        Bytes::from(encoder.finish().unwrap())
    }

    fn file(builder: &mut tar::Builder<Vec<u8>>, path: &str, mode: u32, content: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut header, path, content).unwrap();
    }

    fn link(builder: &mut tar::Builder<Vec<u8>>, path: &str, target: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_entry_type(tar::EntryType::Symlink);
        builder.append_link(&mut header, path, target).unwrap();
    }

    #[test]
    fn unpacks_tar_gz_with_modes_and_links() {
        let archive = tar_gz(|builder| {
            file(builder, "jdk/bin/java", 0o755, b"#!/bin/sh\n");
            file(builder, "jdk/release", 0o644, b"JAVA_VERSION=17");
            link(builder, "jdk/bin/java-link", "java");
        });
        let entries = unpack(archive, ArchiveFormat::TarGz).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(matches!(
            &entries[0],
            Entry::File { path, executable: true, .. } if path == &PathBuf::from("jdk/bin/java")
        ));
        assert!(matches!(
            &entries[1],
            Entry::File {
                executable: false,
                ..
            }
        ));
        assert!(
            matches!(&entries[2], Entry::Symlink { target, .. } if target == &PathBuf::from("java"))
        );
    }

    #[test]
    fn refuses_entries_that_leave_the_directory() {
        let escaping_link = tar_gz(|builder| link(builder, "jdk/lib", "../../etc"));
        let error = unpack(escaping_link, ArchiveFormat::TarGz).unwrap_err();
        assert!(error.contains("outside"), "{error}");

        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("../evil", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        let bytes = Bytes::from(zip.finish().unwrap().into_inner());
        let error = unpack(bytes, ArchiveFormat::Zip).unwrap_err();
        assert!(error.contains("leaves"), "{error}");
    }

    #[test]
    fn unpacks_zip_with_modes() {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        zip.start_file("jdk/bin/java", options).unwrap();
        zip.write_all(b"#!/bin/sh\n").unwrap();
        zip.add_directory("jdk/lib/", zip::write::SimpleFileOptions::default())
            .unwrap();
        let bytes = Bytes::from(zip.finish().unwrap().into_inner());
        let entries = unpack(bytes, ArchiveFormat::Zip).unwrap();
        assert!(matches!(
            &entries[0],
            Entry::File {
                executable: true,
                ..
            }
        ));
        assert!(matches!(&entries[1], Entry::Directory(path) if path == &PathBuf::from("jdk/lib")));
    }
}
