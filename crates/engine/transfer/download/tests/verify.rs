use coblet_runtime::{Expectation, Storage, TokioStorage};
use coblet_types::FileHash;

use super::{FileIndex, VerifyMode, verify_file};

const ABC_SHA1: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

#[tokio::test]
async fn fast_mode_trusts_the_index_until_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let storage = TokioStorage::new();
    let file = dir.path().join("a");
    storage.write_atomic(&file, "abc".into()).await.unwrap();
    let expect = Expectation::hash(FileHash::new("sha1", ABC_SHA1).unwrap(), Some(3));
    let index = FileIndex::new();

    assert!(
        verify_file(&storage, &index, &file, &expect, VerifyMode::Fast)
            .await
            .unwrap()
    );
    assert_eq!(index.len(), 1);
    assert!(
        verify_file(&storage, &index, &file, &expect, VerifyMode::Fast)
            .await
            .unwrap()
    );

    // Same size, different content and modification time: the index no longer matches.
    std::thread::sleep(std::time::Duration::from_millis(20));
    storage.write_atomic(&file, "abd".into()).await.unwrap();
    assert!(
        !verify_file(&storage, &index, &file, &expect, VerifyMode::Fast)
            .await
            .unwrap()
    );
    assert!(
        !verify_file(&storage, &index, &file, &expect, VerifyMode::Strict)
            .await
            .unwrap()
    );
    assert!(
        !verify_file(
            &storage,
            &index,
            &dir.path().join("missing"),
            &expect,
            VerifyMode::Fast
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn saves_and_loads() {
    let dir = tempfile::tempdir().unwrap();
    let storage = TokioStorage::new();
    let file = dir.path().join("a");
    storage.write_atomic(&file, "abc".into()).await.unwrap();
    let expect = Expectation::hash(FileHash::new("sha1", ABC_SHA1).unwrap(), None);
    let index = FileIndex::new();
    verify_file(&storage, &index, &file, &expect, VerifyMode::Strict)
        .await
        .unwrap();
    let saved = dir.path().join("index.json");
    index.save(&storage, &saved).await.unwrap();
    let loaded = FileIndex::load(&storage, &saved).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert!(
        FileIndex::load(&storage, &dir.path().join("none"))
            .await
            .unwrap()
            .is_empty()
    );
}
