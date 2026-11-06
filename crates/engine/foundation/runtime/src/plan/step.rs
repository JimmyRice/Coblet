use std::fmt;
use std::sync::Arc;

use coblet_types::FileHash;
use serde::{Deserialize, Serialize};

use super::PlanPath;
use crate::{BoxFuture, HttpUrl, StepContext, StepError, StepOutcome};

/// One unit of work in a Plan, interpreted by the Operation executor.
///
/// The set is closed: every built-in change is expressed with these
/// variants, and anything else goes through [`Step::Custom`]. A Plan that
/// contains a custom step cannot be serialized.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// Fetch a file into staging or a shared directory.
    Download(DownloadStep),
    /// Check the hash of a file that is already present.
    VerifyHash(VerifyHashStep),
    /// Unpack an archive.
    Extract(ExtractStep),
    /// Write bytes that the Plan carries itself.
    WriteFile(WriteFileStep),
    /// Atomically move a staged file or directory to its final place.
    MoveIntoPlace(MoveStep),
    /// Delete a file or directory, or move it to the trash.
    Remove(RemoveStep),
    /// Run a child process, such as a Forge installer processor.
    RunProcess(ProcessStep),
    /// Write an Instance Backend's metadata file; runs after its preconditions are rechecked.
    WriteMetadata(MetadataStep),
    /// Work defined by an extension.
    #[serde(skip)]
    Custom(Arc<dyn CustomStep>),
}

/// Step implemented outside the engine.
pub trait CustomStep: fmt::Debug + Send + Sync + 'static {
    /// Returns a short kind name such as `acme:patch-options`.
    fn kind(&self) -> &str;

    /// Describes the step for a Plan preview.
    fn describe(&self) -> String;

    /// Does the work. The executor calls this once the step's dependencies have finished.
    fn run<'a>(&'a self, cx: &'a StepContext) -> BoxFuture<'a, Result<StepOutcome, StepError>>;
}

impl PartialEq for dyn CustomStep {
    /// Custom steps compare by identity, since their contents are opaque.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(self, other)
    }
}

/// Download of one file from one or more mirrors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadStep {
    /// Addresses tried in order; later ones are fallbacks.
    pub sources: Sources,
    /// What the downloaded file must match.
    pub expect: Expectation,
    /// Where the file goes.
    pub dest: PlanPath,
    /// What the file is, for statistics and per-kind limits.
    pub kind: ArtifactKind,
}

/// Non-empty list of download addresses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    /// The address tried first.
    pub primary: HttpUrl,
    /// Addresses tried after the primary one fails.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<HttpUrl>,
}

impl Sources {
    /// Lists a single address.
    #[must_use]
    pub fn single(url: HttpUrl) -> Self {
        Self {
            primary: url,
            fallbacks: Vec::new(),
        }
    }

    /// Iterates over all addresses in the order they are tried.
    pub fn iter(&self) -> impl Iterator<Item = &HttpUrl> {
        std::iter::once(&self.primary).chain(&self.fallbacks)
    }
}

/// What a file must match before it is accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum Expectation {
    /// The file must match every listed hash, and the size when given.
    Hashes {
        /// Expected size in bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        /// The first hash; there is always at least one.
        primary: FileHash,
        /// More hashes from the same metadata, such as SHA-512 next to SHA-1.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra: Vec<FileHash>,
    },
    /// The source publishes no hash. Plan previews show a warning for these.
    Unverified {
        /// Expected size in bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
    },
}

impl Expectation {
    /// Expects one hash and optionally a size.
    #[must_use]
    pub fn hash(hash: FileHash, size: Option<u64>) -> Self {
        Self::Hashes {
            size,
            primary: hash,
            extra: Vec::new(),
        }
    }

    /// Returns the expected size, if any.
    #[must_use]
    pub fn size(&self) -> Option<u64> {
        match self {
            Self::Hashes { size, .. } | Self::Unverified { size } => *size,
        }
    }

    /// Reports whether the file is checked against at least one hash.
    #[must_use]
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Hashes { .. })
    }
}

/// Kind of downloaded artifact.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A Maven library.
    Library,
    /// A native library archive.
    Natives,
    /// An asset index or asset object.
    Asset,
    /// The game client jar.
    ClientJar,
    /// A server jar.
    ServerJar,
    /// A Java runtime file.
    Runtime,
    /// A Mod, resource pack, shader pack or data pack.
    Content,
    /// A modpack archive.
    Pack,
    /// Loader metadata or an installer.
    Loader,
    /// A version manifest, version file or runtime index.
    Metadata,
    /// Anything else, named by the extension that created it.
    Other(String),
}

/// Hash check of an existing file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyHashStep {
    /// The file to check.
    pub path: PlanPath,
    /// What it must match.
    pub expect: Expectation,
}

