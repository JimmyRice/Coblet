use std::fmt;

use coblet_runtime::{BoxFuture, Extension};
use coblet_types::ExtensionId;

use super::{TraitContext, TraitError, TraitSource, TraitTarget};

/// Trait source made from a closure that looks at one version at a time.
///
/// For rules that need neither the network nor the whole batch; write a
/// [`TraitSource`] for those.
///
/// # Examples
///
/// ```
/// use coblet_mojang::{FnTraitSource, VersionTrait};
/// use coblet_types::ExtensionId;
///
/// let snapshots = FnTraitSource::new(
///     ExtensionId::new("acme:snapshots")?,
///     "Snapshot marker",
///     |target| {
///         if target.input().id.as_str().contains('w') {
///             target.traits_mut().insert(VersionTrait::from("acme:snapshot"));
///         }
///         Ok(())
///     },
/// );
/// # drop(snapshots);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct FnTraitSource<F> {
    id: ExtensionId,
    name: String,
    adjust: F,
}

impl<F> FnTraitSource<F>
where
    F: Fn(&mut TraitTarget) -> Result<(), TraitError> + Send + Sync + 'static,
{
    /// Creates a source that calls `adjust` for every version of a batch.
    #[must_use]
    pub fn new(id: ExtensionId, name: impl Into<String>, adjust: F) -> Self {
        Self {
            id,
            name: name.into(),
            adjust,
        }
    }
}

impl<F> fmt::Debug for FnTraitSource<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnTraitSource")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl<F: Send + Sync + 'static> Extension for FnTraitSource<F> {
    fn id(&self) -> &ExtensionId {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

impl<F> TraitSource for FnTraitSource<F>
where
    F: Fn(&mut TraitTarget) -> Result<(), TraitError> + Send + Sync + 'static,
{
    fn adjust<'a>(
        &'a self,
        _cx: &'a TraitContext<'a>,
        versions: &'a mut [TraitTarget],
    ) -> BoxFuture<'a, Result<(), TraitError>> {
        Box::pin(async move { versions.iter_mut().try_for_each(&self.adjust) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use coblet_catalog::{CachePolicy, CatalogContext, MetaCache};
    use coblet_runtime::TokioStorage;
    use coblet_types::{ExtensionId, GameVersionId};

    use super::FnTraitSource;
    use crate::traits::testing::NoTransport;
    use crate::{TraitError, TraitInput, TraitSources, TraitTarget, VersionTrait, VersionTraits};

    #[tokio::test]
    async fn calls_the_closure_per_version_and_fails_as_a_whole() {
        let dir = tempfile::tempdir().unwrap();
        let meta = MetaCache::new(
            Arc::new(NoTransport),
            Arc::new(TokioStorage::new()),
            dir.path(),
        );
        let cx = CatalogContext::new(meta, CachePolicy::Revalidate);
        let mut sources = TraitSources::new();
        sources
            .register(Arc::new(FnTraitSource::new(
                ExtensionId::new("test:closure").unwrap(),
                "closure",
                |target: &mut TraitTarget| {
                    if target.input().id.as_str() == "bad" {
                        return Err(TraitError::failed("bad version"));
                    }
                    target.traits_mut().insert(VersionTrait::from("seen"));
                    Ok(())
                },
            )))
            .unwrap();
        let target = |id: &str| {
            TraitTarget::new(
                TraitInput::new(GameVersionId::new(id).unwrap()),
                VersionTraits::new(),
            )
        };
        let mut good = [target("a"), target("b")];
        assert!(sources.run(&cx, &mut good).await.is_empty());
        assert!(good.iter().all(|t| t.traits().len() == 1));
        let mut mixed = [target("a"), target("bad")];
        let failures = sources.run(&cx, &mut mixed).await;
        assert_eq!(failures.len(), 1);
        assert!(mixed.iter().all(|t| t.traits().is_empty()));
    }
}
