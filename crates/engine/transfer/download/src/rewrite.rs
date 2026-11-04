use coblet_runtime::{ArtifactKind, HttpUrl};

/// Changes download addresses before a task starts, for proxies and regional mirrors.
///
/// Rewriting only changes where bytes come from: the task's hashes are
/// checked as usual, so a mirror cannot substitute other content. coblet
/// installs no rewriter by default.
///
/// # Examples
///
/// ```
/// use coblet_download::UrlRewriter;
/// use coblet_runtime::{ArtifactKind, HttpUrl};
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
