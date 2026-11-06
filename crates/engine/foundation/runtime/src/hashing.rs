use coblet_types::{Digest, FileHash, HashAlgorithm};
use sha1::Digest as _;

use crate::Expectation;

/// Computes SHA-1, SHA-256 and SHA-512 digests over a stream of chunks.
///
/// Only the algorithms asked for are computed, so a download checked
/// against SHA-1 pays for SHA-1 alone. The byte count is always tracked.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{Expectation, StreamHasher};
/// use coblet_types::FileHash;
///
/// let expect = Expectation::hash(
///     FileHash::new("sha1", "a9993e364706816aba3e25717850c26c9cd0d89d")?,
///     Some(3),
/// );
/// let mut hasher = StreamHasher::for_expectation(&expect);
/// hasher.update(b"a");
/// hasher.update(b"bc");
/// assert!(hasher.finish().check(&expect).is_ok());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct StreamHasher {
    sha1: Option<sha1::Sha1>,
    sha256: Option<sha2::Sha256>,
    sha512: Option<sha2::Sha512>,
    len: u64,
}

/// Digests and length of a hashed byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputedHashes {
    /// Number of bytes hashed.
    pub len: u64,
    sha1: Option<FileHash>,
    sha256: Option<FileHash>,
    sha512: Option<FileHash>,
}

/// Reason a file does not match its [`Expectation`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The size differs.
    #[error("expected {expected} bytes, got {actual}")]
    Size {
        /// Expected size.
        expected: u64,
        /// Actual size.
        actual: u64,
    },
    /// A digest differs.
    #[error("expected {expected}, got {actual}")]
    Hash {
        /// Expected hash.
        expected: Box<FileHash>,
        /// Computed hash.
        actual: Box<FileHash>,
    },
    /// None of the expected hashes uses an algorithm coblet computes.
    #[error("cannot verify a {0} hash")]
    UnsupportedAlgorithm(HashAlgorithm),
}

impl StreamHasher {
    /// Creates a hasher for the given algorithms; unsupported ones are ignored.
    #[must_use]
    pub fn new<'a>(algorithms: impl IntoIterator<Item = &'a HashAlgorithm>) -> Self {
        algorithms
            .into_iter()
            .fold(Self::default(), |hasher, algorithm| match algorithm {
                HashAlgorithm::Sha1 => Self {
                    sha1: Some(sha1::Sha1::new()),
                    ..hasher
                },
                HashAlgorithm::Sha256 => Self {
                    sha256: Some(sha2::Sha256::new()),
                    ..hasher
                },
                HashAlgorithm::Sha512 => Self {
                    sha512: Some(sha2::Sha512::new()),
                    ..hasher
                },
                _ => hasher,
            })
    }

    /// Creates a hasher for every algorithm named by `expect`.
    #[must_use]
    pub fn for_expectation(expect: &Expectation) -> Self {
        match expect {
            Expectation::Hashes { primary, extra, .. } => {
                let algorithms: Vec<HashAlgorithm> = std::iter::once(primary)
                    .chain(extra)
                    .map(FileHash::algorithm)
                    .collect();
                Self::new(&algorithms)
            }
            Expectation::Unverified { .. } => Self::default(),
        }
    }

    /// Feeds a chunk.
    pub fn update(&mut self, data: &[u8]) {
        self.len += u64::try_from(data.len()).unwrap_or(u64::MAX);
        if let Some(hasher) = &mut self.sha1 {
            hasher.update(data);
        }
        if let Some(hasher) = &mut self.sha256 {
            hasher.update(data);
        }
        if let Some(hasher) = &mut self.sha512 {
            hasher.update(data);
        }
    }

    /// Returns the bytes hashed so far.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Reports whether no bytes were hashed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Completes the digests.
    #[must_use]
    pub fn finish(self) -> ComputedHashes {
        ComputedHashes {
            len: self.len,
            sha1: self
                .sha1
                .map(|h| FileHash::Sha1(Digest::from_bytes(h.finalize().into()))),
            sha256: self
                .sha256
                .map(|h| FileHash::Sha256(Digest::from_bytes(h.finalize().into()))),
            sha512: self
                .sha512
                .map(|h| FileHash::Sha512(Digest::from_bytes(h.finalize().into()))),
        }
    }
}

impl ComputedHashes {
    /// Returns the computed hash for `algorithm`, if it was requested.
    #[must_use]
    pub fn get(&self, algorithm: &HashAlgorithm) -> Option<&FileHash> {
        match algorithm {
            HashAlgorithm::Sha1 => self.sha1.as_ref(),
            HashAlgorithm::Sha256 => self.sha256.as_ref(),
            HashAlgorithm::Sha512 => self.sha512.as_ref(),
            _ => None,
        }
    }

    /// Checks the size and every computable hash of `expect`.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::Size`] or [`VerifyError::Hash`] on a mismatch,
    /// and [`VerifyError::UnsupportedAlgorithm`] when none of the expected
    /// hashes could be computed.
    pub fn check(&self, expect: &Expectation) -> Result<(), VerifyError> {
        if let Some(expected) = expect.size()
            && expected != self.len
        {
            return Err(VerifyError::Size {
                expected,
                actual: self.len,
            });
        }
        let Expectation::Hashes { primary, extra, .. } = expect else {
            return Ok(());
        };
        let mut checked = 0_usize;
        for expected in std::iter::once(primary).chain(extra) {
            if let Some(actual) = self.get(&expected.algorithm()) {
                if actual != expected {
                    return Err(VerifyError::Hash {
                        expected: Box::new(expected.clone()),
                        actual: Box::new(actual.clone()),
                    });
                }
                checked += 1;
            }
        }
        if checked == 0 {
            return Err(VerifyError::UnsupportedAlgorithm(primary.algorithm()));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/hashing.rs"]
mod tests;
