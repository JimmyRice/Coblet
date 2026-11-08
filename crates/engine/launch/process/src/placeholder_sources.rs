use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use coblet_launch_command::placeholder_names;
use coblet_mojang::VersionTraits;
use coblet_runtime::{BoxFuture, Extension, RegistryError};
use coblet_types::{ExtensionId, GameVersionId, Platform};

use crate::{
    GameLayout, LaunchContext, LaunchError, PlaceholderOrigin, check_placeholder_name,
    command_profile,
};

/// Supplies values for placeholders that nothing else in a launch set,
/// such as `${acme_server}` in an argument a profile or customizer added.
///
/// The `resolve-placeholders` step asks sources in order, each only for
/// the names still missing, and stops once none are. A source answers the
/// names it knows and leaves out the others.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use coblet_launch::{LaunchError, PlaceholderCtx, PlaceholderSource};
/// use coblet_runtime::{BoxFuture, Extension};
/// use coblet_types::ExtensionId;
///
/// struct Brand(ExtensionId);
///
/// impl Extension for Brand {
///     fn id(&self) -> &ExtensionId { &self.0 }
///     fn display_name(&self) -> &str { "Brand" }
/// }
///
/// impl PlaceholderSource for Brand {
///     fn resolve<'a>(
///         &'a self,
///         names: &'a [String],
///         cx: &'a PlaceholderCtx,
///     ) -> BoxFuture<'a, Result<BTreeMap<String, String>, LaunchError>> {
///         let known = names
///             .iter()
///             .filter(|name| *name == "acme_brand")
///             .map(|name| (name.clone(), format!("acme-{}", cx.game_version)))
///             .collect();
///         Box::pin(async move { Ok(known) })
///     }
/// }
/// ```
pub trait PlaceholderSource: Extension {
    /// Returns values for those of `names` the source knows.
    ///
    /// # Errors
    ///
    /// Returns an error to stop the launch; it reaches the caller as
    /// [`LaunchError::PlaceholderSource`].
    fn resolve<'a>(
        &'a self,
        names: &'a [String],
        cx: &'a PlaceholderCtx,
    ) -> BoxFuture<'a, Result<BTreeMap<String, String>, LaunchError>>;
}

/// What a [`PlaceholderSource`] can see of the launch.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct PlaceholderCtx {
    /// The game version.
    pub game_version: GameVersionId,
    /// Its traits as the profile pipeline and customizers left them.
    pub traits: VersionTraits,
    /// The machine the game runs on.
    pub platform: Platform,
    /// File locations of this launch.
    pub layout: GameLayout,
    /// Major version of the chosen Java, once `select-java` ran.
    pub java_major: Option<u32>,
}

impl PlaceholderCtx {
    /// Describes the launch in `cx`.
    #[must_use]
    pub fn new(cx: &LaunchContext) -> Self {
        Self {
            game_version: cx.profile.game_version.clone(),
            traits: cx.profile.traits.clone(),
            platform: cx.profile.platform.clone(),
            layout: cx.layout.clone(),
            java_major: cx.java.as_ref().map(|java| java.major),
        }
    }
}

/// Ordered list of [`PlaceholderSource`]s; none by default.
#[derive(Clone, Default)]
pub struct PlaceholderSources {
    entries: Vec<Arc<dyn PlaceholderSource>>,
}

impl fmt::Debug for PlaceholderSources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|source| source.id()))
            .finish()
    }
}

impl PlaceholderSources {
    /// Creates an empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a source.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Duplicate`] when another source has the same ID.
    pub fn register(&mut self, source: Arc<dyn PlaceholderSource>) -> Result<(), RegistryError> {
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
        source: Arc<dyn PlaceholderSource>,
    ) -> Option<Arc<dyn PlaceholderSource>> {
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
    pub fn remove(&mut self, id: &ExtensionId) -> Option<Arc<dyn PlaceholderSource>> {
        let index = self.entries.iter().position(|known| known.id() == id)?;
        Some(self.entries.remove(index))
    }

    /// Returns a source by ID.
    #[must_use]
    pub fn get(&self, id: &ExtensionId) -> Option<&Arc<dyn PlaceholderSource>> {
        self.entries.iter().find(|known| known.id() == id)
    }

    /// Iterates over the sources in the order they are asked.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Arc<dyn PlaceholderSource>> {
        self.entries.iter()
    }
}

/// Returns the placeholders the arguments of this launch use that have no
/// value yet and that coblet does not fill itself, in order.
pub(crate) fn missing_placeholders(cx: &LaunchContext) -> Vec<String> {
    let profile = command_profile(&cx.profile, &cx.layout);
    let templates = profile
        .jvm_args
        .iter()
        .chain(&profile.game_args)
        .map(|arg| arg.value.as_str())
        .chain(cx.jvm_args.iter().map(String::as_str))
        .chain(cx.request.jvm_args.iter().map(String::as_str))
        .chain(cx.request.game_args.iter().map(String::as_str));
    let mut seen = BTreeSet::new();
    let mut missing = Vec::new();
    for template in templates {
        for name in placeholder_names(template) {
            if check_placeholder_name(name).is_ok()
                && cx.placeholders.get(name).is_none()
                && seen.insert(name)
            {
                missing.push(name.to_owned());
            }
        }
    }
    missing
}

/// Asks the sources for the missing placeholders and records their answers.
pub(crate) async fn resolve_placeholders(cx: &mut LaunchContext) -> Result<(), LaunchError> {
    let mut missing = missing_placeholders(cx);
    if missing.is_empty() || cx.services.placeholder_sources.iter().len() == 0 {
        return Ok(());
    }
    let sources = cx.services.placeholder_sources.clone();
    let info = PlaceholderCtx::new(cx);
    for source in sources.iter() {
        if missing.is_empty() {
            break;
        }
        let answers = source.resolve(&missing, &info).await.map_err(|error| {
            LaunchError::PlaceholderSource {
                source_id: source.id().clone(),
                message: error.to_string(),
            }
        })?;
        for (name, value) in answers {
            if !missing.contains(&name) {
                tracing::debug!(source = %source.id(), %name, "ignoring a placeholder nobody asked for");
                continue;
            }
            cx.placeholders
                .set(&name, value, PlaceholderOrigin::Source(source.id().clone()))?;
        }
        missing.retain(|name| cx.placeholders.get(name).is_none());
    }
    Ok(())
}
