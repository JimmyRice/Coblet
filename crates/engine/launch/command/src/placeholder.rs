use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;

use crate::CommandError;

/// Names of the placeholders coblet fills itself.
///
/// Some only get a value in some launches, such as `resolution_width`
/// without a window size; nothing else may supply them.
pub const BUILTIN_PLACEHOLDERS: &[&str] = &[
    "assets_index_name",
    "assets_root",
    "auth_access_token",
    "auth_player_name",
    "auth_session",
    "auth_uuid",
    "auth_xuid",
    "classpath",
    "classpath_separator",
    "clientid",
    "game_assets",
    "game_directory",
    "launcher_name",
    "launcher_version",
    "library_directory",
    "natives_directory",
    "quickPlayMultiplayer",
    "quickPlayPath",
    "quickPlayRealms",
    "quickPlaySingleplayer",
    "resolution_height",
    "resolution_width",
    "user_properties",
    "user_type",
    "version_name",
    "version_type",
];

/// Tells whether coblet fills the placeholder `name` itself.
#[must_use]
pub fn is_builtin_placeholder(name: &str) -> bool {
    BUILTIN_PLACEHOLDERS.binary_search(&name).is_ok()
}

/// Returns the names of the `${name}` placeholders in `template`, in order.
///
/// An unterminated `${` ends the list.
///
/// # Examples
///
/// ```
/// use coblet_launch_command::placeholder_names;
///
/// let names: Vec<_> = placeholder_names("-Dport=${acme_port}:${version_name}").collect();
/// assert_eq!(names, ["acme_port", "version_name"]);
/// ```
pub fn placeholder_names(template: &str) -> impl Iterator<Item = &str> {
    let mut rest = template;
    std::iter::from_fn(move || {
        let start = rest.find("${")?;
        let after = &rest[start + 2..];
        let end = after.find('}')?;
        rest = &after[end + 1..];
        Some(&after[..end])
    })
}

/// Supplies values for placeholders the built-in table does not know.
///
/// Resolvers are asked in registration order after the built-in table.
pub trait PlaceholderResolver: Send + Sync {
    /// Returns the value of `${name}`, or `None` when the resolver does not know it.
    fn resolve(&self, name: &str) -> Option<OsString>;
}

/// What happens to a placeholder nothing resolves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum UnknownPlaceholder {
    /// Fail the build.
    #[default]
    Error,
    /// Keep `${name}` as written.
    Keep,
    /// Replace it with nothing.
    Empty,
}

/// Placeholder values for one build.
pub(crate) struct Values<'a> {
    pub(crate) table: BTreeMap<&'static str, OsString>,
    pub(crate) resolvers: &'a [Arc<dyn PlaceholderResolver>],
    pub(crate) unknown: UnknownPlaceholder,
}

impl Values<'_> {
    fn lookup(&self, name: &str) -> Option<OsString> {
        self.table.get(name).cloned().or_else(|| {
            self.resolvers
                .iter()
                .find_map(|resolver| resolver.resolve(name))
        })
    }

    /// Replaces every `${name}` in `template`.
    pub(crate) fn substitute(&self, template: &str) -> Result<OsString, CommandError> {
        let mut out = OsString::new();
        let mut rest = template;
        while let Some(start) = rest.find("${") {
            out.push(&rest[..start]);
            let after = &rest[start + 2..];
            let end = after
                .find('}')
                .ok_or_else(|| CommandError::Unterminated(template.to_owned()))?;
            let name = &after[..end];
            match self.lookup(name) {
                Some(value) => out.push(value),
                None => match self.unknown {
                    UnknownPlaceholder::Error => {
                        return Err(CommandError::UnknownPlaceholder(name.to_owned()));
                    }
                    UnknownPlaceholder::Keep => out.push(&rest[start..start + 3 + end]),
                    UnknownPlaceholder::Empty => {}
                },
            }
            rest = &after[end + 1..];
        }
        out.push(rest);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::sync::Arc;

    use super::{
        BUILTIN_PLACEHOLDERS, PlaceholderResolver, UnknownPlaceholder, Values,
        is_builtin_placeholder, placeholder_names,
    };
    use crate::CommandError;

    struct Brand;

    impl PlaceholderResolver for Brand {
        fn resolve(&self, name: &str) -> Option<OsString> {
            (name == "acme_brand").then(|| "acme".into())
        }
    }

    #[test]
    fn substitutes_with_table_resolvers_and_policy() {
        let resolvers: Vec<Arc<dyn PlaceholderResolver>> = vec![Arc::new(Brand)];
        let mut values = Values {
            table: BTreeMap::from([("version_name", OsString::from("1.20.1"))]),
            resolvers: &resolvers,
            unknown: UnknownPlaceholder::Error,
        };
        assert_eq!(
            values
                .substitute("-Dv=${version_name}-${acme_brand}")
                .unwrap(),
            "-Dv=1.20.1-acme"
        );
        assert_eq!(
            values.substitute("${nope}"),
            Err(CommandError::UnknownPlaceholder("nope".to_owned()))
        );
        assert!(matches!(
            values.substitute("${open"),
            Err(CommandError::Unterminated(_))
        ));
        values.unknown = UnknownPlaceholder::Keep;
        assert_eq!(values.substitute("a${nope}b").unwrap(), "a${nope}b");
        values.unknown = UnknownPlaceholder::Empty;
        assert_eq!(values.substitute("a${nope}b").unwrap(), "ab");
    }

    #[test]
    fn lists_placeholder_names_and_knows_the_builtin_ones() {
        assert!(BUILTIN_PLACEHOLDERS.is_sorted());
        assert!(is_builtin_placeholder("auth_access_token"));
        assert!(is_builtin_placeholder("resolution_width"));
        assert!(!is_builtin_placeholder("acme_port"));
        let names: Vec<_> = placeholder_names("a${x}b${}c${y}${open").collect();
        assert_eq!(names, ["x", "", "y"]);
        assert_eq!(placeholder_names("plain").count(), 0);
    }
}
