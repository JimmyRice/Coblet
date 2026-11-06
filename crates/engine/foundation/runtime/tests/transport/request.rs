use super::{HttpUrl, HttpUrlError};

#[test]
fn validates_urls() {
    assert!(HttpUrl::new("HTTPS://Example.com").unwrap().is_https());
    assert!(!HttpUrl::new("http://example.com/x").unwrap().is_https());
    assert!(matches!(
        HttpUrl::new("ftp://x"),
        Err(HttpUrlError::UnsupportedScheme(_))
    ));
    assert!(matches!(
        HttpUrl::new("https:///path"),
        Err(HttpUrlError::MissingHost(_))
    ));
    assert!(matches!(
        HttpUrl::new("https://"),
        Err(HttpUrlError::MissingHost(_))
    ));
    assert!(matches!(
        HttpUrl::new("https://a.com/a b"),
        Err(HttpUrlError::InvalidCharacter(_))
    ));
}
