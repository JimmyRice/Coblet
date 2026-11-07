use std::collections::BTreeMap;
use std::fmt::Write as _;

use coblet_runtime::{BoxFuture, Extension, HttpUrl};
use coblet_types::{Arch, ExtensionId, FileHash, OsFamily};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    ArchiveFormat, JavaDownload, JavaError, JavaPackage, JavaProvider, JavaQuery, ProviderContext,
};

/// Java provider described by data instead of code: a fixed list of
/// packages, or a JSON API and where its fields are.
///
/// Addresses take `{os}`, `{arch}` and `{major}`, percent-encoded, with
/// the names the `[os]` and `[arch]` tables give; names missing from the
/// tables are the canonical ones, such as `macos` and `aarch64`. It
/// registers like any other [`JavaProvider`].
///
/// # Format
///
/// ```toml
/// id = "acme:temurin"
/// name = "Temurin"
///
/// [packages]                       # format: "json" or "static"
/// format = "json"
/// url = "https://api.acme.net/v3/latest/{major}?os={os}&arch={arch}&type=jre"
/// items = ""                       # JSON pointer to the array; "" is the document
/// id = "/binary/package/name"      # pointer to a plain file name
/// version = "/version/semver"      # string, number or array of numbers (joined with dots)
/// download = "/binary/package/link"
/// hash = "/binary/package/checksum"
/// hash_algorithm = "sha256"        # when the hash has no `sha256:` prefix
/// size = "/binary/package/size"    # optional
/// home = "/binary/home"            # optional pointer to the Java home inside the archive
/// archive = "auto"                 # "auto" (from the address), "zip", "tar.gz" or "manifest"
///
/// [os]
/// macos = "mac"
///
/// [arch]
/// x86_64 = "x64"
/// ```
///
/// A `static` provider lists `[[packages.list]]` entries with `id`,
/// `version`, `major`, `os`, `arch`, `url`, `hash` and optional `size`,
/// `archive` and `home`; the entries matching the query's system,
/// architecture and major version are offered.
///
/// Items without a hash are skipped: the engine installs nothing it cannot
/// check. With `archive = "auto"`, items whose address does not end in
/// `.zip`, `.tar.gz` or `.tgz`, such as installers, are skipped too.
///
/// # Examples
///
/// ```
/// use coblet_java::DataJavaProvider;
/// use coblet_runtime::Extension;
///
/// let provider = DataJavaProvider::from_toml(r#"
///     id = "acme:java"
///     name = "Acme Java"
///     [packages]
///     format = "static"
///     [[packages.list]]
///     id = "acme-17-linux-x64"
///     version = "17.0.12"
///     major = 17
///     os = "linux"
///     arch = "x86_64"
///     url = "https://java.acme.test/17.tar.gz"
///     hash = "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
/// "#)?;
/// assert_eq!(provider.id().as_str(), "acme:java");
/// # Ok::<(), coblet_java::JavaError>(())
/// ```
#[derive(Clone, Debug)]
pub struct DataJavaProvider {
    id: ExtensionId,
    name: String,
    packages: Packages,
    os_names: BTreeMap<String, String>,
    arch_names: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    name: String,
    packages: PackagesSpec,
    #[serde(default)]
    os: BTreeMap<String, String>,
    #[serde(default)]
    arch: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant, reason = "parsed once per definition")]
enum PackagesSpec {
    Json {
        url: String,
        #[serde(default)]
        items: String,
        id: String,
        version: String,
        download: String,
        hash: String,
        #[serde(default)]
        hash_algorithm: Option<String>,
        #[serde(default)]
        size: Option<String>,
        #[serde(default)]
        home: Option<String>,
        #[serde(default)]
        major: Option<String>,
        #[serde(default)]
        archive: Option<String>,
    },
    Static {
        list: Vec<StaticEntry>,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticEntry {
    id: String,
    version: String,
    major: u32,
    os: OsFamily,
    arch: Arch,
    url: HttpUrl,
    hash: FileHash,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    archive: Option<String>,
    #[serde(default)]
    home: Option<String>,
}

/// How a package is downloaded, as data files name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Auto,
    Archive(ArchiveFormat),
    Manifest,
}

impl Kind {
    fn parse(text: Option<&str>) -> Result<Self, String> {
        Ok(match text.unwrap_or("auto") {
            "auto" => Self::Auto,
            "zip" => Self::Archive(ArchiveFormat::Zip),
            "tar.gz" => Self::Archive(ArchiveFormat::TarGz),
            "manifest" => Self::Manifest,
            other => return Err(format!("unknown archive kind `{other}`")),
        })
    }

