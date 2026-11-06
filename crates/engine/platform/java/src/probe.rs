use std::path::{Path, PathBuf};

use coblet_runtime::{ProcessSpawner, ProcessSpec};
use coblet_types::{Arch, ArchName};

use crate::{InstallSource, JavaError, JavaInstall, major_version};

/// Most output read from a probed `java` process.
const PROBE_OUTPUT_LIMIT: usize = 256 * 1024;

/// Properties printed by `java -XshowSettings:properties -version`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Properties {
    pub(crate) version: Option<String>,
    pub(crate) home: Option<String>,
    pub(crate) arch: Option<String>,
    pub(crate) vendor: Option<String>,
}

/// Reads the `key = value` lines of the property listing.
pub(crate) fn parse_properties(output: &str) -> Properties {
    let mut properties = Properties::default();
    for line in output.lines() {
        let Some((key, value)) = line.trim().split_once(" = ") else {
            continue;
        };
        let value = Some(value.trim().to_owned());
        match key.trim() {
            "java.version" => properties.version = value,
            "java.home" => properties.home = value,
            "os.arch" => properties.arch = value,
            "java.vendor" => properties.vendor = value,
            _ => {}
        }
    }
    properties
}

/// Runs a `java` executable and reads its version, home and architecture.
///
/// # Errors
///
/// Returns [`JavaError::Probe`] when the process cannot run or prints no version.
pub async fn probe(
    spawner: &dyn ProcessSpawner,
    executable: &Path,
    source: InstallSource,
) -> Result<JavaInstall, JavaError> {
    let failed = |reason: String| JavaError::Probe {
        executable: executable.to_path_buf(),
        reason,
    };
    let spec = ProcessSpec::new(executable)
        .arg("-XshowSettings:properties")
        .arg("-version");
    let process = spawner
        .spawn(spec)
        .map_err(|error| failed(error.to_string()))?;
    let output = process
        .capture(PROBE_OUTPUT_LIMIT)
        .await
        .map_err(|error| failed(error.to_string()))?;
    // The listing goes to stderr; some wrappers print it to stdout.
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let properties = parse_properties(&text);
    let version = properties
        .version
        .ok_or_else(|| failed("no java.version in the output".to_owned()))?;
    let major =
        major_version(&version).ok_or_else(|| failed(format!("unreadable version `{version}`")))?;
    let arch = properties
        .arch
        .as_deref()
        .and_then(|arch| ArchName::new(arch).ok())
        .map_or_else(|| Arch::X86_64, Arch::from);
    Ok(JavaInstall {
        executable: executable.to_path_buf(),
        home: properties.home.map_or_else(
            || {
                executable
                    .parent()
                    .and_then(Path::parent)
                    .map_or_else(PathBuf::new, Path::to_path_buf)
            },
            PathBuf::from,
        ),
        version,
        major,
        arch,
        vendor: properties.vendor,
        source,
    })
}

#[cfg(test)]
#[path = "../tests/probe.rs"]
mod tests;
