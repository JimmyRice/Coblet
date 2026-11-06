use std::path::Path;

use super::named_path;

#[test]
fn refuses_escaping_asset_names() {
    let base = Path::new("/r");
    assert_eq!(
        named_path(base, "sound/a.ogg").unwrap(),
        Path::new("/r/sound/a.ogg")
    );
    assert!(named_path(base, "../x").is_none());
    assert!(named_path(base, "a//b").is_none());
    assert!(named_path(base, "a\\b").is_none());
}