    /// Returns the download form for an address, or `None` when `Auto`
    /// cannot tell the format from it.
    #[allow(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "the address is lower-cased first"
    )]
    fn download(
        self,
        url: HttpUrl,
        hash: FileHash,
        size: Option<u64>,
        home: Option<String>,
    ) -> Option<JavaDownload> {
        let format = match self {
            Self::Manifest => return Some(JavaDownload::Manifest { url, hash, size }),
            Self::Archive(format) => format,
            Self::Auto => {
                let path = url
                    .as_str()
                    .split(['?', '#'])
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if path.ends_with(".zip") {
                    ArchiveFormat::Zip
                } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
                    ArchiveFormat::TarGz
                } else {
                    return None;
                }
            }
        };
        Some(JavaDownload::Archive {
            url,
            hash,
            size,
            format,
            home,
        })
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant, reason = "one per provider")]
enum Packages {
    Json(JsonFields),
    Static(Vec<(StaticEntry, Kind)>),
}

#[derive(Clone, Debug)]
struct JsonFields {
    url: String,
    items: String,
    id: String,
    version: String,
    download: String,
    hash: String,
    hash_algorithm: Option<String>,
    size: Option<String>,
    home: Option<String>,
    major: Option<String>,
    kind: Kind,
}

impl DataJavaProvider {
    /// Parses a definition in TOML.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Definition`] when the text is malformed.
    pub fn from_toml(text: &str) -> Result<Self, JavaError> {
        let definition: Definition =
            toml::from_str(text).map_err(|error| JavaError::Definition(error.to_string()))?;
        Self::from_definition(definition)
    }

    /// Parses a definition in JSON, with the same fields as the TOML form.
    ///
    /// # Errors
    ///
    /// Returns [`JavaError::Definition`] when the text is malformed.
    pub fn from_json(text: &str) -> Result<Self, JavaError> {
        let definition: Definition =
            serde_json::from_str(text).map_err(|error| JavaError::Definition(error.to_string()))?;
        Self::from_definition(definition)
    }

    fn from_definition(definition: Definition) -> Result<Self, JavaError> {
        let invalid = JavaError::Definition;
        let id = ExtensionId::new(&definition.id)
            .map_err(|error| invalid(format!("id `{}`: {error}", definition.id)))?;
        let packages = match definition.packages {
            PackagesSpec::Json {
                url,
                items,
                id,
                version,
                download,
                hash,
                hash_algorithm,
                size,
                home,
                major,
                archive,
            } => Packages::Json(JsonFields {
                url,
                items,
                id,
                version,
                download,
                hash,
                hash_algorithm,
                size,
                home,
                major,
                kind: Kind::parse(archive.as_deref()).map_err(invalid)?,
            }),
            PackagesSpec::Static { list } => Packages::Static(
                list.into_iter()
                    .map(|entry| {
                        let kind = Kind::parse(entry.archive.as_deref()).map_err(invalid)?;
                        Ok((entry, kind))
                    })
                    .collect::<Result<_, JavaError>>()?,
            ),
        };
        Ok(Self {
            id,
            name: definition.name,
            packages,
            os_names: definition.os,
            arch_names: definition.arch,
        })
    }

    fn fill(&self, template: &str, query: &JavaQuery) -> String {
        let os = query.os.as_str();
        let arch = query.arch.as_str();
        let os = self.os_names.get(os).map_or(os, String::as_str);
        let arch = self.arch_names.get(arch).map_or(arch, String::as_str);
        template
            .replace("{os}", &encode(os))
            .replace("{arch}", &encode(arch))
            .replace("{major}", &query.major.to_string())
    }

