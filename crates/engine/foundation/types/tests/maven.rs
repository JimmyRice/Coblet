use super::{MavenCoordinate, MavenCoordinateError};

#[test]
fn parses_three_and_four_part_coordinates() {
    let plain: MavenCoordinate = "net.fabricmc:fabric-loader:0.16.9".parse().unwrap();
    assert_eq!(plain.group(), "net.fabricmc");
    assert_eq!(plain.artifact(), "fabric-loader");
    assert_eq!(plain.version(), "0.16.9");
    assert_eq!(plain.classifier(), None);
    assert_eq!(plain.extension(), "jar");
    assert_eq!(
        plain.relative_path(),
        "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"
    );

    let zipped: MavenCoordinate = "net.minecraftforge:forge:1.20.1-47.4.20:universal@zip"
        .parse()
        .unwrap();
    assert_eq!(zipped.classifier(), Some("universal"));
    assert_eq!(zipped.extension(), "zip");
    assert_eq!(zipped.file_name(), "forge-1.20.1-47.4.20-universal.zip");
}

#[test]
fn displays_the_input_form() {
    for text in [
        "org.lwjgl:lwjgl:3.3.3",
        "org.lwjgl:lwjgl:3.3.3:natives-linux",
        "de.oceanlabs.mcp:mcp_config:1.20.1-20230612.114412@zip",
        "a:b:c:d@e",
    ] {
        assert_eq!(text.parse::<MavenCoordinate>().unwrap().to_string(), text);
    }
}

#[test]
fn rejects_paths_that_escape_the_repository() {
    for text in [
        "..:x:1",
        "a..b:x:1",
        "a:..:1",
        "a:x:../../etc",
        "a:x:1:.",
        "a/b:x:1",
        "a:x:1@j/ar",
    ] {
        assert!(
            matches!(
                text.parse::<MavenCoordinate>(),
                Err(MavenCoordinateError::InvalidPart { .. })
            ),
            "{text} was accepted"
        );
    }
}

#[test]
fn rejects_wrong_part_counts() {
    for text in ["a:b", "a:b:c:d:e", ""] {
        assert!(
            matches!(
                text.parse::<MavenCoordinate>(),
                Err(MavenCoordinateError::WrongPartCount(_))
            ),
            "{text} was accepted"
        );
    }
}

#[test]
fn round_trips_through_serde() {
    let coordinate: MavenCoordinate = "org.ow2.asm:asm:9.7".parse().unwrap();
    let json = serde_json::to_string(&coordinate).unwrap();
    assert_eq!(json, r#""org.ow2.asm:asm:9.7""#);
    assert_eq!(
        serde_json::from_str::<MavenCoordinate>(&json).unwrap(),
        coordinate
    );
}
