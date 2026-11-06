use std::sync::Arc;

use coblet_runtime::{Secret, SecretKey, SecretStore, SecretStoreError, TokioStorage};

use super::{EncryptedFileStore, EncryptionKey};

fn key(byte: u8) -> EncryptionKey {
    EncryptionKey::from_bytes([byte; 32])
}

#[tokio::test]
async fn stores_secrets_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/secrets.bin");
    let store = EncryptedFileStore::new(Arc::new(TokioStorage::new()), &path, key(1));
    let name = SecretKey::new("coblet:microsoft/acc/refresh_token").unwrap();
    assert_eq!(store.get(&name).await.unwrap(), None);
    store
        .put(&name, Secret::new("hunter2-token"))
        .await
        .unwrap();
    let other = SecretKey::new("coblet:microsoft/acc/minecraft_token").unwrap();
    store.put(&other, Secret::new("mc")).await.unwrap();

    let raw = std::fs::read(&path).unwrap();
    assert!(raw.starts_with(b"CBLTSEC1"));
    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains("hunter2") && !text.contains("refresh_token"));

    let reopened = EncryptedFileStore::new(Arc::new(TokioStorage::new()), &path, key(1));
    assert_eq!(
        reopened.get(&name).await.unwrap().unwrap().expose(),
        "hunter2-token"
    );
    reopened.delete(&name).await.unwrap();
    reopened.delete(&name).await.unwrap();
    assert_eq!(store.get(&name).await.unwrap(), None);
    assert_eq!(store.get(&other).await.unwrap().unwrap().expose(), "mc");

    // A second write produces a different file because the nonce changes.
    store.put(&other, Secret::new("mc")).await.unwrap();
    assert_ne!(std::fs::read(&path).unwrap(), raw);
}

#[tokio::test]
async fn refuses_a_wrong_key_or_a_damaged_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.bin");
    let name = SecretKey::new("a/b").unwrap();
    let store = EncryptedFileStore::new(Arc::new(TokioStorage::new()), &path, key(1));
    store.put(&name, Secret::new("x")).await.unwrap();

    let wrong = EncryptedFileStore::new(Arc::new(TokioStorage::new()), &path, key(2));
    assert!(matches!(
        wrong.get(&name).await,
        Err(SecretStoreError::Unavailable(_))
    ));
    assert!(wrong.put(&name, Secret::new("y")).await.is_err());

    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        store.get(&name).await,
        Err(SecretStoreError::Unavailable(_))
    ));
    std::fs::write(&path, b"short").unwrap();
    assert!(store.get(&name).await.is_err());
}

#[test]
fn keys_are_random_and_never_printed() {
    let a = EncryptionKey::generate().unwrap();
    let b = EncryptionKey::generate().unwrap();
    assert_ne!(a.as_bytes(), b.as_bytes());
    assert_eq!(format!("{a:?}"), "EncryptionKey(<redacted>)");
}
