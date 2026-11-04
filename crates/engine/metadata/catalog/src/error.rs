use coblet_runtime::{HttpUrl, StorageError, TransportError, VerifyError};
use coblet_types::ExtensionId;

/// Failure to list or fetch catalog data.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The request did not complete and nothing usable was cached.
    #[error("request to {url} failed: {source}")]
    Transport {
        /// The address.
        url: HttpUrl,
        /// The transport failure.
        #[source]
        source: TransportError,
    },
    /// The server answered with an unexpected status and nothing usable was cached.
    #[error("{url} answered with status {status}")]
    Status {
        /// The address.
        url: HttpUrl,
        /// The status code.
        status: u16,
    },
    /// The document does not match the hash the caller expected.
    #[error("document from {url} does not match: {source}")]
    Verify {
        /// The address.
        url: HttpUrl,
        /// The mismatch.
        #[source]
        source: VerifyError,
    },
    /// The document could not be parsed.
    #[error("cannot parse {what}: {message}")]
    Parse {
        /// What was being parsed, such as a URL or a file name.
        what: String,
        /// The parser's description.
        message: String,
    },
    /// Reading or writing the cache failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A cache key is not a safe relative path.
    #[error("cache key `{0}` is not a relative path")]
    InvalidKey(String),
    /// A source failed; other sources may still have answered.
    #[error("catalog source `{source_id}` failed: {error}")]
    Source {
        /// The source.
        source_id: ExtensionId,
        /// Its failure.
        #[source]
        error: Box<CatalogError>,
    },
    /// The catalog has no sources.
    #[error("the catalog has no sources")]
    NoSources,
}

impl CatalogError {
    /// Creates a [`CatalogError::Parse`] from any displayable error.
    #[must_use]
    pub fn parse(what: impl Into<String>, error: impl std::fmt::Display) -> Self {
        Self::Parse {
            what: what.into(),
            message: error.to_string(),
        }
    }
}
