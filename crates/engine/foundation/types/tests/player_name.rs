use super::{PlayerName, PlayerNameError};

#[test]
fn accepts_valid_names() {
    for name in ["abc", "Steve", "Notch_2009", "ABCDEFGHIJKLMNOP"] {
        assert_eq!(PlayerName::new(name).unwrap().as_str(), name);
    }
}

#[test]
fn rejects_invalid_names() {
    assert_eq!(
        PlayerName::new("ab"),
        Err(PlayerNameError::Length("ab".to_owned()))
    );
    assert!(matches!(
        PlayerName::new("a".repeat(17)),
        Err(PlayerNameError::Length(_))
    ));
    assert!(matches!(
        PlayerName::new("Ste-ve"),
        Err(PlayerNameError::InvalidCharacter { character: '-', .. })
    ));
    assert!(matches!(
        PlayerName::new("南瞳大冒险"),
        Err(PlayerNameError::InvalidCharacter {
            character: '南',
            ..
        })
    ));
    assert!(serde_json::from_str::<PlayerName>(r#""x y z""#).is_err());
}
