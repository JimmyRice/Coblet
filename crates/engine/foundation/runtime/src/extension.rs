use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use coblet_types::ExtensionId;

/// Common surface of every registrable extension: Mod Loaders, server
/// distributions, content Providers, pack formats, Instance Backends,
/// Platform Profiles and auth methods.
///
/// Built-in and third-party extensions implement the same traits; the only
/// difference is the namespace of their [`ExtensionId`].
pub trait Extension: Send + Sync + 'static {
    /// Returns the registry key, such as `coblet:fabric`.
    fn id(&self) -> &ExtensionId;

    /// Returns the name shown to users, such as `Fabric`.
    fn display_name(&self) -> &str;
}

/// Set of extensions of one kind, keyed and ordered by [`ExtensionId`].
///
/// `E` is usually a trait object such as `dyn ModLoader`. Iteration follows
/// ID order, so listings are stable across runs.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_runtime::{Extension, Registry};
/// use coblet_types::ExtensionId;
///
/// trait Greeter: Extension {}
///
/// struct Hello(ExtensionId);
///
/// impl Extension for Hello {
///     fn id(&self) -> &ExtensionId { &self.0 }
///     fn display_name(&self) -> &str { "Hello" }
/// }
/// impl Greeter for Hello {}
///
/// let mut greeters: Registry<dyn Greeter> = Registry::new();
/// greeters.register(Arc::new(Hello(ExtensionId::new("acme:hello")?)))?;
/// assert!(greeters.register(Arc::new(Hello(ExtensionId::new("acme:hello")?))).is_err());
/// assert_eq!(greeters.len(), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Registry<E: ?Sized> {
    entries: BTreeMap<ExtensionId, Arc<E>>,
}

/// Reason an extension could not be registered.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Another extension already uses this ID.
    #[error("extension `{0}` is already registered")]
    Duplicate(ExtensionId),
}

impl<E: ?Sized + Extension> Registry<E> {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Adds an extension under its own ID.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when the ID is taken; the existing
    /// entry stays in place.
    pub fn register(&mut self, extension: Arc<E>) -> Result<(), RegistryError> {
        let id = extension.id().clone();
        if self.entries.contains_key(&id) {
            return Err(RegistryError::Duplicate(id));
        }
        self.entries.insert(id, extension);
        Ok(())
    }

    /// Looks up an extension by ID.
    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&Arc<E>> {
        self.entries.get(id)
    }

    /// Iterates over all extensions in ID order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Arc<E>> {
        self.entries.values()
    }

    /// Returns the number of registered extensions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<E: ?Sized + Extension> Default for Registry<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: ?Sized> Clone for Registry<E> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
        }
    }
}

impl<E: ?Sized> fmt::Debug for Registry<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.entries.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_types::ExtensionId;

    use super::{Extension, Registry, RegistryError};

    trait Loader: Extension {
        fn kind(&self) -> &'static str;
    }

    struct Fake {
        id: ExtensionId,
        kind: &'static str,
    }

    impl Extension for Fake {
        fn id(&self) -> &ExtensionId {
            &self.id
        }

        fn display_name(&self) -> &str {
            self.kind
        }
    }

    impl Loader for Fake {
        fn kind(&self) -> &'static str {
            self.kind
        }
    }

    fn fake(id: &str, kind: &'static str) -> Arc<dyn Loader> {
        Arc::new(Fake {
            id: ExtensionId::new(id).unwrap(),
            kind,
        })
    }

    #[test]
    fn rejects_duplicates_and_keeps_the_first() {
        let mut loaders: Registry<dyn Loader> = Registry::new();
        loaders.register(fake("coblet:fabric", "builtin")).unwrap();
        let id = ExtensionId::new("coblet:fabric").unwrap();
        assert_eq!(
            loaders.register(fake("coblet:fabric", "impostor")),
            Err(RegistryError::Duplicate(id.clone()))
        );
        assert_eq!(loaders.get(&id).unwrap().kind(), "builtin");
    }

    #[test]
    fn iterates_in_id_order() {
        let mut loaders: Registry<dyn Loader> = Registry::default();
        for id in ["coblet:quilt", "acme:legacy", "coblet:fabric"] {
            loaders.register(fake(id, "x")).unwrap();
        }
        let ids: Vec<&str> = loaders.iter().map(|loader| loader.id().as_str()).collect();
        assert_eq!(ids, ["acme:legacy", "coblet:fabric", "coblet:quilt"]);
        assert_eq!(format!("{loaders:?}").matches("ExtensionId").count(), 3);
    }
}
