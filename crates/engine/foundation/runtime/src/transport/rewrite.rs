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

    /// Reports whether a metadata document this rewriter redirected may be
    /// used although it does not match its hash; `false` by default.
    ///
    /// Some mirrors serve JSON documents reformatted, with the same content
    /// but other bytes. Only metadata ([`ArtifactKind::Metadata`] and
    /// [`ArtifactKind::Loader`] documents fetched through a metadata cache)
    /// can be accepted this way; downloaded files are always checked.
    fn trusts_metadata(&self) -> bool {
        false
    }
}

/// Where a request goes after the [`UrlRewriters`] chain.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    /// The address to request first.
    pub url: HttpUrl,
    /// The address before rewriting, when a rewriter changed it and the
    /// chain falls back to original addresses.
    pub fallback: Option<HttpUrl>,
    /// Whether a rewriter that changed the address trusts its metadata.
    pub trust_metadata: bool,
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
    trust_metadata: bool,
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
            trust_metadata: false,
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

    /// Accepts metadata documents from this mirror that do not match their
    /// hash, such as reformatted JSON; see [`UrlRewriter::trusts_metadata`].
    #[must_use]
    pub fn trusting_metadata(self) -> Self {
        Self {
            trust_metadata: true,
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

    fn trusts_metadata(&self) -> bool {
        self.trust_metadata
    }
}

/// Ordered chain of [`UrlRewriter`]s, cheap to clone.
///
/// Each rewriter sees the address the previous one returned, so a proxy
/// rule can follow a mirror rule. When a rewritten address fails, or
/// serves something that does not match its hash, the original address is
/// tried next; [`without_fallback`](UrlRewriters::without_fallback) turns
/// that off for setups that must never reach the original hosts.
#[derive(Clone)]
pub struct UrlRewriters {
    rewriters: Arc<[Arc<dyn UrlRewriter>]>,
    fallback: bool,
}

impl Default for UrlRewriters {
    fn default() -> Self {
        Self {
            rewriters: Arc::new([]),
            fallback: true,
        }
    }
}

impl UrlRewriters {
    /// Creates an empty chain that keeps every address.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the chain with `rewriter` appended.
    #[must_use]
    pub fn with(&self, rewriter: Arc<dyn UrlRewriter>) -> Self {
        Self {
            rewriters: self.rewriters.iter().cloned().chain([rewriter]).collect(),
            fallback: self.fallback,
        }
    }

    /// Returns the chain without falling back to original addresses.
    #[must_use]
    pub fn without_fallback(self) -> Self {
        Self {
            fallback: false,
            ..self
        }
    }

    /// Reports whether failed rewritten addresses fall back to the original ones.
    #[must_use]
    pub fn falls_back(&self) -> bool {
        self.fallback
    }

    /// Returns the number of rewriters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rewriters.len()
    }

    /// Returns whether the chain is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rewriters.is_empty()
    }

    /// Returns the address to request for `url`.
    #[must_use]
    pub fn apply(&self, url: &HttpUrl, kind: &ArtifactKind) -> HttpUrl {
        self.route(url, kind).url
    }

    /// Returns where a request for `url` goes and what happens when it fails.
    #[must_use]
    pub fn route(&self, url: &HttpUrl, kind: &ArtifactKind) -> Route {
        let mut current = url.clone();
        let mut trust_metadata = false;
        for rewriter in self.rewriters.iter() {
            if let Some(next) = rewriter.rewrite(&current, kind)
                && next != current
            {
                trust_metadata |= rewriter.trusts_metadata();
                current = next;
            }
        }
        let rewritten = current != *url;
        Route {
            fallback: (rewritten && self.fallback).then(|| url.clone()),
            trust_metadata: rewritten && trust_metadata,
            url: current,
        }
    }
}

impl FromIterator<Arc<dyn UrlRewriter>> for UrlRewriters {
    fn from_iter<I: IntoIterator<Item = Arc<dyn UrlRewriter>>>(iter: I) -> Self {
        Self {
            rewriters: iter.into_iter().collect(),
            fallback: true,
        }
    }
}

