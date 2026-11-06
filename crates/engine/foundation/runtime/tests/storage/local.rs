use std::path::Path;

use bytes::Bytes;

use super::TokioStorage;
use crate::{EntryKind, Storage};

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn writes_atomically_and_reads_back() {
    let dir = temp();
    let storage = TokioStorage::new();
    let file = dir.path().join("nested/dir/file.txt");
    storage
        .write_atomic(&file, Bytes::from("one"))
        .await
        .unwrap();
    storage
        .write_atomic(&file, Bytes::from("two"))
        .await
        .unwrap();
    assert_eq!(&storage.read(&file).await.unwrap()[..], b"two");
    let listed = storage.read_dir(file.parent().unwrap()).await.unwrap();
    assert_eq!(listed.len(), 1, "temporary files were left behind");
    assert_eq!(listed[0].kind, EntryKind::File);
}

#[tokio::test]
async fn streams_create_append_and_open() {
    let dir = temp();
    let storage = TokioStorage::new();
    let file = dir.path().join("a.part");
    let mut writer = storage.create(&file).await.unwrap();
    writer.write(Bytes::from("ab")).await.unwrap();
    writer.finish().await.unwrap();
    let mut writer = storage.append(&file).await.unwrap();
    writer.write(Bytes::from("cd")).await.unwrap();
    writer.finish().await.unwrap();

    let mut reader = storage.open(&file).await.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        collected.extend_from_slice(&chunk);
    }
    assert_eq!(collected, b"abcd");
    assert_eq!(storage.metadata(&file).await.unwrap().unwrap().len, 4);
}

#[tokio::test]
async fn moves_links_and_removes() {
    let dir = temp();
    let storage = TokioStorage::new();
    let a = dir.path().join("a");
    let b = dir.path().join("sub/b");
    storage.write_atomic(&a, Bytes::from("x")).await.unwrap();
    storage.rename(&a, &b).await.unwrap();
    assert!(storage.metadata(&a).await.unwrap().is_none());
    storage.hard_link(&b, &a).await.unwrap();
    storage.copy(&b, &dir.path().join("c")).await.unwrap();
    storage
        .symlink(Path::new("sub/b"), &dir.path().join("link"))
        .await
        .unwrap();
    let link = storage.metadata(&dir.path().join("link")).await.unwrap();
    assert_eq!(link.unwrap().kind, EntryKind::Symlink);
    storage.set_executable(&b).await.unwrap();
    storage.remove_file(&a).await.unwrap();
    storage.remove_file(&a).await.unwrap();
    storage
        .remove_dir_all(&dir.path().join("sub"))
        .await
        .unwrap();
    storage
        .remove_dir_all(&dir.path().join("sub"))
        .await
        .unwrap();
}

#[tokio::test]
async fn locks_are_exclusive_until_dropped() {
    let dir = temp();
    let storage = TokioStorage::new();
    let path = dir.path().join("instance.lock");
    let held = storage.lock(&path).await.unwrap();
    assert_eq!(held.path(), path);
    assert!(storage.try_lock(&path).await.unwrap().is_none());
    drop(held);
    assert!(storage.try_lock(&path).await.unwrap().is_some());
}
