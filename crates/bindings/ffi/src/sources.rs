use std::ffi::{CString, c_char, c_void};

use coblet::runtime::{ArtifactKind, HttpUrl, UrlRewriter};

use crate::customize::Owned;
use crate::guard::guard;
use crate::types::{CobletStatus, Failure, text};

/// Callback table of an address rewriter written in C, for mirrors that a
/// rule from `coblet_builder_add_mirror` cannot describe.
///
/// `rewrite` runs once per request on the library's runtime threads,
/// possibly several at once, and should return quickly. `kind` names what
/// the request fetches in snake case, such as `library`, `asset` or
/// `metadata`. To change the address the callback passes the new one to
/// `coblet_rewrite_set_url` on `out`, which is valid only during the call;
/// otherwise the address stays. The library owns `user_data` from the call
/// that receives the table, even when it fails, and calls `free` once when
/// the `Coblet` is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CobletUrlRewriter {
    /// Passed to every callback.
    pub user_data: *mut c_void,
    /// Looks at one request address.
    pub rewrite: Option<
        unsafe extern "C" fn(
            user_data: *mut c_void,
            url: *const c_char,
            kind: *const c_char,
            out: *mut CobletRewrite,
        ),
    >,
    /// Releases `user_data`; may be NULL.
    pub free: Option<unsafe extern "C" fn(user_data: *mut c_void)>,
}

/// Result of one `rewrite` callback.
#[derive(Debug, Default)]
pub struct CobletRewrite {
    url: Option<String>,
}

type Rewrite = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut CobletRewrite);

/// C rewriter used as a coblet rewriter.
pub(crate) struct ForeignRewriter {
    rewrite: Rewrite,
    owned: Owned,
}

impl std::fmt::Debug for ForeignRewriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForeignRewriter").finish_non_exhaustive()
    }
}

impl ForeignRewriter {
    /// Takes over a callback table.
    pub(crate) fn new(table: &CobletUrlRewriter) -> Result<Self, Failure> {
        let owned = Owned {
            user_data: table.user_data,
            free: table.free,
        };
        let rewrite = table
            .rewrite
            .ok_or_else(|| Failure::argument("url rewriter", "needs a rewrite callback"))?;
        Ok(Self { rewrite, owned })
    }
}

