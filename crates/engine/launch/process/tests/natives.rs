use std::io::{Cursor, Write};

use bytes::Bytes;
use zip::write::SimpleFileOptions;

use super::unpack;

pub(crate) fn jar(entries: &[(&str, &[u8])]) -> Bytes {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in entries {
        if name.ends_with('/') {
            writer
                .add_directory(*name, SimpleFileOptions::default())
                .unwrap();
        } else {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
    }
    Bytes::from(writer.finish().unwrap().into_inner())
}

#[test]
fn skips_excluded_directories_and_unsafe_names() {
    let bytes = jar(&[
        ("META-INF/", b""),
        ("META-INF/MANIFEST.MF", b"x"),
        ("liblwjgl.dylib", b"native"),
        ("../evil.so", b"bad"),
        ("sub/libopenal.dylib", b"al"),
    ]);
    let files = unpack(bytes, &["META-INF/".to_owned()]).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|(path, _)| path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(names, ["liblwjgl.dylib", "sub/libopenal.dylib"]);
    assert_eq!(&files[0].1[..], b"native");
}
