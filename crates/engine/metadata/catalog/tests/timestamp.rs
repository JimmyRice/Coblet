use super::Timestamp;

#[test]
fn parses_offsets_and_fractions() {
    let utc = Timestamp::parse("2009-05-13T20:11:00+00:00").unwrap();
    assert_eq!(utc.unix(), 1_242_245_460);
    assert_eq!(Timestamp::parse("2009-05-13T22:11:00+02:00").unwrap(), utc);
    assert_eq!(Timestamp::parse("2009-05-13T20:11:00.125Z").unwrap(), utc);
    assert_eq!(utc.to_string(), "2009-05-13T20:11:00Z");
    assert_eq!(Timestamp::from_unix(0).to_string(), "1970-01-01T00:00:00Z");
    assert_eq!(Timestamp::parse("1969-12-31T23:59:59Z").unwrap().unix(), -1);
}

#[test]
fn rejects_malformed_text() {
    for text in [
        "",
        "2009-05-13",
        "2009-05-13T20:11:00",
        "2009-13-13T20:11:00Z",
        "2009-05-13T20:11:00+0200",
        "2009-05-13T20:11:00.Z",
        "２００９-05-13T20:11:00Z",
    ] {
        assert!(Timestamp::parse(text).is_err(), "{text}");
    }
}
