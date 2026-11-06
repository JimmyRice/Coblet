use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;

use crate::CommandError;

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
#[path = "../tests/placeholder.rs"]
mod tests;
