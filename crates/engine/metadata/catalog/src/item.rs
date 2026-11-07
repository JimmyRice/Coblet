use coblet_types::{ExtensionId, GameVersionId, VersionType};

use crate::Timestamp;

/// Entry of a [`Catalog`](crate::Catalog): a game version, a Mod Loader version or a server build.
///
/// Items never compare version strings. Catalogs order them by
/// [`released`](CatalogItem::released) and keep source order for ties.
pub trait CatalogItem: Clone + Send + Sync + 'static {
    /// Returns the key that identifies the item across sources; the first
    /// source to return a key wins.
    fn key(&self) -> &str;

    /// Returns the release channel, such as [`VersionType::Release`].
    fn version_type(&self) -> &VersionType;

    /// Returns the release time, when the source knows it.
    fn released(&self) -> Option<Timestamp>;

    /// Reports whether the item is the source's recommendation (the star in a version list).
    fn is_recommended(&self) -> bool {
        false
    }

    /// Reports whether the item works with a game version; `true` when it
    /// does not depend on one, which is the default.
    fn is_compatible_with(&self, game: &GameVersionId) -> bool {
        let _ = game;
        true
    }

    /// Returns text matched by [`Filter::text`]; the key by default.
    fn search_text(&self) -> &str {
        self.key()
    }

    /// Records the source that listed the item; the catalog calls it for
    /// every item it receives. Does nothing by default.
    fn listed_by(&mut self, source: &ExtensionId) {
        let _ = source;
    }
}

/// Conditions an item must meet to be listed.
///
/// Every condition left unset matches everything.
///
/// # Examples
///
/// ```
/// use coblet_catalog::{Filter, Timestamp};
/// use coblet_types::VersionType;
///
/// let filter = Filter::new()
///     .with_types([VersionType::Release, VersionType::OldBeta])
///     .with_text("1.20")
///     .released_after("2020-01-01T00:00:00Z".parse()?);
/// assert_eq!(filter.types.len(), 2);
/// # Ok::<(), coblet_catalog::TimestampError>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Allowed release channels; empty allows all.
    pub types: Vec<VersionType>,
    /// Case-insensitive substring of [`CatalogItem::search_text`].
    pub text: Option<String>,
    /// Earliest release time, inclusive. Items without a time are excluded when set.
    pub released_after: Option<Timestamp>,
    /// Latest release time, inclusive. Items without a time are excluded when set.
    pub released_before: Option<Timestamp>,
    /// Game version the item must work with; also passed to sources in [`CatalogQuery::game`].
    pub game: Option<GameVersionId>,
}

impl Filter {
    /// Creates a filter that matches everything.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allows only these release channels.
    #[must_use]
    pub fn with_types(self, types: impl IntoIterator<Item = VersionType>) -> Self {
        Self {
            types: types.into_iter().collect(),
            ..self
        }
    }

    /// Requires the search text to contain `text`, ignoring case.
    #[must_use]
    pub fn with_text(self, text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..self
        }
    }

    /// Requires a release time at or after `time`.
    #[must_use]
    pub fn released_after(self, time: Timestamp) -> Self {
        Self {
            released_after: Some(time),
            ..self
        }
    }

    /// Requires a release time at or before `time`.
    #[must_use]
    pub fn released_before(self, time: Timestamp) -> Self {
        Self {
            released_before: Some(time),
            ..self
        }
    }

    /// Requires compatibility with a game version.
    #[must_use]
    pub fn compatible_with(self, game: GameVersionId) -> Self {
        Self {
            game: Some(game),
            ..self
        }
    }

    /// Reports whether an item meets every condition.
    #[must_use]
    pub fn matches<Item: CatalogItem>(&self, item: &Item) -> bool {
        if !self.types.is_empty() && !self.types.contains(item.version_type()) {
            return false;
        }
        if let Some(text) = &self.text {
            let haystack = item.search_text().to_lowercase();
            if !haystack.contains(&text.to_lowercase()) {
                return false;
            }
        }
        if self.released_after.is_some() || self.released_before.is_some() {
            let Some(released) = item.released() else {
                return false;
            };
            if self.released_after.is_some_and(|after| released < after)
                || self.released_before.is_some_and(|before| released > before)
            {
                return false;
            }
        }
        self.game
            .as_ref()
            .is_none_or(|game| item.is_compatible_with(game))
    }
}

/// What a [`CatalogSource`](crate::CatalogSource) is asked for.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct CatalogQuery {
    /// Game version the list is for, used by Mod Loader catalogs; `None` for game versions.
    pub game: Option<GameVersionId>,
}

impl CatalogQuery {
    /// Asks for every item.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Asks for the items of one game version.
    #[must_use]
    pub fn for_game(game: GameVersionId) -> Self {
        Self { game: Some(game) }
    }
}

/// Items returned by one source, in the source's own order.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPage<Item> {
    /// The items.
    pub items: Vec<Item>,
}

impl<Item> CatalogPage<Item> {
    /// Wraps a list of items.
    #[must_use]
    pub fn new(items: Vec<Item>) -> Self {
        Self { items }
    }
}
