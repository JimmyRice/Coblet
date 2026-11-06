use std::io;

use super::StorageError;

#[test]
fn reports_operation_and_path() {
    let error = StorageError::io(
        "rename",
        "/data/a.part",
        io::Error::new(io::ErrorKind::NotFound, "no such file"),
    );
    assert!(error.is_not_found());
    assert_eq!(
        error.to_string(),
        "rename failed for /data/a.part: no such file"
    );
}
