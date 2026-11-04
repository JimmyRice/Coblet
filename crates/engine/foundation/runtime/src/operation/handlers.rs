use super::{StepContext, StepError};
use crate::{BoxFuture, DownloadStep, ExtractStep, ProcessStep, StepOutcome};

/// Runs [`Step::Download`](crate::Step::Download); `coblet-download` provides the implementation.
pub trait DownloadHandler: Send + Sync + 'static {
    /// Fetches the file, verifies it and places it at the step's destination.
    fn download<'a>(
        &'a self,
        step: &'a DownloadStep,
        cx: &'a StepContext,
    ) -> BoxFuture<'a, Result<StepOutcome, StepError>>;
}

/// Runs [`Step::Extract`](crate::Step::Extract).
pub trait ExtractHandler: Send + Sync + 'static {
    /// Unpacks the archive below the step's directory.
    fn extract<'a>(
        &'a self,
        step: &'a ExtractStep,
        cx: &'a StepContext,
    ) -> BoxFuture<'a, Result<StepOutcome, StepError>>;
}

/// Runs [`Step::RunProcess`](crate::Step::RunProcess), choosing the Java runtime for `Program::Java`.
pub trait ProcessHandler: Send + Sync + 'static {
    /// Runs the process and checks its expected outputs.
    fn run<'a>(
        &'a self,
        step: &'a ProcessStep,
        cx: &'a StepContext,
    ) -> BoxFuture<'a, Result<StepOutcome, StepError>>;
}
