use std::fmt;
use std::sync::Arc;

use crate::{ArtifactKind, HttpUrl, HttpUrlError};

/// Changes request addresses before they are sent, for proxies and regional mirrors.
///
/// Rewriting only changes where bytes come from: downloads and metadata
/// documents are checked against their hashes as usual, so a mirror cannot
/// substitute other content. coblet installs no rewriter by default.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{ArtifactKind, HttpUrl, UrlRewriter};
///
/// struct Mirror;
///
/// impl UrlRewriter for Mirror {
///     fn rewrite(&self, url: &HttpUrl, _kind: &ArtifactKind) -> Option<HttpUrl> {
///         let rest = url.as_str().strip_prefix("https://libraries.minecraft.net/")?;
///         HttpUrl::new(format!("https://mirror.example/libraries/{rest}")).ok()
///     }
/// }
///
/// let url = HttpUrl::new("https://libraries.minecraft.net/a.jar")?;
/// assert_eq!(
///     Mirror.rewrite(&url, &ArtifactKind::Library).unwrap().as_str(),
///     "https://mirror.example/libraries/a.jar"
/// );
/// # Ok::<(), coblet_runtime::HttpUrlError>(())
/// ```
pub trait UrlRewriter: Send + Sync + 'static {
    /// Returns the address to use instead of `url`, or `None` to keep it.
    fn rewrite(&self, url: &HttpUrl, kind: &ArtifactKind) -> Option<HttpUrl>;
}

/// Rewriter that replaces one address prefix with another.
///
/// It is the data form of [`UrlRewriter`]: a mirror described by a pair of
/// prefixes needs no code, which suits configuration files and bindings.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{ArtifactKind, HttpUrl, PrefixRewriter, UrlRewriter};
///
/// let mirror = PrefixRewriter::new(
///     "https://piston-meta.mojang.com/",
///     "https://mirror.example/piston-meta/",
/// )?
/// .for_kinds([ArtifactKind::Metadata]);
///
/// let manifest = HttpUrl::new("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")?;
/// assert_eq!(
///     mirror.rewrite(&manifest, &ArtifactKind::Metadata).unwrap().as_str(),
///     "https://mirror.example/piston-meta/mc/game/version_manifest_v2.json"
/// );
/// assert!(mirror.rewrite(&manifest, &ArtifactKind::Library).is_none());
/// # Ok::<(), coblet_runtime::HttpUrlError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixRewriter {
    from: String,
    to: String,
    kinds: Option<Vec<ArtifactKind>>,
}

impl PrefixRewriter {
    /// Creates a rewriter that applies to every artifact kind.
    ///
    /// # Errors
    ///
    /// Returns [`HttpUrlError`] when either prefix is not an HTTP or HTTPS address.
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Result<Self, HttpUrlError> {
        let from = HttpUrl::new(from)?;
        let to = HttpUrl::new(to)?;
        Ok(Self {
            from: from.into(),
            to: to.into(),
            kinds: None,
        })
    }

    /// Limits the rewriter to these artifact kinds.
    #[must_use]
    pub fn for_kinds(self, kinds: impl IntoIterator<Item = ArtifactKind>) -> Self {
        Self {
            kinds: Some(kinds.into_iter().collect()),
            ..self
        }
    }

    /// Returns the prefix that is replaced.
    #[must_use]
    pub fn from(&self) -> &str {
        &self.from
    }

    /// Returns the replacement prefix.
    #[must_use]
    pub fn to(&self) -> &str {
        &self.to
    }

    /// Returns the kinds the rewriter is limited to, or `None` for all kinds.
    #[must_use]
    pub fn kinds(&self) -> Option<&[ArtifactKind]> {
        self.kinds.as_deref()
    }
}

impl UrlRewriter for PrefixRewriter {
    fn rewrite(&self, url: &HttpUrl, kind: &ArtifactKind) -> Option<HttpUrl> {
        if self
            .kinds
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(kind))
        {
            return None;
        }
        let rest = url.as_str().strip_prefix(self.from.as_str())?;
        HttpUrl::new(format!("{}{rest}", self.to)).ok()
    }
}

/// Ordered chain of [`UrlRewriter`]s, cheap to clone.
///
/// Each rewriter sees the address the previous one returned, so a proxy
/// rule can follow a mirror rule.
#[derive(Clone, Default)]
pub struct UrlRewriters(Arc<[Arc<dyn UrlRewriter>]>);

impl UrlRewriters {
    /// Creates an empty chain that keeps every address.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the chain with `rewriter` appended.
    #[must_use]
    pub fn with(&self, rewriter: Arc<dyn UrlRewriter>) -> Self {
        Self(self.0.iter().cloned().chain([rewriter]).collect())
    }

    /// Returns the number of rewriters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the chain is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the address to request for `url`.
    #[must_use]
    pub fn apply(&self, url: &HttpUrl, kind: &ArtifactKind) -> HttpUrl {
        self.0.iter().fold(url.clone(), |url, rewriter| {
            rewriter.rewrite(&url, kind).unwrap_or(url)
        })
    }
}

impl FromIterator<Arc<dyn UrlRewriter>> for UrlRewriters {
    fn from_iter<I: IntoIterator<Item = Arc<dyn UrlRewriter>>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl fmt::Debug for UrlRewriters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("UrlRewriters").field(&self.0.len()).finish()
    }
}

#[cfg(test)]
#[path = "rewrite_tests.rs"]
mod tests;
