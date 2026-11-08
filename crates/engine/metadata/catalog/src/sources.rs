use std::fmt;
use std::sync::Arc;

use coblet_runtime::RegistryError;
use coblet_types::ExtensionId;

use crate::CatalogSource;

/// Ordered list of the sources of a [`Catalog`](crate::Catalog), where
/// every source has its own ID.
///
/// When two sources list the same item, the one earlier in the list wins,
/// so the order is part of the configuration: a source registered later
/// only fills gaps, and [`replace`](Self::replace) keeps the place of the
/// source it swaps out.
///
/// # Examples
///
/// ```
/// # use std::sync::Arc;
/// # use coblet_catalog::{CatalogContext, CatalogError, CatalogPage, CatalogQuery, CatalogSource, CatalogSources};
/// # use coblet_runtime::{BoxFuture, Extension};
/// # use coblet_types::ExtensionId;
/// # struct Fixed(ExtensionId);
/// # impl Extension for Fixed {
/// #     fn id(&self) -> &ExtensionId { &self.0 }
/// #     fn display_name(&self) -> &'static str { "Fixed" }
/// # }
/// # impl CatalogSource<String> for Fixed {
/// #     fn fetch<'a>(&'a self, _: &'a CatalogContext, _: &'a CatalogQuery)
/// #         -> BoxFuture<'a, Result<CatalogPage<String>, CatalogError>> {
/// #         Box::pin(async { Ok(CatalogPage::new(Vec::new())) })
/// #     }
/// # }
/// let first = ExtensionId::new("acme:first")?;
/// let mut sources = CatalogSources::<String>::new();
/// sources.register(Arc::new(Fixed(first.clone())))?;
/// assert!(sources.register(Arc::new(Fixed(first.clone()))).is_err());
/// assert!(sources.remove(&first).is_some());
/// assert!(sources.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct CatalogSources<Item> {
    entries: Vec<Arc<dyn CatalogSource<Item>>>,
}

impl<Item: 'static> CatalogSources<Item> {
    /// Creates an empty list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Appends a source.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when a source already has its ID.
    pub fn register(&mut self, source: Arc<dyn CatalogSource<Item>>) -> Result<(), RegistryError> {
        if self.get(source.id()).is_some() {
            return Err(RegistryError::Duplicate(source.id().clone()));
        }
        self.entries.push(source);
        Ok(())
    }

    /// Puts a source in place of the one with the same ID, keeping its
    /// position, or appends it; returns the source it replaced.
    pub fn replace(
        &mut self,
        source: Arc<dyn CatalogSource<Item>>,
    ) -> Option<Arc<dyn CatalogSource<Item>>> {
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|known| known.id() == source.id())
        {
            return Some(std::mem::replace(slot, source));
        }
        self.entries.push(source);
        None
    }

    /// Removes and returns the source with this ID.
    pub fn remove(&mut self, id: &ExtensionId) -> Option<Arc<dyn CatalogSource<Item>>> {
        let index = self.entries.iter().position(|known| known.id() == id)?;
        Some(self.entries.remove(index))
    }

    /// Returns the source with this ID.
    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&Arc<dyn CatalogSource<Item>>> {
        self.entries.iter().find(|known| known.id() == id)
    }

    /// Iterates over the sources in order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Arc<dyn CatalogSource<Item>>> {
        self.entries.iter()
    }

    /// Returns the number of sources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<Item: 'static> Default for CatalogSources<Item> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Item> Clone for CatalogSources<Item> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
        }
    }
}

impl<Item: 'static> fmt::Debug for CatalogSources<Item> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|source| source.id().as_str()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_runtime::{BoxFuture, Extension};
    use coblet_types::ExtensionId;

    use super::CatalogSources;
    use crate::{CatalogContext, CatalogError, CatalogPage, CatalogQuery, CatalogSource};

    struct Named(ExtensionId, &'static str);

    impl Extension for Named {
        fn id(&self) -> &ExtensionId {
            &self.0
        }

        fn display_name(&self) -> &str {
            self.1
        }
    }

    impl CatalogSource<String> for Named {
        fn fetch<'a>(
            &'a self,
            _cx: &'a CatalogContext,
            _query: &'a CatalogQuery,
        ) -> BoxFuture<'a, Result<CatalogPage<String>, CatalogError>> {
            Box::pin(async { Ok(CatalogPage::new(Vec::new())) })
        }
    }

    fn named(id: &str, name: &'static str) -> Arc<dyn CatalogSource<String>> {
        Arc::new(Named(ExtensionId::new(id).unwrap(), name))
    }

    fn names(sources: &CatalogSources<String>) -> Vec<&str> {
        sources.iter().map(|source| source.display_name()).collect()
    }

    #[test]
    fn keeps_order_through_replace_and_remove() {
        let mut sources = CatalogSources::new();
        sources.register(named("acme:a", "A")).unwrap();
        sources.register(named("acme:b", "B")).unwrap();
        sources.register(named("acme:c", "C")).unwrap();
        assert!(sources.register(named("acme:b", "B2")).is_err());

        assert!(sources.replace(named("acme:b", "B2")).is_some());
        assert!(sources.replace(named("acme:d", "D")).is_none());
        assert_eq!(names(&sources), ["A", "B2", "C", "D"]);

        assert!(
            sources
                .remove(&ExtensionId::new("acme:a").unwrap())
                .is_some()
        );
        assert!(
            sources
                .remove(&ExtensionId::new("acme:a").unwrap())
                .is_none()
        );
        assert_eq!(names(&sources), ["B2", "C", "D"]);
        assert_eq!(sources.len(), 3);
        assert_eq!(format!("{sources:?}"), r#"["acme:b", "acme:c", "acme:d"]"#);
    }
}
