use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Fixed-size hash output, displayed and parsed as lowercase hexadecimal.
///
/// Parsing accepts upper- and lowercase digits; `N` is the output size in bytes.
///
/// # Examples
///
/// ```
/// use coblet_types::Sha1Digest;
///
/// let digest: Sha1Digest = "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709".parse()?;
/// assert_eq!(digest.to_string(), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
/// # Ok::<(), coblet_types::DigestError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest<const N: usize>([u8; N]);

/// SHA-1 output, the hash in Mojang version metadata.
pub type Sha1Digest = Digest<20>;
/// SHA-256 output.
pub type Sha256Digest = Digest<32>;
/// SHA-512 output.
pub type Sha512Digest = Digest<64>;

/// Reason a hexadecimal digest was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DigestError {
    /// The string has the wrong number of hex digits.
    #[error("digest has {actual} hex digits, expected {expected}")]
    WrongLength {
        /// Expected number of hex digits.
        expected: usize,
        /// Number of characters found.
        actual: usize,
    },
    /// The string contains a character that is not a hex digit.
    #[error("digest contains `{0}`, which is not a hex digit")]
    NotHex(char),
}

impl<const N: usize> Digest<N> {
    /// Wraps raw hash output.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; N]) -> Self {
        Self(bytes)
    }

    /// Returns the raw hash output.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; N] {
        &self.0
    }

    /// Parses `2 * N` hex digits.
    ///
    /// # Errors
    ///
    /// Returns [`DigestError::WrongLength`] or [`DigestError::NotHex`].
    pub fn from_hex(hex: &str) -> Result<Self, DigestError> {
        if let Some(c) = hex.chars().find(|c| !c.is_ascii()) {
            return Err(DigestError::NotHex(c));
        }
        if hex.len() != N * 2 {
            return Err(DigestError::WrongLength {
                expected: N * 2,
                actual: hex.len(),
            });
        }
        let mut bytes = [0_u8; N];
        let (pairs, _) = hex.as_bytes().as_chunks::<2>();
        for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (nibble(high)? << 4) | nibble(low)?;
        }
        Ok(Self(bytes))
    }
}

fn nibble(digit: u8) -> Result<u8, DigestError> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        other => Err(DigestError::NotHex(char::from(other))),
    }
}

impl<const N: usize> fmt::Display for Digest<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl<const N: usize> fmt::Debug for Digest<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl<const N: usize> FromStr for Digest<N> {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

impl<const N: usize> TryFrom<String> for Digest<N> {
    type Error = DigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_hex(&value)
    }
}

impl<const N: usize> From<Digest<N>> for String {
    fn from(value: Digest<N>) -> Self {
        value.to_string()
    }
}

/// Hash algorithm named by metadata, with an open fallback.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HashAlgorithm {
    /// SHA-1.
    Sha1,
    /// SHA-256.
    Sha256,
    /// SHA-512.
    Sha512,
    /// An algorithm the engine cannot compute, such as a `murmur2` fingerprint.
    Other(String),
}

impl HashAlgorithm {
    /// Returns the lowercase name, such as `sha512`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
            Self::Other(name) => name,
        }
    }
}

impl From<&str> for HashAlgorithm {
    fn from(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "sha1" | "sha-1" => Self::Sha1,
            "sha256" | "sha-256" => Self::Sha256,
            "sha512" | "sha-512" => Self::Sha512,
            _ => Self::Other(name.to_owned()),
        }
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Expected hash of a file, written as `<algorithm>:<value>`, such as `sha1:da39…`.
///
/// # Examples
///
/// ```
/// use coblet_types::{FileHash, HashAlgorithm};
///
/// let hash: FileHash = "sha1:da39a3ee5e6b4b0d3255bfef95601890afd80709".parse()?;
/// assert_eq!(hash.algorithm(), HashAlgorithm::Sha1);
///
/// let murmur: FileHash = "murmur2:3608199863".parse()?;
/// assert_eq!(murmur.algorithm().as_str(), "murmur2");
/// # Ok::<(), coblet_types::FileHashError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum FileHash {
    /// A SHA-1 digest.
    Sha1(Sha1Digest),
    /// A SHA-256 digest.
    Sha256(Sha256Digest),
    /// A SHA-512 digest.
    Sha512(Sha512Digest),
    /// A value for an algorithm the engine does not compute, kept verbatim.
    Other {
        /// The algorithm name as written in the source metadata.
        algorithm: String,
        /// The value as written in the source metadata.
        value: String,
    },
}

/// Reason a `<algorithm>:<value>` hash was rejected.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FileHashError {
    /// The text has no `:` or an empty side.
    #[error("file hash `{0}` is not in <algorithm>:<value> form")]
    Malformed(String),
    /// The algorithm is known but the value is not a valid digest.
    #[error("invalid {algorithm} digest: {source}")]
    Digest {
        /// The algorithm the digest was parsed for.
        algorithm: HashAlgorithm,
        /// The digest parsing failure.
        #[source]
        source: DigestError,
    },
}

impl FileHash {
    /// Builds a hash from an algorithm name and a value, as found in metadata.
    ///
    /// # Errors
    ///
    /// Returns [`FileHashError::Digest`] when the algorithm is SHA-1, SHA-256
    /// or SHA-512 and the value is not a digest of the right length.
    pub fn new(algorithm: &str, value: &str) -> Result<Self, FileHashError> {
        let parsed = HashAlgorithm::from(algorithm);
        let wrap = |source| FileHashError::Digest {
            algorithm: parsed.clone(),
            source,
        };
        Ok(match &parsed {
            HashAlgorithm::Sha1 => Self::Sha1(value.parse().map_err(wrap)?),
            HashAlgorithm::Sha256 => Self::Sha256(value.parse().map_err(wrap)?),
            HashAlgorithm::Sha512 => Self::Sha512(value.parse().map_err(wrap)?),
            HashAlgorithm::Other(name) => Self::Other {
                algorithm: name.clone(),
                value: value.to_owned(),
            },
        })
    }

    /// Returns the algorithm of this hash.
    #[must_use]
    pub fn algorithm(&self) -> HashAlgorithm {
        match self {
            Self::Sha1(_) => HashAlgorithm::Sha1,
            Self::Sha256(_) => HashAlgorithm::Sha256,
            Self::Sha512(_) => HashAlgorithm::Sha512,
            Self::Other { algorithm, .. } => HashAlgorithm::Other(algorithm.clone()),
        }
    }
}

impl fmt::Display for FileHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sha1(digest) => write!(f, "sha1:{digest}"),
            Self::Sha256(digest) => write!(f, "sha256:{digest}"),
            Self::Sha512(digest) => write!(f, "sha512:{digest}"),
            Self::Other { algorithm, value } => write!(f, "{algorithm}:{value}"),
        }
    }
}

impl FromStr for FileHash {
    type Err = FileHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once(':') {
            Some((algorithm, value)) if !algorithm.is_empty() && !value.is_empty() => {
                Self::new(algorithm, value)
            }
            _ => Err(FileHashError::Malformed(s.to_owned())),
        }
    }
}

impl TryFrom<String> for FileHash {
    type Error = FileHashError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<FileHash> for String {
    fn from(value: FileHash) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
#[path = "../tests/hash.rs"]
mod tests;
