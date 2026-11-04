use coblet_types::MiB;

/// Failure to build a launch command.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// The initial heap is larger than the maximum, or the maximum is zero.
    #[error("invalid memory limits: min {} MiB, max {} MiB", .min.0, .max.0)]
    Memory {
        /// Initial heap.
        min: MiB,
        /// Maximum heap.
        max: MiB,
    },
    /// A template uses a placeholder nothing resolves.
    #[error("unknown placeholder `${{{0}}}`")]
    UnknownPlaceholder(String),
    /// A template has `${` without a closing brace.
    #[error("unterminated placeholder in `{0}`")]
    Unterminated(String),
    /// A value the command needs was not given.
    #[error("missing {0}")]
    Missing(&'static str),
}
