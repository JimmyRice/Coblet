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
