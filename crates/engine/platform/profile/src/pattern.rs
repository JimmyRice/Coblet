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
#[path = "../tests/pattern.rs"]
mod tests;
