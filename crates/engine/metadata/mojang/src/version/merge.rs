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
mod tests {
    use super::InheritError;
    use crate::RawVersionJson;

    fn parse(json: &str) -> RawVersionJson {
        RawVersionJson::from_slice(json.as_bytes()).unwrap()
    }

    #[test]
    fn merges_a_loader_patch_onto_its_parent() {
        let parent = parse(
            r#"{"id":"1.20.1","type":"release","mainClass":"net.minecraft.client.main.Main",
            "arguments":{"game":["--username","${auth_player_name}"],"jvm":["-cp","${classpath}"]},
            "libraries":[{"name":"com.mojang:brigadier:1.1.8"}],
            "javaVersion":{"component":"java-runtime-gamma","majorVersion":17},"assets":"5"}"#,
        );
        let child = parse(
            r#"{"id":"fabric-loader-0.16.14-1.20.1","inheritsFrom":"1.20.1",
            "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
            "arguments":{"game":[],"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},
            "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.14","url":"https://maven.fabricmc.net/"}]}"#,
        );
        let merged = child
            .resolve_inheritance(|id| (id.as_str() == "1.20.1").then(|| parent.clone()))
            .unwrap();
        assert_eq!(merged.id.as_str(), "fabric-loader-0.16.14-1.20.1");
        assert_eq!(merged.inherits_from, None);
        assert_eq!(
            merged.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        let names: Vec<String> = merged
            .libraries
            .iter()
            .map(|library| library.name.to_string())
            .collect();
        assert_eq!(
            names,
            [
                "net.fabricmc:fabric-loader:0.16.14",
                "com.mojang:brigadier:1.1.8"
            ]
        );
        let arguments = merged.arguments.unwrap();
        assert_eq!(arguments.jvm.len(), 3);
        assert_eq!(arguments.game.len(), 2);
        assert_eq!(merged.java_version.unwrap().major_version, 17);
        assert_eq!(merged.assets.as_deref(), Some("5"));
    }

    #[test]
    fn reports_missing_parents_and_cycles() {
        let a = parse(r#"{"id":"a","inheritsFrom":"b"}"#);
        let b = parse(r#"{"id":"b","inheritsFrom":"a"}"#);
        assert!(matches!(
            a.clone().resolve_inheritance(|_| None),
            Err(InheritError::Missing { .. })
        ));
        assert!(matches!(
            a.resolve_inheritance(|_| Some(b.clone())),
            Err(InheritError::Cycle(_))
        ));
    }
}
