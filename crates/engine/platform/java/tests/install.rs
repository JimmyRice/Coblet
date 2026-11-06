use super::major_version;

#[test]
fn reads_major_versions() {
    assert_eq!(major_version("1.8.0_74"), Some(8));
    assert_eq!(major_version("17.0.21"), Some(17));
    assert_eq!(major_version("25"), Some(25));
    assert_eq!(major_version("21-ea"), Some(21));
    assert_eq!(major_version(""), None);
}