    async fn json_packages(
        &self,
        cx: &ProviderContext<'_>,
        fields: &JsonFields,
        query: &JavaQuery,
    ) -> Result<Vec<JavaPackage>, JavaError> {
        let url = self.fill(&fields.url, query);
        let name = format!(
            "packages-{}-{}-{}.json",
            query.os.as_str(),
            query.arch.as_str(),
            query.major
        );
        let document: Value = cx.fetch_json(&url, &name).await?;
        let items = document
            .pointer(&fields.items)
            .and_then(Value::as_array)
            .ok_or_else(|| JavaError::provider(&self.id, format!("{url}: no item array")))?;
        let mut packages = Vec::new();
        for item in items {
            match json_package(fields, item, query) {
                Ok(Some(package)) => packages.push(package),
                Ok(None) => {}
                Err(reason) => {
                    tracing::debug!(provider = %self.id, %reason, "skipping java package");
                }
            }
        }
        Ok(packages)
    }
}

fn json_package(
    fields: &JsonFields,
    item: &Value,
    query: &JavaQuery,
) -> Result<Option<JavaPackage>, String> {
    let text = |pointer: &str| item.pointer(pointer).and_then(Value::as_str);
    let id = text(&fields.id).ok_or("no id")?.to_owned();
    let version = item
        .pointer(&fields.version)
        .and_then(version_text)
        .ok_or("no version")?;
    let url = text(&fields.download).ok_or("no download address")?;
    let url = HttpUrl::new(url).map_err(|error| format!("{url}: {error}"))?;
    let Some(hash) = text(&fields.hash).filter(|hash| !hash.is_empty()) else {
        return Ok(None);
    };
    let hash = match (&fields.hash_algorithm, hash.contains(':')) {
        (Some(algorithm), false) => format!("{algorithm}:{hash}"),
        _ => hash.to_owned(),
    };
    let hash: FileHash = hash.parse().map_err(|error| format!("hash: {error}"))?;
    let size = fields
        .size
        .as_deref()
        .and_then(|pointer| item.pointer(pointer))
        .and_then(Value::as_u64);
    let home = fields.home.as_deref().and_then(text).map(str::to_owned);
    let major = fields
        .major
        .as_deref()
        .and_then(|pointer| item.pointer(pointer))
        .and_then(Value::as_u64)
        .and_then(|major| u32::try_from(major).ok())
        .unwrap_or(query.major);
    let Some(download) = fields.kind.download(url, hash, size, home) else {
        return Ok(None);
    };
    Ok(Some(JavaPackage {
        id,
        version,
        major,
        arch: query.arch.clone(),
        download,
    }))
}

fn static_packages(list: &[(StaticEntry, Kind)], query: &JavaQuery) -> Vec<JavaPackage> {
    list.iter()
        .filter(|(entry, _)| {
            entry.os == query.os && entry.arch == query.arch && entry.major == query.major
        })
        .filter_map(|(entry, kind)| {
            let download = kind.download(
                entry.url.clone(),
                entry.hash.clone(),
                entry.size,
                entry.home.clone(),
            )?;
            Some(JavaPackage {
                id: entry.id.clone(),
                version: entry.version.clone(),
                major: entry.major,
                arch: entry.arch.clone(),
                download,
            })
        })
        .collect()
}

/// Reads a version field: a string, a number, or numbers joined with dots.
fn version_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Array(parts) => {
            let parts: Option<Vec<String>> = parts
                .iter()
                .map(|part| part.as_u64().map(|part| part.to_string()))
                .collect();
            parts.map(|parts| parts.join("."))
        }
        _ => None,
    }
}

/// Percent-encodes everything but unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

