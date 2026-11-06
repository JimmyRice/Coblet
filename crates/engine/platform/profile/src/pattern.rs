use std::cmp::Ordering;

/// Matches `text` against a glob where `*` stands for any run of characters.
///
/// Used for Maven coordinates (`org.lwjgl:*:*:natives-linux*`) and argument
/// patterns (`-XstartOnFirstThread`).
#[must_use]
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return text.is_empty();
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(index) => rest = &rest[index + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Compares dotted library versions such as `3.3.1` or `2.9.4-nightly-20150209`
/// by their leading numeric segments; missing segments count as zero.
///
/// Only library versions (LWJGL, OS versions) are compared this way. Game
/// version IDs are never compared.
#[must_use]
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    fn numbers(version: &str) -> Vec<u64> {
        version
            .split(['.', '-', '+', '_'])
            .map_while(|part| part.parse().ok())
            .collect()
    }
    let (left, right) = (numbers(left), numbers(right));
    let len = left.len().max(right.len());
    (0..len)
        .map(|index| {
            let a = left.get(index).copied().unwrap_or(0);
            let b = right.get(index).copied().unwrap_or(0);
            a.cmp(&b)
        })
        .find(|ordering| ordering.is_ne())
        .unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
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
}
