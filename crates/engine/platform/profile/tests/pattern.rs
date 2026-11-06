use std::cmp::Ordering;

use super::{compare_versions, glob_matches};

#[test]
fn matches_globs() {
    let lib = "org.lwjgl:lwjgl-glfw:3.3.1:natives-linux";
    assert!(glob_matches("org.lwjgl:*:*:natives-linux*", lib));
    assert!(glob_matches("*", lib));
    assert!(glob_matches(lib, lib));
    assert!(!glob_matches("org.lwjgl:*:*:natives-macos*", lib));
    assert!(!glob_matches("org.lwjgl:*", "com.mojang:x:1"));
    assert!(glob_matches("a*b*c", "aXbYc"));
    assert!(!glob_matches("a*b*c", "aXbY"));
    assert!(!glob_matches("ab*ba", "aba"));
    assert!(glob_matches("", ""));
}

#[test]
fn compares_library_versions() {
    assert_eq!(compare_versions("3.3.1", "3.3.0"), Ordering::Greater);
    assert_eq!(compare_versions("3.2.2", "3.3"), Ordering::Less);
    assert_eq!(compare_versions("3.3", "3.3.0"), Ordering::Equal);
    assert_eq!(
        compare_versions("2.9.4-nightly-20150209", "3.0.0"),
        Ordering::Less
    );
    assert_eq!(compare_versions("15.5", "11"), Ordering::Greater);
}