impl Extension for DataJavaProvider {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl JavaProvider for DataJavaProvider {
    fn packages<'a>(
        &'a self,
        cx: &'a ProviderContext<'a>,
        query: &'a JavaQuery,
    ) -> BoxFuture<'a, Result<Vec<JavaPackage>, JavaError>> {
        Box::pin(async move {
            match &self.packages {
                Packages::Json(fields) => self.json_packages(cx, fields, query).await,
                Packages::Static(list) => Ok(static_packages(list, query)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_runtime::{
        BoxFuture, Extension, MemoryBody, Request, Response, TokioStorage, Transport,
        TransportError,
    };
    use coblet_types::{Arch, OsFamily};

    use super::DataJavaProvider;
    use crate::{
        ArchiveFormat, JavaDownload, JavaError, JavaPackage, JavaProvider, JavaQuery,
        ProviderContext,
    };

    const LISTING: &str = r#"[
        { "name": "acme-21-mac.tar.gz", "version": [21, 0, 8], "link": "https://cdn.acme.test/acme-21-mac.tar.gz",
          "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08", "size": 42 },
        { "name": "acme-21-mac.dmg", "version": [21, 0, 8], "link": "https://cdn.acme.test/acme-21-mac.dmg",
          "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" },
        { "name": "acme-21-unchecked.zip", "version": "21.0.8", "link": "https://cdn.acme.test/acme-21.zip" }
    ]"#;

    struct Api(Mutex<Vec<String>>);

    impl Transport for Api {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            self.0
                .lock()
                .unwrap()
                .push(request.url().as_str().to_owned());
            Box::pin(async { Ok(Response::new(200, Vec::new(), MemoryBody::new(LISTING))) })
        }
    }

    async fn packages(
        provider: &DataJavaProvider,
        query: &JavaQuery,
    ) -> (Vec<JavaPackage>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let api = Arc::new(Api(Mutex::default()));
        let catalog = CatalogContext::new(
            MetaCache::new(api.clone(), Arc::new(TokioStorage::new()), dir.path()),
            CachePolicy::Revalidate,
        );
        let cx = ProviderContext::new(&catalog, provider.id());
        let packages = provider.packages(&cx, query).await.unwrap();
        let requested = api.0.lock().unwrap().clone();
        (packages, requested)
    }

    #[tokio::test]
    async fn reads_a_json_api_with_name_tables() {
        let provider = DataJavaProvider::from_toml(
            r#"
            id = "acme:java"
            name = "Acme"
            [packages]
            format = "json"
            url = "https://api.acme.test/{major}?os={os}&arch={arch}"
            id = "/name"
            version = "/version"
            download = "/link"
            hash = "/sha256"
            hash_algorithm = "sha256"
            size = "/size"
            [os]
            macos = "mac os"
            [arch]
            aarch64 = "arm64"
            "#,
        )
        .unwrap();
        let query = JavaQuery::new(OsFamily::MacOs, Arch::Aarch64, 21);
        let (packages, requested) = packages(&provider, &query).await;
        assert_eq!(
            requested,
            ["https://api.acme.test/21?os=mac%20os&arch=arm64"]
        );
        assert_eq!(packages.len(), 1, "{packages:?}");
        let package = &packages[0];
        assert_eq!(package.id, "acme-21-mac.tar.gz");
        assert_eq!(package.version, "21.0.8");
        assert_eq!((package.major, package.arch.clone()), (21, Arch::Aarch64));
        assert!(matches!(
            &package.download,
            JavaDownload::Archive {
                format: ArchiveFormat::TarGz,
                size: Some(42),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn filters_static_entries() {
        let provider = DataJavaProvider::from_json(
            r#"{ "id": "acme:static", "name": "Static", "packages": { "format": "static", "list": [
                { "id": "a", "version": "17.0.1", "major": 17, "os": "linux", "arch": "aarch64",
                  "url": "https://java.acme.test/a.zip",
                  "hash": "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" },
                { "id": "b", "version": "21.0.1", "major": 21, "os": "linux", "arch": "aarch64",
                  "url": "https://java.acme.test/b.json", "archive": "manifest",
                  "hash": "sha1:0000000000000000000000000000000000000001" }
            ] } }"#,
        )
        .unwrap();
        let (packages, requested) = packages(
            &provider,
            &JavaQuery::new(OsFamily::Linux, Arch::Aarch64, 21),
        )
        .await;
        assert_eq!(requested.len(), 0);
        let ids: Vec<_> = packages.iter().map(|package| package.id.as_str()).collect();
        assert_eq!(ids, ["b"]);
        assert!(matches!(
            packages[0].download,
            JavaDownload::Manifest { .. }
        ));
    }

    #[test]
    fn rejects_malformed_definitions() {
        let unknown_kind = r#"
            id = "acme:java"
            name = "Acme"
            [packages]
            format = "json"
            url = "https://api.acme.test/"
            id = "/name"
            version = "/version"
            download = "/link"
            hash = "/sha256"
            archive = "rar"
        "#;
        assert!(matches!(
            DataJavaProvider::from_toml(unknown_kind),
            Err(JavaError::Definition(message)) if message.contains("rar")
        ));
        assert!(DataJavaProvider::from_toml("id = \"bad id\"").is_err());
    }
}
