use std::sync::Arc;

use coblet::runtime::HttpUrl;

/// What a request fetches, so a mirror rule can apply to some kinds only.
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
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
    /// Mod Loader metadata or an installer.
    Loader,
    /// A version manifest, version file or runtime index.
    Metadata,
    /// Anything else, named by the extension that created it.
    Other {
        /// The name.
        name: String,
    },
}

impl From<ArtifactKind> for coblet::runtime::ArtifactKind {
    fn from(kind: ArtifactKind) -> Self {
        match kind {
            ArtifactKind::Library => Self::Library,
            ArtifactKind::Natives => Self::Natives,
            ArtifactKind::Asset => Self::Asset,
            ArtifactKind::ClientJar => Self::ClientJar,
            ArtifactKind::ServerJar => Self::ServerJar,
            ArtifactKind::Runtime => Self::Runtime,
            ArtifactKind::Content => Self::Content,
            ArtifactKind::Pack => Self::Pack,
            ArtifactKind::Loader => Self::Loader,
            ArtifactKind::Metadata => Self::Metadata,
            ArtifactKind::Other { name } => Self::Other(name),
        }
    }
}

impl From<&coblet::runtime::ArtifactKind> for ArtifactKind {
    fn from(kind: &coblet::runtime::ArtifactKind) -> Self {
        use coblet::runtime::ArtifactKind as Kind;
        match kind {
            Kind::Library => Self::Library,
            Kind::Natives => Self::Natives,
            Kind::Asset => Self::Asset,
            Kind::ClientJar => Self::ClientJar,
            Kind::ServerJar => Self::ServerJar,
            Kind::Runtime => Self::Runtime,
            Kind::Content => Self::Content,
            Kind::Pack => Self::Pack,
            Kind::Loader => Self::Loader,
            Kind::Metadata => Self::Metadata,
            Kind::Other(name) => Self::Other { name: name.clone() },
            other => Self::Other {
                name: format!("{other:?}").to_lowercase(),
            },
        }
    }
}

/// Address rewriter written in Swift, Kotlin or Python, for mirrors that a
/// prefix rule cannot describe.
///
/// It is called once per request on the library's worker threads, so it
/// should return quickly; prefer `CobletBuilder.add_mirror` when a prefix
/// rule is enough.
#[uniffi::export(foreign)]
pub trait UrlRewriter: Send + Sync {
    /// Returns the address to use instead of `url`, or `None` to keep it.
    /// An address that is not HTTP or HTTPS is ignored.
    fn rewrite(&self, url: String, kind: ArtifactKind) -> Option<String>;
}

/// Foreign rewriter used as a coblet rewriter.
pub(crate) struct ForeignRewriter(pub(crate) Arc<dyn UrlRewriter>);

impl coblet::runtime::UrlRewriter for ForeignRewriter {
    fn rewrite(&self, url: &HttpUrl, kind: &coblet::runtime::ArtifactKind) -> Option<HttpUrl> {
        let rewritten = self.0.rewrite(url.as_str().to_owned(), kind.into())?;
        HttpUrl::new(rewritten).ok()
    }
}