/// Archive extraction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractStep {
    /// The zip or jar to read.
    pub archive: PlanPath,
    /// Directory the entries are written below.
    pub into: PlanPath,
    /// Entry name prefixes to skip, such as `META-INF/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// File written from bytes carried in the Plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFileStep {
    /// Where the file goes.
    pub path: PlanPath,
    /// The file contents.
    pub contents: Vec<u8>,
}

/// Atomic move from staging to the final location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveStep {
    /// The staged file or directory.
    pub from: PlanPath,
    /// The final location, replaced if present.
    pub to: PlanPath,
}

/// Removal of a file or directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveStep {
    /// What to remove.
    pub path: PlanPath,
    /// Whether to keep a copy in the trash.
    pub mode: RemoveMode,
}

/// How [`RemoveStep`] disposes of its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveMode {
    /// Move into `data_root/trash/`.
    Trash,
    /// Delete permanently.
    Delete,
}

/// Child process run by the executor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessStep {
    /// What to run.
    pub program: Program,
    /// Arguments, with Plan paths resolved at execution time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ProcessArg>,
    /// Working directory; the target directory when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PlanPath>,
    /// Files the process must produce, checked after it exits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<ExpectedOutput>,
}

/// Program run by a [`ProcessStep`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Program {
    /// The `java` executable of the runtime the executor selects for the target.
    Java,
    /// A program looked up on `PATH`.
    Command(String),
}

/// One argument of a [`ProcessStep`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessArg {
    /// Passed as written.
    Literal(String),
    /// Replaced by the absolute path.
    Path(PlanPath),
    /// Replaced by the absolute paths joined with the platform class path separator.
    ClassPath(Vec<PlanPath>),
}

/// File a process must leave behind, with its expected hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedOutput {
    /// The produced file.
    pub path: PlanPath,
    /// Its expected hash.
    pub hash: FileHash,
}

/// Metadata file written by an Instance Backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataStep {
    /// The metadata file, such as `target:instance.cfg`.
    pub path: PlanPath,
    /// The complete new document.
    pub contents: String,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_types::FileHash;

    use super::{
        ArtifactKind, CustomStep, DownloadStep, Expectation, ProcessArg, ProcessStep, Program,
        Sources, Step,
    };
    use crate::{BoxFuture, HttpUrl, PlanPath, RelPath, StepContext, StepError, StepOutcome};

    fn path(root: fn(RelPath) -> PlanPath, rel: &str) -> PlanPath {
        root(RelPath::new(rel).unwrap())
    }

    #[test]
    fn serializes_steps_with_a_type_tag() {
        let step = Step::Download(DownloadStep {
            sources: Sources::single(
                HttpUrl::new("https://libraries.minecraft.net/a.jar").unwrap(),
            ),
            expect: Expectation::hash(
                FileHash::new("sha1", "da39a3ee5e6b4b0d3255bfef95601890afd80709").unwrap(),
                Some(0),
            ),
            dest: path(PlanPath::data, "libraries/a.jar"),
            kind: ArtifactKind::Library,
        });
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json["type"], "download");
        assert_eq!(json["expect"]["check"], "hashes");
        assert_eq!(
            json["expect"]["primary"],
            "sha1:da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(serde_json::from_value::<Step>(json).unwrap(), step);
    }

    #[test]
    fn describes_processes_with_plan_paths() {
        let step = Step::RunProcess(ProcessStep {
            program: Program::Java,
            args: vec![
                ProcessArg::Literal("-cp".to_owned()),
                ProcessArg::ClassPath(vec![path(PlanPath::data, "libraries/p.jar")]),
                ProcessArg::Path(path(PlanPath::staging, "client.jar")),
            ],
            cwd: None,
            outputs: Vec::new(),
        });
        let text = serde_json::to_string(&step).unwrap();
        assert_eq!(serde_json::from_str::<Step>(&text).unwrap(), step);
    }

    #[test]
    fn refuses_to_serialize_custom_steps() {
        #[derive(Debug)]
        struct Patch;
        impl CustomStep for Patch {
            fn kind(&self) -> &'static str {
                "acme:patch"
            }
            fn describe(&self) -> String {
                "patch options.txt".to_owned()
            }
            fn run<'a>(
                &'a self,
                _cx: &'a StepContext,
            ) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
                Box::pin(async { Ok(StepOutcome::Succeeded) })
            }
        }
        let shared: Arc<dyn CustomStep> = Arc::new(Patch);
        let step = Step::Custom(Arc::clone(&shared));
        assert!(serde_json::to_string(&step).is_err());
        assert_eq!(step, Step::Custom(shared));
        assert_ne!(step, Step::Custom(Arc::new(Patch)));
    }

    #[test]
    fn reports_expectations() {
        let unverified = Expectation::Unverified { size: Some(10) };
        assert!(!unverified.is_verified());
        assert_eq!(unverified.size(), Some(10));
        let urls = Sources {
            primary: HttpUrl::new("https://a.example/x").unwrap(),
            fallbacks: vec![HttpUrl::new("https://b.example/x").unwrap()],
        };
        assert_eq!(urls.iter().count(), 2);
    }
}
