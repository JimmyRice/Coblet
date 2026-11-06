use super::OsFamily;

#[test]
fn round_trips_every_known_family() {
    for family in [
        OsFamily::Windows,
        OsFamily::MacOs,
        OsFamily::Linux,
        OsFamily::FreeBsd,
        OsFamily::OpenBsd,
        OsFamily::NetBsd,
        OsFamily::DragonFly,
        OsFamily::Illumos,
        OsFamily::Android,
    ] {
        assert_eq!(family.as_str().parse::<OsFamily>().unwrap(), family);
    }
}

#[test]
fn keeps_unknown_systems() {
    let haiku: OsFamily = "haiku".parse().unwrap();
    assert!(matches!(&haiku, OsFamily::Other(name) if name.as_str() == "haiku"));
    assert_eq!(serde_json::to_string(&haiku).unwrap(), r#""haiku""#);
    assert_eq!(
        serde_json::from_str::<OsFamily>(r#""FreeBSD""#).unwrap(),
        OsFamily::FreeBsd
    );
}
