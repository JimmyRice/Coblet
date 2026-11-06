use std::path::{Path, PathBuf};

use super::{TestModule, has_autotests_off, normalize, test_modules};

#[test]
fn finds_test_modules_and_their_shape() {
    let source = r#"
mod plain;

#[cfg(test)]
#[path = "../tests/meta.rs"]
mod tests;

#[cfg(test)]
mod mock {
    fn helper() {}
}

mod outer {
    #[cfg(test)]
    mod deep;
}

#[cfg(not(test))]
mod release;

#[cfg(all(test, unix))]
mod unix_only {}

#[cfg(all(unix, not(test)))]
mod unix_release;
"#;
    let found = test_modules(source).unwrap();
    let shape: Vec<_> = found
        .iter()
        .map(|module| {
            (
                module.name.as_str(),
                module.path.as_deref(),
                module.inline,
                module.nested,
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("tests", Some("../tests/meta.rs"), false, false),
            ("mock", None, true, false),
            ("deep", None, false, true),
            ("unix_only", None, true, false),
        ]
    );
    assert_eq!(
        found[0],
        TestModule {
            line: 4,
            name: "tests".to_owned(),
            path: Some("../tests/meta.rs".to_owned()),
            inline: false,
            nested: false,
        }
    );
}

#[test]
fn reads_autotests_and_resolves_parent_paths() {
    assert!(has_autotests_off(
        "[package]\nname = \"a\"\nautotests = false\n"
    ));
    assert!(!has_autotests_off("[package]\nname = \"a\"\n"));
    assert!(!has_autotests_off("[package]\nautotests = true\n"));
    assert_eq!(
        normalize(Path::new(
            "/c/src/transport/../../tests/transport/rewrite.rs"
        )),
        PathBuf::from("/c/tests/transport/rewrite.rs")
    );
}
