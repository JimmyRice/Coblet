use super::parse_properties;

#[test]
fn parses_the_property_listing() {
    let output = "Property settings:\n    file.encoding = UTF-8\n    java.home = /opt/jdk-17\n    \
                      java.vendor = Eclipse Adoptium\n    java.version = 17.0.21\n    os.arch = aarch64\n    \
                      line.separator = \\n \n\nopenjdk version \"17.0.21\" 2025-10-21\n";
    let properties = parse_properties(output);
    assert_eq!(properties.version.as_deref(), Some("17.0.21"));
    assert_eq!(properties.home.as_deref(), Some("/opt/jdk-17"));
    assert_eq!(properties.arch.as_deref(), Some("aarch64"));
    assert_eq!(properties.vendor.as_deref(), Some("Eclipse Adoptium"));
}
