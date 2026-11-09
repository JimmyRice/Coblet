use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::sync::Arc;

use coblet_launch_command::{PlaceholderResolver, UnknownPlaceholder, is_builtin_placeholder};
use coblet_types::ExtensionId;

use crate::LaunchError;

/// Who gave a placeholder its value in one launch.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceholderOrigin {
    /// A value set on the launcher for every launch.
    Static,
    /// The launch request.
    Request,
    /// A launch customizer.
    Customizer(ExtensionId),
    /// A launch step, by name.
    Step(String),
    /// A placeholder source.
    Source(ExtensionId),
}

impl fmt::Display for PlaceholderOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static => f.write_str("static"),
            Self::Request => f.write_str("request"),
            Self::Customizer(id) => write!(f, "customizer {id}"),
            Self::Step(name) => write!(f, "step {name}"),
            Self::Source(id) => write!(f, "source {id}"),
        }
    }
}

/// Checks that `name` can be a placeholder coblet does not fill itself.
///
/// # Errors
///
/// Returns [`LaunchError::InvalidPlaceholder`] when `name` is empty,
/// contains `$`, `{` or `}`, or is a built-in placeholder such as
/// `auth_access_token`.
pub fn check_placeholder_name(name: &str) -> Result<(), LaunchError> {
    let reason = if name.is_empty() {
        "is empty"
    } else if name.contains(['$', '{', '}']) {
        "contains `$`, `{` or `}`"
    } else if is_builtin_placeholder(name) {
        "is filled by coblet"
    } else {
        return Ok(());
    };
    Err(LaunchError::InvalidPlaceholder {
        name: name.to_owned(),
        reason,
    })
}

/// Values of the placeholders coblet does not fill itself, for one launch.
///
/// Values set later replace earlier ones. The launcher's static values go
/// in first, then the request's, then those of customizers and steps;
/// placeholder sources only fill names nothing else set. The built-in
/// placeholders always win and cannot be set here.
#[derive(Clone, Debug, Default)]
pub struct Placeholders {
    values: BTreeMap<String, (String, PlaceholderOrigin)>,
    unknown: UnknownPlaceholder,
}

impl Placeholders {
    /// Creates an empty set that fails on unknown placeholders.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `${name}` to `value`.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::InvalidPlaceholder`] for names
    /// [`check_placeholder_name`] rejects.
    pub fn set(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        origin: PlaceholderOrigin,
    ) -> Result<(), LaunchError> {
        let name = name.into();
        check_placeholder_name(&name)?;
        self.values.insert(name, (value.into(), origin));
        Ok(())
    }

    /// Returns the value of `${name}`, when set.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|(value, _)| value.as_str())
    }

    /// Returns who set `${name}`, when set.
    #[must_use]
    pub fn origin(&self, name: &str) -> Option<&PlaceholderOrigin> {
        self.values.get(name).map(|(_, origin)| origin)
    }

    /// Returns the names that have a value, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    /// Returns what happens to placeholders nothing fills.
    #[must_use]
    pub fn unknown(&self) -> UnknownPlaceholder {
        self.unknown
    }

    /// Sets what happens to placeholders nothing fills.
    pub fn set_unknown(&mut self, policy: UnknownPlaceholder) {
        self.unknown = policy;
    }

    /// Describes where each value came from, one line per placeholder,
    /// without the values, which may be secrets.
    #[must_use]
    pub fn explain(&self) -> String {
        let mut out = String::new();
        for (name, (_, origin)) in &self.values {
            let _ = writeln!(out, "placeholder {name} from {origin}");
        }
        out
    }

    pub(crate) fn resolver(&self) -> Arc<dyn PlaceholderResolver> {
        Arc::new(Values(
            self.values
                .iter()
                .map(|(name, (value, _))| (name.clone(), OsString::from(value)))
                .collect(),
        ))
    }
}

struct Values(BTreeMap<String, OsString>);

impl PlaceholderResolver for Values {
    fn resolve(&self, name: &str) -> Option<OsString> {
        self.0.get(name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use coblet_types::ExtensionId;

    use super::{PlaceholderOrigin, Placeholders, check_placeholder_name};
    use crate::LaunchError;

    #[test]
    fn later_values_win_and_builtin_names_are_refused() {
        let mut values = Placeholders::new();
        values
            .set("acme_brand", "static", PlaceholderOrigin::Static)
            .unwrap();
        values
            .set("acme_brand", "asked", PlaceholderOrigin::Request)
            .unwrap();
        let id = ExtensionId::new("acme:proxy").unwrap();
        values
            .set("acme_port", "25565", PlaceholderOrigin::Customizer(id))
            .unwrap();
        assert_eq!(values.get("acme_brand"), Some("asked"));
        assert_eq!(
            values.origin("acme_brand"),
            Some(&PlaceholderOrigin::Request)
        );
        assert_eq!(
            values.explain(),
            "placeholder acme_brand from request\nplaceholder acme_port from customizer acme:proxy\n"
        );
        for name in ["auth_access_token", "", "a}b", "${x"] {
            assert!(matches!(
                values.set(name, "x", PlaceholderOrigin::Static),
                Err(LaunchError::InvalidPlaceholder { .. })
            ));
        }
        assert!(check_placeholder_name("acme.port-1").is_ok());
        assert_eq!(values.names().count(), 2);
    }
}
