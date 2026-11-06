use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Extension used when a coordinate has no `@extension` suffix.
const DEFAULT_EXTENSION: &str = "jar";

/// Maven coordinate in Gradle notation: `group:artifact:version[:classifier][@extension]`.
///
/// Library entries in `client.json` and Loader profiles use this notation.
/// Every part is checked so that [`MavenCoordinate::relative_path`] can never
/// escape the repository directory.
///
/// # Examples
///
/// ```
/// use coblet_types::MavenCoordinate;
///
/// let lwjgl: MavenCoordinate = "org.lwjgl:lwjgl:3.3.3:natives-macos-arm64".parse()?;
/// assert_eq!(lwjgl.classifier(), Some("natives-macos-arm64"));
/// assert_eq!(
///     lwjgl.relative_path(),
///     "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-macos-arm64.jar"
/// );
/// # Ok::<(), coblet_types::MavenCoordinateError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MavenCoordinate {
    group: String,
    artifact: String,
    version: String,
    classifier: Option<String>,
    extension: Option<String>,
}

/// Reason a Maven coordinate was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MavenCoordinateError {
    /// The coordinate has fewer than three or more than four `:`-separated parts.
    #[error("maven coordinate `{0}` needs group:artifact:version with an optional classifier")]
    WrongPartCount(String),
    /// A part is empty, is `.` or `..`, or contains a forbidden character.
    #[error("maven coordinate part `{part}` is not allowed: {reason}")]
    InvalidPart {
        /// The rejected part.
        part: String,
        /// Why the part was rejected.
        reason: &'static str,
    },
}

impl MavenCoordinate {
    /// Builds a coordinate from its three required parts.
    ///
    /// # Errors
    ///
    /// Returns [`MavenCoordinateError::InvalidPart`] when a part is empty,
    /// equals `.` or `..`, or contains `/`, `\`, `:`, `@` or whitespace. The
    /// group may contain dots but no empty dot-separated segment.
    pub fn new(
        group: impl Into<String>,
        artifact: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, MavenCoordinateError> {
        let group = group.into();
        check_group(&group)?;
        Ok(Self {
            group,
            artifact: checked(artifact.into())?,
            version: checked(version.into())?,
            classifier: None,
            extension: None,
        })
    }

    /// Returns a copy with the given classifier, such as `natives-linux`.
    ///
    /// # Errors
    ///
    /// Returns [`MavenCoordinateError::InvalidPart`] under the same rules as [`MavenCoordinate::new`].
    pub fn with_classifier(
        self,
        classifier: impl Into<String>,
    ) -> Result<Self, MavenCoordinateError> {
        Ok(Self {
            classifier: Some(checked(classifier.into())?),
            ..self
        })
    }

    /// Returns a copy with the given file extension instead of `jar`.
    ///
    /// # Errors
    ///
    /// Returns [`MavenCoordinateError::InvalidPart`] under the same rules as [`MavenCoordinate::new`].
    pub fn with_extension(
        self,
        extension: impl Into<String>,
    ) -> Result<Self, MavenCoordinateError> {
        Ok(Self {
            extension: Some(checked(extension.into())?),
            ..self
        })
    }

    /// Returns the group, such as `org.lwjgl`.
    #[must_use]
    pub fn group(&self) -> &str {
        &self.group
    }

    /// Returns the artifact, such as `lwjgl`.
    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
    }

    /// Returns the version string, uninterpreted.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the classifier, if any.
    #[must_use]
    pub fn classifier(&self) -> Option<&str> {
        self.classifier.as_deref()
    }

    /// Returns the file extension, `jar` unless the coordinate says otherwise.
    #[must_use]
    pub fn extension(&self) -> &str {
        self.extension.as_deref().unwrap_or(DEFAULT_EXTENSION)
    }

    /// Returns the file name, such as `lwjgl-3.3.3-natives-linux.jar`.
    #[must_use]
    pub fn file_name(&self) -> String {
        match &self.classifier {
            Some(classifier) => format!(
                "{}-{}-{classifier}.{}",
                self.artifact,
                self.version,
                self.extension()
            ),
            None => format!("{}-{}.{}", self.artifact, self.version, self.extension()),
        }
    }

    /// Returns the path inside a Maven repository, using `/` separators.
    ///
    /// The result works both as a URL suffix and as a path below `libraries/`.
    #[must_use]
    pub fn relative_path(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.file_name()
        )
    }
}

fn checked(part: String) -> Result<String, MavenCoordinateError> {
    let reason = if part.is_empty() {
        Some("empty")
    } else if part == "." || part == ".." {
        Some("relative path segment")
    } else if part
        .chars()
        .any(|c| matches!(c, '/' | '\\' | ':' | '@') || c.is_whitespace() || c.is_control())
    {
        Some("contains / \\ : @, whitespace or a control character")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(MavenCoordinateError::InvalidPart { part, reason }),
        None => Ok(part),
    }
}

fn check_group(group: &str) -> Result<(), MavenCoordinateError> {
    for segment in group.split('.') {
        if segment.is_empty() {
            return Err(MavenCoordinateError::InvalidPart {
                part: group.to_owned(),
                reason: "empty group segment",
            });
        }
        checked(segment.to_owned())?;
    }
    Ok(())
}

impl fmt::Display for MavenCoordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)?;
        if let Some(classifier) = &self.classifier {
            write!(f, ":{classifier}")?;
        }
        if let Some(extension) = &self.extension {
            write!(f, "@{extension}")?;
        }
        Ok(())
    }
}

impl FromStr for MavenCoordinate {
    type Err = MavenCoordinateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (body, extension) = match s.split_once('@') {
            Some((body, extension)) => (body, Some(extension)),
            None => (s, None),
        };
        let parts: Vec<&str> = body.split(':').collect();
        let coordinate = match parts.as_slice() {
            [group, artifact, version] => Self::new(*group, *artifact, *version)?,
            [group, artifact, version, classifier] => {
                Self::new(*group, *artifact, *version)?.with_classifier(*classifier)?
            }
            _ => return Err(MavenCoordinateError::WrongPartCount(s.to_owned())),
        };
        match extension {
            Some(extension) => coordinate.with_extension(extension),
            None => Ok(coordinate),
        }
    }
}

impl TryFrom<String> for MavenCoordinate {
    type Error = MavenCoordinateError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<MavenCoordinate> for String {
    fn from(value: MavenCoordinate) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
#[path = "../tests/maven.rs"]
mod tests;
