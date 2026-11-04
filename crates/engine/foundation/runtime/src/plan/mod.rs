//! Plans: complete, previewable, serializable descriptions of a change.

mod path;
mod step;

pub use path::{PathRoot, PlanPath, RelPath, RelPathError};
pub use step::{
    ArtifactKind, CustomStep, DownloadStep, Expectation, ExpectedOutput, ExtractStep, MetadataStep,
    MoveStep, ProcessArg, ProcessStep, Program, RemoveMode, RemoveStep, Sources, Step,
    VerifyHashStep, WriteFileStep,
};
