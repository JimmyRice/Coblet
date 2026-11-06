use super::{OsName, PlatformNameError, PlatformTrait};

#[test]
fn lowercases_valid_names() {
    assert_eq!(OsName::new("Haiku").unwrap().as_str(), "haiku");
    assert_eq!(
        PlatformTrait::new("gpu.nvidia-prime_1").unwrap().as_str(),
        "gpu.nvidia-prime_1"
    );
}

#[test]
fn rejects_invalid_names() {
    assert_eq!(OsName::new(""), Err(PlatformNameError::Empty));
    assert!(matches!(
        OsName::new("mac os"),
        Err(PlatformNameError::InvalidCharacter { character: ' ', .. })
    ));
    assert_eq!(
        OsName::new(&"a".repeat(65)),
        Err(PlatformNameError::TooLong(65))
    );
}
