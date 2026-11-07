use std::fmt;
use std::sync::Arc;

use coblet_catalog::CatalogContext;
use coblet_runtime::{RegistryError, WarningCode};
use coblet_types::ExtensionId;

use super::{TraitContext, TraitFailure, TraitSource, TraitTable, TraitTarget, VersionTraits};

/// Warning code published when a trait source fails and its changes are dropped.
pub const TRAIT_SOURCE_FAILED: WarningCode =
    WarningCode::from_static("version.trait-source-failed");

/// Ordered list of [`TraitSource`]s.
///
/// The built-in list holds the built-in [`TraitTable`], ID
/// `coblet:version-traits`. Sources registered later run later, so they
/// can remove traits earlier ones added. Built-in and third-party sources
/// are equal: either can be replaced in place or removed by ID.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use coblet_mojang::{TraitSources, TraitTable};
///
/// let mut sources = TraitSources::builtin();
/// assert!(sources.register(Arc::new(TraitTable::builtin().clone())).is_err());
/// sources.remove(&"coblet:version-traits".parse()?);
/// assert!(sources.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Default)]
pub struct TraitSources {
    entries: Vec<Arc<dyn TraitSource>>,
}

impl fmt::Debug for TraitSources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|source| source.id()))
            .finish()
    }
}

