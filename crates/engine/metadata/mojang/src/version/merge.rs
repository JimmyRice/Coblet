use coblet_types::GameVersionId;

use super::{Arguments, Downloads, RawVersionJson};

/// Reason an `inheritsFrom` chain could not be resolved.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InheritError {
    /// A parent version is not available.
    #[error("version `{child}` inherits from `{parent}`, which is not available")]
    Missing {
        /// The version that names the parent.
        child: GameVersionId,
        /// The missing parent.
        parent: GameVersionId,
    },
    /// The chain refers back to a version already in it.
    #[error("inheritsFrom chain of `{0}` contains a cycle")]
    Cycle(GameVersionId),
}

impl RawVersionJson {
    /// Merges this file onto the version it inherits from.
    ///
    /// The child's ID, main class, legacy arguments and other single values
    /// replace the parent's; its libraries come before the parent's; its
    /// modern arguments follow the parent's. The result inherits from the
    /// parent's parent.
    #[must_use]
    pub fn inherit(self, parent: Self) -> Self {
        let arguments = match (parent.arguments, self.arguments) {
            (Some(parent), Some(child)) => Some(Arguments {
                game: parent.game.into_iter().chain(child.game).collect(),
                jvm: parent.jvm.into_iter().chain(child.jvm).collect(),
            }),
            (parent, child) => child.or(parent),
        };
        let mut extra = parent.extra;
        extra.extend(self.extra);
        let mut other = parent.downloads.other;
        other.extend(self.downloads.other);
        Self {
            id: self.id,
            version_type: self.version_type.or(parent.version_type),
            inherits_from: parent.inherits_from,
            main_class: self.main_class.or(parent.main_class),
            arguments,
            minecraft_arguments: self.minecraft_arguments.or(parent.minecraft_arguments),
            libraries: self.libraries.into_iter().chain(parent.libraries).collect(),
            asset_index: self.asset_index.or(parent.asset_index),
            assets: self.assets.or(parent.assets),
            downloads: Downloads {
                client: self.downloads.client.or(parent.downloads.client),
                server: self.downloads.server.or(parent.downloads.server),
                client_mappings: self
                    .downloads
                    .client_mappings
                    .or(parent.downloads.client_mappings),
                server_mappings: self
                    .downloads
                    .server_mappings
                    .or(parent.downloads.server_mappings),
                other,
            },
            java_version: self.java_version.or(parent.java_version),
            logging: self.logging.or(parent.logging),
            release_time: self.release_time.or(parent.release_time),
            time: self.time.or(parent.time),
            compliance_level: self.compliance_level.or(parent.compliance_level),
            minimum_launcher_version: self
                .minimum_launcher_version
                .max(parent.minimum_launcher_version),
            extra,
        }
    }

    /// Follows the `inheritsFrom` chain, loading each parent with `load`, and merges it.
    ///
    /// # Errors
    ///
    /// Returns [`InheritError`] when a parent is missing or the chain has a cycle.
    pub fn resolve_inheritance(
        self,
        mut load: impl FnMut(&GameVersionId) -> Option<Self>,
    ) -> Result<Self, InheritError> {
        let mut seen = vec![self.id.clone()];
        let mut current = self;
        while let Some(parent_id) = current.inherits_from.clone() {
            if seen.contains(&parent_id) {
                return Err(InheritError::Cycle(current.id));
            }
            let parent = load(&parent_id).ok_or_else(|| InheritError::Missing {
                child: current.id.clone(),
                parent: parent_id.clone(),
            })?;
            seen.push(parent_id);
            current = current.inherit(parent);
        }
        Ok(current)
    }
}

#[cfg(test)]
#[path = "../../tests/version/merge.rs"]
mod tests;
