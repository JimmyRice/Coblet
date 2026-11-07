use coblet_types::MavenCoordinate;

/// Returns the address of a Maven artifact in a repository.
///
/// # Examples
///
/// ```
/// use coblet_loader::kit::artifact_url;
///
/// let coordinate = "net.neoforged:neoforge:21.1.77:installer".parse()?;
/// assert_eq!(
///     artifact_url("https://maven.neoforged.net/releases/", &coordinate),
///     "https://maven.neoforged.net/releases/net/neoforged/neoforge/21.1.77/neoforge-21.1.77-installer.jar"
/// );
/// # Ok::<(), coblet_types::MavenCoordinateError>(())
/// ```
#[must_use]
pub fn artifact_url(repository: &str, coordinate: &MavenCoordinate) -> String {
    format!(
        "{}/{}",
        repository.trim_end_matches('/'),
        coordinate.relative_path()
    )
}

/// Returns the address of the `maven-metadata.xml` of an artifact.
///
/// # Examples
///
/// ```
/// use coblet_loader::kit::metadata_url;
///
/// assert_eq!(
///     metadata_url("https://maven.neoforged.net/releases", "net.neoforged", "neoforge"),
///     "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml"
/// );
/// ```
#[must_use]
pub fn metadata_url(repository: &str, group: &str, artifact: &str) -> String {
    format!(
        "{}/{}/{artifact}/maven-metadata.xml",
        repository.trim_end_matches('/'),
        group.replace('.', "/")
    )
}

/// The parts of a `maven-metadata.xml` that version listings need.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MavenMetadata {
    /// Versions in document order, which repositories keep oldest first.
    pub versions: Vec<String>,
    /// The `<latest>` version, if given.
    pub latest: Option<String>,
    /// The `<release>` version, if given.
    pub release: Option<String>,
}

/// Returns the text between the first `<tag>` and its `</tag>` in `text`.
fn element<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

fn unescape(text: &str) -> String {
    text.trim()
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

impl MavenMetadata {
    /// Reads the versions of a `maven-metadata.xml`.
    ///
    /// Only `<versioning>` with `<latest>`, `<release>` and `<versions>` is
    /// read; the document is not otherwise validated.
    ///
    /// # Errors
    ///
    /// Returns a description when the document has no `<versioning>` element.
    ///
    /// # Examples
    ///
    /// ```
    /// use coblet_loader::kit::MavenMetadata;
    ///
    /// let metadata = MavenMetadata::parse(r#"<?xml version="1.0"?>
    /// <metadata><versioning>
    ///   <release>21.1.77</release>
    ///   <versions><version>21.1.1-beta</version><version>21.1.77</version></versions>
    /// </versioning></metadata>"#)?;
    /// assert_eq!(metadata.versions, ["21.1.1-beta", "21.1.77"]);
    /// assert_eq!(metadata.release.as_deref(), Some("21.1.77"));
    /// # Ok::<(), String>(())
    /// ```
    pub fn parse(xml: &str) -> Result<Self, String> {
        let versioning =
            element(xml, "versioning").ok_or_else(|| "no <versioning> element".to_owned())?;
        let mut versions = Vec::new();
        if let Some(mut list) = element(versioning, "versions") {
            while let Some(value) = element(list, "version") {
                versions.push(unescape(value));
                let consumed = list.find("</version>").map_or(list.len(), |end| end + 10);
                list = &list[consumed..];
            }
        }
        Ok(Self {
            versions,
            latest: element(versioning, "latest").map(unescape),
            release: element(versioning, "release").map(unescape),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::MavenMetadata;

    #[test]
    fn reads_versions_with_whitespace_and_entities() {
        let metadata = MavenMetadata::parse(
            "<metadata>\n  <groupId>a</groupId>\n  <versioning>\n    <versions>\n      <version> 1.0 </version>\n      <version>1.0&amp;fix</version>\n    </versions>\n  </versioning>\n</metadata>",
        )
        .unwrap();
        assert_eq!(metadata.versions, ["1.0", "1.0&fix"]);
        assert_eq!(metadata.latest, None);
        assert_eq!(
            MavenMetadata::parse("<metadata/>").unwrap_err(),
            "no <versioning> element"
        );
        let empty = MavenMetadata::parse("<versioning><latest>2</latest></versioning>").unwrap();
        assert_eq!(empty.versions, Vec::<String>::new());
        assert_eq!(empty.latest.as_deref(), Some("2"));
    }
}
