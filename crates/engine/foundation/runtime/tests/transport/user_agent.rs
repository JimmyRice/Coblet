use super::{UserAgent, UserAgentError};

#[test]
fn accepts_product_tokens() {
    for value in [
        "a/1",
        "MyLauncher/1.0",
        "MyLauncher/1.0 (contact@example.com) coblet/0.1",
    ] {
        assert_eq!(UserAgent::new(value).unwrap().as_str(), value);
    }
}

#[test]
fn rejects_bad_values() {
    for value in ["", "MyLauncher", "/1.0", "MyLauncher/", " MyLauncher/1.0"] {
        assert!(
            matches!(
                UserAgent::new(value),
                Err(UserAgentError::MissingProductToken(_))
            ),
            "{value:?} was accepted"
        );
    }
    assert!(matches!(
        UserAgent::new("MyLauncher/1.0\r\nX-Evil: 1"),
        Err(UserAgentError::InvalidCharacter(_))
    ));
    assert!(matches!(
        UserAgent::new("启动器/1.0"),
        Err(UserAgentError::InvalidCharacter(_))
    ));
}
