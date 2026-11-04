//! Compile-time checks that public types stay usable across threads.

use crate::{
    Arch, ExtensionId, FileHash, GameVersionId, Libc, MavenCoordinate, MiB, OsFamily, Platform,
    PlayerName, Sha1Digest, VersionType,
};

const fn assert_send_sync<T: Send + Sync>() {}

const _: () = {
    assert_send_sync::<Arch>();
    assert_send_sync::<ExtensionId>();
    assert_send_sync::<FileHash>();
    assert_send_sync::<GameVersionId>();
    assert_send_sync::<Libc>();
    assert_send_sync::<MavenCoordinate>();
    assert_send_sync::<MiB>();
    assert_send_sync::<OsFamily>();
    assert_send_sync::<Platform>();
    assert_send_sync::<PlayerName>();
    assert_send_sync::<Sha1Digest>();
    assert_send_sync::<VersionType>();
};
