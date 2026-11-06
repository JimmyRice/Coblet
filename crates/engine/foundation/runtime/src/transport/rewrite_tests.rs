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
    let plain = Arc::new(PrefixRewriter::new("https://a.example/", "https://b.example/").unwrap());
    let chain = UrlRewriters::new().with(trusting).with(plain);
    let route = chain.route(&url("https://a.example/f"), &ArtifactKind::Metadata);
    assert_eq!(route.url.as_str(), "https://b.example/f");
    assert!(!route.trust_metadata);
}