impl fmt::Debug for UrlRewriters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UrlRewriters")
            .field("rewriters", &self.rewriters.len())
            .field("fallback", &self.fallback)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn url(text: &str) -> HttpUrl {
        HttpUrl::new(text).unwrap()
    }

    #[test]
    fn prefix_rewriter_keeps_other_addresses() {
        let mirror = PrefixRewriter::new("https://a.example/", "https://b.example/x/").unwrap();
        assert_eq!(
            mirror
                .rewrite(&url("https://a.example/lib.jar"), &ArtifactKind::Library)
                .unwrap()
                .as_str(),
            "https://b.example/x/lib.jar"
        );
        assert!(
            mirror
                .rewrite(&url("https://c.example/lib.jar"), &ArtifactKind::Library)
                .is_none()
        );
    }

    #[test]
    fn prefix_rewriter_rejects_non_http_prefixes() {
        assert!(PrefixRewriter::new("ftp://a.example/", "https://b.example/").is_err());
        assert!(PrefixRewriter::new("https://a.example/", "file:///tmp/").is_err());
    }

    #[test]
    fn chain_applies_rewriters_in_order() {
        let chain = UrlRewriters::new()
            .with(Arc::new(
                PrefixRewriter::new("https://a.example/", "https://b.example/").unwrap(),
            ))
            .with(Arc::new(
                PrefixRewriter::new("https://b.example/", "https://c.example/").unwrap(),
            ));
        assert_eq!(chain.len(), 2);
        assert_eq!(
            chain
                .apply(&url("https://a.example/f"), &ArtifactKind::Asset)
                .as_str(),
            "https://c.example/f"
        );
        assert_eq!(
            UrlRewriters::new()
                .apply(&url("https://a.example/f"), &ArtifactKind::Asset)
                .as_str(),
            "https://a.example/f"
        );
    }

    #[test]
    fn kind_filter_limits_the_rewriter() {
        let mirror = PrefixRewriter::new("https://a.example/", "https://b.example/")
            .unwrap()
            .for_kinds([ArtifactKind::Metadata, ArtifactKind::Loader]);
        assert_eq!(
            mirror.kinds(),
            Some(&[ArtifactKind::Metadata, ArtifactKind::Loader][..])
        );
        let target = url("https://a.example/f");
        assert!(mirror.rewrite(&target, &ArtifactKind::Loader).is_some());
        assert!(mirror.rewrite(&target, &ArtifactKind::Asset).is_none());
    }

    #[test]
    fn routes_fall_back_to_the_original_address() {
        let mirror = Arc::new(
            PrefixRewriter::new("https://a.example/", "https://b.example/")
                .unwrap()
                .trusting_metadata(),
        );
        let chain = UrlRewriters::new().with(mirror);
        assert!(chain.falls_back());
        let route = chain.route(&url("https://a.example/f"), &ArtifactKind::Metadata);
        assert_eq!(route.url.as_str(), "https://b.example/f");
        assert_eq!(
            route.fallback.as_ref().map(HttpUrl::as_str),
            Some("https://a.example/f")
        );
        assert!(route.trust_metadata);

        let kept = chain.route(&url("https://c.example/f"), &ArtifactKind::Metadata);
        assert_eq!(kept.url.as_str(), "https://c.example/f");
        assert_eq!(kept.fallback, None);
        assert!(!kept.trust_metadata);

        let strict = chain.clone().without_fallback();
        assert!(!strict.falls_back());
        let route = strict.route(&url("https://a.example/f"), &ArtifactKind::Metadata);
        assert_eq!(route.fallback, None);
        assert!(route.trust_metadata);
    }

    #[test]
    fn only_a_rewriter_that_changed_the_address_grants_trust() {
        let trusting = Arc::new(
            PrefixRewriter::new("https://x.example/", "https://y.example/")
                .unwrap()
                .trusting_metadata(),
        );
        let plain =
            Arc::new(PrefixRewriter::new("https://a.example/", "https://b.example/").unwrap());
        let chain = UrlRewriters::new().with(trusting).with(plain);
        let route = chain.route(&url("https://a.example/f"), &ArtifactKind::Metadata);
        assert_eq!(route.url.as_str(), "https://b.example/f");
        assert!(!route.trust_metadata);
    }
}
