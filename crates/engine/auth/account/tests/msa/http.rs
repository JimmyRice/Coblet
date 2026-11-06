use super::{encode_pairs, percent_decode, percent_encode, query_pairs};

#[test]
fn encodes_and_decodes() {
    assert_eq!(percent_encode("a b/c~"), "a%20b%2Fc~");
    assert_eq!(
        encode_pairs(&[("scope", "XboxLive.signin offline_access"), ("x", "1")]),
        "scope=XboxLive.signin%20offline_access&x=1"
    );
    assert_eq!(percent_decode("a+b%2Fc").as_deref(), Some("a b/c"));
    assert_eq!(percent_decode("%zz"), None);
    assert_eq!(percent_decode("%4"), None);
}

#[test]
fn parses_queries() {
    let pairs = query_pairs("http://127.0.0.1:5/cb?code=M.a%2Bb&state=s1#frag").unwrap();
    assert_eq!(
        pairs,
        vec![
            ("code".to_owned(), "M.a+b".to_owned()),
            ("state".to_owned(), "s1".to_owned())
        ]
    );
    assert_eq!(query_pairs("app://cb").unwrap(), Vec::new());
    assert!(query_pairs("app://cb?x=%").is_none());
}
