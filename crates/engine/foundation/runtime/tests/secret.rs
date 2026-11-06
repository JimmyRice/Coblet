use super::{MemorySecretStore, Secret, SecretKey, SecretKeyError, SecretStore};

#[tokio::test]
async fn stores_replaces_and_deletes() {
    let store: Box<dyn SecretStore> = Box::new(MemorySecretStore::new());
    let key = SecretKey::new("coblet:microsoft/abc/refresh_token").unwrap();
    assert_eq!(store.get(&key).await.unwrap(), None);
    store.put(&key, Secret::new("first")).await.unwrap();
    store.put(&key, Secret::new("second")).await.unwrap();
    assert_eq!(store.get(&key).await.unwrap().unwrap().expose(), "second");
    store.delete(&key).await.unwrap();
    store.delete(&key).await.unwrap();
    assert_eq!(store.get(&key).await.unwrap(), None);
}

#[test]
fn never_prints_secret_values() {
    let secret = Secret::new("hunter2");
    assert_eq!(secret.to_string(), "<redacted>");
    assert!(!format!("{:?}", Some(secret)).contains("hunter2"));
}

#[test]
fn validates_keys() {
    assert_eq!(SecretKey::new(""), Err(SecretKeyError::Empty));
    assert!(matches!(
        SecretKey::new("a b"),
        Err(SecretKeyError::InvalidCharacter(_))
    ));
}
