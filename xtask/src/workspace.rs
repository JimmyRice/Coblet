//! Locating the workspace and walking its source tree.

use std::io;
use std::path::{Path, PathBuf};

/// Directories that never contain workspace sources.
const SKIPPED_DIRS: &[&str] = &["target", "node_modules"];

/// Returns the workspace root, the parent of the xtask manifest directory.
pub(crate) fn root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}

/// Lists every `.rs` file under `root`, skipping build output and hidden directories.
pub(crate) fn rust_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    walk(root, |path| path.extension().is_some_and(|ext| ext == "rs"))
}

/// Lists every `Cargo.toml` under `root`, skipping build output and hidden directories.
pub(crate) fn manifests(root: &Path) -> io::Result<Vec<PathBuf>> {
    walk(root, |path| {
        path.file_name().is_some_and(|name| name == "Cargo.toml")
    })
}

/// Lists the files under `root` that `keep` accepts, sorted.
fn walk(root: &Path, keep: impl Fn(&Path) -> bool) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref()) {
                    pending.push(path);
                }
            } else if file_type.is_file() && keep(&path) {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Strips `root` from `path` for display, falling back to the full path.
pub(crate) fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}