/// Returns the snake case name of a kind, or the name of an `Other` kind.
pub(crate) fn kind_name(kind: &ArtifactKind) -> String {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(serde_json::Value::Object(map)) => map
            .into_iter()
            .next()
            .and_then(|(_, value)| value.as_str().map(str::to_owned))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

impl UrlRewriter for ForeignRewriter {
    #[allow(unsafe_code, reason = "calls a C rewriter callback")]
    fn rewrite(&self, url: &HttpUrl, kind: &ArtifactKind) -> Option<HttpUrl> {
        let url = CString::new(url.as_str()).ok()?;
        let kind = CString::new(kind_name(kind)).ok()?;
        let mut out = CobletRewrite::default();
        // SAFETY: the table owner promised a callback that accepts its user
        // data, two NUL-terminated strings and the result handle, all valid
        // for the duration of the call.
        unsafe {
            (self.rewrite)(
                self.owned.user_data,
                url.as_ptr(),
                kind.as_ptr(),
                &raw mut out,
            );
        };
        HttpUrl::new(out.url?).ok()
    }
}

/// Parses an optional JSON array of kind names, such as `["library", "metadata"]`.
pub(crate) fn parse_kinds(json: Option<&str>) -> Result<Option<Vec<ArtifactKind>>, Failure> {
    let Some(json) = json else {
        return Ok(None);
    };
    let kinds: Vec<ArtifactKind> = serde_json::from_str(json)
        .map_err(|error| Failure::argument("kinds", &error.to_string()))?;
    Ok((!kinds.is_empty()).then_some(kinds))
}

/// Sets the address a running `rewrite` callback wants instead; the
/// library copies it. An address that is not HTTP or HTTPS is ignored.
///
/// # Safety
///
/// `out` must be the handle passed to the running `rewrite` callback;
/// `url` must be NULL or NUL-terminated.
#[allow(unsafe_code, reason = "exported C symbol")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblet_rewrite_set_url(
    out: *mut CobletRewrite,
    url: *const c_char,
) -> CobletStatus {
    // SAFETY: forwarded from the caller's guarantees.
    unsafe {
        guard(std::ptr::null_mut(), || {
            let out = out
                .as_mut()
                .ok_or_else(|| Failure::argument("out", "is NULL"))?;
            out.url = Some(text(url, "url")?.to_owned());
            Ok(CobletStatus::Ok)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code, reason = "drives the C ABI")]

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use coblet::runtime::{ArtifactKind, HttpUrl, UrlRewriter};

    use super::{
        CobletRewrite, CobletUrlRewriter, ForeignRewriter, coblet_rewrite_set_url, kind_name,
        parse_kinds,
    };
    use crate::builder::{
        COBLET_MIRROR_TRUST_METADATA, coblet_builder_add_mirror, coblet_builder_add_url_rewriter,
        coblet_builder_add_version_manifest, coblet_builder_build, coblet_builder_new,
        coblet_builder_set_mirror_fallback,
    };
    use crate::context::{coblet_error_free, coblet_free};
    use crate::types::CobletStatus;

    static FREED: AtomicUsize = AtomicUsize::new(0);

    /// Sends `https://mirror.example/` to `https://proxy.example/` for libraries only.
    unsafe extern "C" fn proxy(
        _data: *mut c_void,
        url: *const c_char,
        kind: *const c_char,
        out: *mut CobletRewrite,
    ) {
        // SAFETY: the library passes NUL-terminated strings.
        let (url, kind) = unsafe { (CStr::from_ptr(url), CStr::from_ptr(kind)) };
        if kind.to_str().unwrap() != "library" {
            return;
        }
        if let Some(rest) = url
            .to_str()
            .unwrap()
            .strip_prefix("https://mirror.example/")
        {
            let new = CString::new(format!("https://proxy.example/{rest}")).unwrap();
            // SAFETY: `out` is the handle of this call.
            let status = unsafe { coblet_rewrite_set_url(out, new.as_ptr()) };
            assert_eq!(status, CobletStatus::Ok);
        }
    }

    unsafe extern "C" fn count_free(_data: *mut c_void) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }

    fn table() -> CobletUrlRewriter {
        CobletUrlRewriter {
            user_data: std::ptr::null_mut(),
            rewrite: Some(proxy),
            free: Some(count_free),
        }
    }

    #[test]
    fn kinds_have_snake_case_names() {
        assert_eq!(kind_name(&ArtifactKind::ClientJar), "client_jar");
        assert_eq!(kind_name(&ArtifactKind::Metadata), "metadata");
        assert_eq!(kind_name(&ArtifactKind::Other("acme".to_owned())), "acme");
        assert_eq!(parse_kinds(None).unwrap(), None);
        assert_eq!(parse_kinds(Some("[]")).unwrap(), None);
        assert_eq!(
            parse_kinds(Some(r#"["library", "metadata"]"#)).unwrap(),
            Some(vec![ArtifactKind::Library, ArtifactKind::Metadata])
        );
        assert!(parse_kinds(Some(r#"["librray"]"#)).is_err());
    }

    #[test]
    fn c_rewriters_and_mirrors_reach_the_coblet() {
        let rewriter = ForeignRewriter::new(&table()).unwrap();
        let url = HttpUrl::new("https://mirror.example/a.jar").unwrap();
        assert_eq!(
            rewriter
                .rewrite(&url, &ArtifactKind::Library)
                .unwrap()
                .as_str(),
            "https://proxy.example/a.jar"
        );
        assert!(rewriter.rewrite(&url, &ArtifactKind::Asset).is_none());
        drop(rewriter);
        let freed = FREED.load(Ordering::SeqCst);
        assert!(freed >= 1);

        let root = CString::new("/tmp/coblet-ffi-sources").unwrap();
        let agent = CString::new("coblet-ffi-test/1.0").unwrap();
        let mut builder = std::ptr::null_mut();
        let mut err = std::ptr::null_mut();
        // SAFETY: valid strings and out-pointers throughout; handles are freed once.
        unsafe {
            assert_eq!(
                coblet_builder_new(
                    root.as_ptr(),
                    agent.as_ptr(),
                    &raw mut builder,
                    &raw mut err
                ),
                CobletStatus::Ok
            );
            let from = CString::new("https://libraries.minecraft.net/").unwrap();
            let to = CString::new("https://mirror.example/").unwrap();
            let kinds = CString::new(r#"["library"]"#).unwrap();
            assert_eq!(
                coblet_builder_add_mirror(
                    builder,
                    from.as_ptr(),
                    to.as_ptr(),
                    kinds.as_ptr(),
                    COBLET_MIRROR_TRUST_METADATA,
                    &raw mut err
                ),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_set_mirror_fallback(builder, false, &raw mut err),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_add_url_rewriter(builder, table(), &raw mut err),
                CobletStatus::Ok
            );
            let id = CString::new("acme:experiments").unwrap();
            let manifest = CString::new("https://meta.example/experiments.json").unwrap();
            assert_eq!(
                coblet_builder_add_version_manifest(
                    builder,
                    id.as_ptr(),
                    manifest.as_ptr(),
                    &raw mut err
                ),
                CobletStatus::Ok
            );
            assert_eq!(
                coblet_builder_add_version_manifest(
                    builder,
                    id.as_ptr(),
                    manifest.as_ptr(),
                    &raw mut err
                ),
                CobletStatus::InvalidArgument
            );
            coblet_error_free(err);
            err = std::ptr::null_mut();

            let mut coblet = std::ptr::null_mut();
            assert_eq!(
                coblet_builder_build(builder, &raw mut coblet, &raw mut err),
                CobletStatus::Ok
            );
            let library = HttpUrl::new("https://libraries.minecraft.net/a.jar").unwrap();
            let inner = &(*coblet).inner;
            for rewriters in [inner.meta().rewriters(), inner.downloader().rewriters()] {
                assert!(!rewriters.falls_back());
                assert!(
                    rewriters
                        .route(&library, &ArtifactKind::Library)
                        .trust_metadata
                );
                assert_eq!(
                    rewriters.apply(&library, &ArtifactKind::Library).as_str(),
                    "https://proxy.example/a.jar"
                );
                assert_eq!(
                    rewriters.apply(&library, &ArtifactKind::Asset).as_str(),
                    library.as_str()
                );
            }
            let before = FREED.load(Ordering::SeqCst);
            coblet_free(coblet);
            assert_eq!(FREED.load(Ordering::SeqCst), before + 1);
        }
    }

    #[test]
    fn bad_mirror_arguments_are_rejected() {
        let root = CString::new("/tmp/coblet-ffi-sources").unwrap();
        let agent = CString::new("coblet-ffi-test/1.0").unwrap();
        let good = CString::new("https://libraries.minecraft.net/").unwrap();
        let bad = CString::new("file:///x").unwrap();
        let mut builder = std::ptr::null_mut();
        // SAFETY: valid strings and out-pointers; the builder is built and freed once.
        unsafe {
            let mut err = std::ptr::null_mut();
            assert_eq!(
                coblet_builder_new(
                    root.as_ptr(),
                    agent.as_ptr(),
                    &raw mut builder,
                    &raw mut err
                ),
                CobletStatus::Ok
            );
            for (from, flags) in [(&bad, 0), (&good, 4)] {
                let status = coblet_builder_add_mirror(
                    builder,
                    from.as_ptr(),
                    good.as_ptr(),
                    std::ptr::null(),
                    flags,
                    &raw mut err,
                );
                assert_eq!(status, CobletStatus::InvalidArgument);
                coblet_error_free(err);
                err = std::ptr::null_mut();
            }
            let mut coblet = std::ptr::null_mut();
            assert_eq!(
                coblet_builder_build(builder, &raw mut coblet, &raw mut err),
                CobletStatus::Ok
            );
            assert!((*coblet).inner.meta().rewriters().is_empty());
            coblet_free(coblet);
        }
    }
}