impl TraitSources {
    /// Creates an empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the list holding the built-in trait table.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            entries: vec![Arc::new(TraitTable::builtin().clone())],
        }
    }

    /// Appends a source.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when another source has the same ID.
    pub fn register(&mut self, source: Arc<dyn TraitSource>) -> Result<(), RegistryError> {
        if self.get(source.id()).is_some() {
            return Err(RegistryError::Duplicate(source.id().clone()));
        }
        self.entries.push(source);
        Ok(())
    }

    /// Puts a source in place of the one with the same ID, keeping its
    /// position, or appends it; returns the source it replaced.
    pub fn replace(&mut self, source: Arc<dyn TraitSource>) -> Option<Arc<dyn TraitSource>> {
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

    /// Removes and returns a source.
    pub fn remove(&mut self, id: &ExtensionId) -> Option<Arc<dyn TraitSource>> {
        let index = self.entries.iter().position(|known| known.id() == id)?;
        Some(self.entries.remove(index))
    }

    /// Returns a source by ID.
    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&Arc<dyn TraitSource>> {
        self.entries.iter().find(|known| known.id() == id)
    }

    /// Iterates over the sources in the order they run.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Arc<dyn TraitSource>> {
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

    /// Runs every source over a batch, in order, and records each change in
    /// the targets.
    ///
    /// A source that fails leaves the batch as it was before that source
    /// ran; its failure is returned and the remaining sources still run.
    pub async fn apply(
        &self,
        catalog: &CatalogContext,
        targets: &mut [TraitTarget],
    ) -> Vec<TraitFailure> {
        let mut failures = Vec::new();
        for source in &self.entries {
            let before: Vec<VersionTraits> = targets
                .iter()
                .map(|target| target.traits().clone())
                .collect();
            let cx = TraitContext::new(catalog, source.id());
            match source.adjust(&cx, targets).await {
                Ok(()) => {
                    for (target, before) in targets.iter_mut().zip(&before) {
                        target.record(source.id(), before);
                    }
                }
                Err(error) => {
                    for (target, before) in targets.iter_mut().zip(before) {
                        *target.traits_mut() = before;
                    }
                    tracing::warn!(source = %source.id(), %error, "trait source failed");
                    failures.push(TraitFailure {
                        source_id: source.id().clone(),
                        error,
                    });
                }
            }
        }
        failures
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_runtime::{BoxFuture, Extension, TokioStorage};
    use coblet_types::{ExtensionId, GameVersionId};

    use super::TraitSources;
    use crate::traits::testing::NoTransport;
    use crate::{
        TraitContext, TraitError, TraitInput, TraitSource, TraitTarget, VersionTrait, VersionTraits,
    };

    /// Adds `add`, removes `remove`, then fails when `fail` is set.
    struct Scripted {
        id: ExtensionId,
        add: &'static str,
        remove: &'static str,
        fail: bool,
    }

    impl Extension for Scripted {
        fn id(&self) -> &ExtensionId {
            &self.id
        }
        fn display_name(&self) -> &'static str {
            "scripted"
        }
    }

    impl TraitSource for Scripted {
        fn adjust<'a>(
            &'a self,
            _: &'a TraitContext<'a>,
            versions: &'a mut [TraitTarget],
        ) -> BoxFuture<'a, Result<(), TraitError>> {
            Box::pin(async move {
                for target in versions.iter_mut() {
                    target.traits_mut().insert(VersionTrait::from(self.add));
                    target.traits_mut().remove(&VersionTrait::from(self.remove));
                }
                if self.fail {
                    return Err(TraitError::failed("broken"));
                }
                Ok(())
            })
        }
    }

    fn scripted(id: &str, add: &'static str, remove: &'static str, fail: bool) -> Arc<Scripted> {
        Arc::new(Scripted {
            id: ExtensionId::new(id).unwrap(),
            add,
            remove,
            fail,
        })
    }

    fn context(dir: &tempfile::TempDir) -> CatalogContext {
        let meta = MetaCache::new(
            Arc::new(NoTransport),
            Arc::new(TokioStorage::new()),
            dir.path(),
        );
        CatalogContext::new(meta, CachePolicy::Revalidate)
    }

    fn target(id: &str, traits: &[&str]) -> TraitTarget {
        TraitTarget::new(
            TraitInput::new(GameVersionId::new(id).unwrap()),
            traits
                .iter()
                .map(|name| VersionTrait::from(*name))
                .collect(),
        )
    }

    fn names(traits: &VersionTraits) -> Vec<&str> {
        traits.iter().map(VersionTrait::as_str).collect()
    }

    #[tokio::test]
    async fn runs_in_order_and_records_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = TraitSources::new();
        sources
            .register(scripted("test:first", "x", "declared", false))
            .unwrap();
        sources
            .register(scripted("test:second", "y", "x", false))
            .unwrap();
        let mut targets = [target("a", &["declared"]), target("b", &[])];
        let failures = sources.apply(&context(&dir), &mut targets).await;
        assert!(failures.is_empty());
        assert_eq!(names(targets[0].traits()), ["y"]);
        let changes: Vec<String> = targets[0]
            .changes()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            changes,
            [
                "removed declared by test:first",
                "added x by test:first",
                "removed x by test:second",
                "added y by test:second"
            ]
        );
    }

    #[tokio::test]
    async fn drops_the_changes_of_a_failed_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = TraitSources::new();
        sources
            .register(scripted("test:broken", "x", "declared", true))
            .unwrap();
        sources
            .register(scripted("test:after", "y", "", false))
            .unwrap();
        let mut targets = [target("a", &["declared"])];
        let failures = sources.apply(&context(&dir), &mut targets).await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].source_id.as_str(), "test:broken");
        assert_eq!(
            failures[0].to_string(),
            "trait source `test:broken` failed: broken"
        );
        assert_eq!(names(targets[0].traits()), ["declared", "y"]);
        assert_eq!(targets[0].changes().len(), 1);
    }

    #[test]
    fn registers_replaces_and_removes_by_id() {
        let mut sources = TraitSources::builtin();
        let builtin = ExtensionId::new("coblet:version-traits").unwrap();
        assert!(sources.get(&builtin).is_some());
        sources
            .register(scripted("test:first", "x", "", false))
            .unwrap();
        assert!(
            sources
                .register(scripted("test:first", "y", "", false))
                .is_err()
        );
        let replaced = sources.replace(scripted("coblet:version-traits", "z", "", false));
        assert!(replaced.is_some());
        assert!(
            sources
                .replace(scripted("test:new", "z", "", false))
                .is_none()
        );
        let ids: Vec<&str> = sources.iter().map(|source| source.id().as_str()).collect();
        assert_eq!(ids, ["coblet:version-traits", "test:first", "test:new"]);
        assert!(sources.remove(&builtin).is_some());
        assert!(sources.remove(&builtin).is_none());
        assert_eq!(sources.len(), 2);
    }
}
