use coblet_mojang::{Library, RawVersionJson};
use coblet_platform::{Component, ComponentPatch, MINECRAFT_UID, Requirement, glob_matches};
use serde::{Deserialize, Serialize};

/// Moves the libraries that match a pattern out of a Mod Loader profile into
/// their own component, such as a mapping layer.
///
/// The rule applies only where the metadata has a matching library, so a
/// loader whose profile lists Intermediary on 1.21.11 gets the extra
/// component there and none on 26.1, where the profile has no mappings.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SplitRule {
    /// Glob on the Maven coordinate, such as `net.fabricmc:intermediary:*`.
    pub library: String,
    /// UID of the component the libraries move to; its version is the library version.
    pub uid: String,
    /// Name shown to users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether the component is only there for the loader; `true` by default.
    #[serde(default = "yes")]
    pub dependency_only: bool,
}

fn yes() -> bool {
    true
}

impl SplitRule {
    /// Creates a rule that moves matching libraries into a dependency-only component.
    #[must_use]
    pub fn new(library: impl Into<String>, uid: impl Into<String>) -> Self {
        Self {
            library: library.into(),
            uid: uid.into(),
            name: None,
            dependency_only: true,
        }
    }

    /// Sets the name shown to users.
    #[must_use]
    pub fn named(self, name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..self
        }
    }

    fn matches(&self, library: &Library) -> bool {
        glob_matches(&self.library, &library.name.to_string())
    }
}

/// Turns a Mod Loader profile in the `client.json` format, as Fabric-style
/// metadata servers return it, into components.
///
/// The profile's main class, libraries and arguments become one component
/// with [`uid`](VersionJsonImport::uid). Libraries matched by a
/// [`SplitRule`] become components of their own, placed before it and
/// required by it. When the profile names its game version in
/// `inheritsFrom`, the split components, or the loader itself when nothing
/// was split, require `net.minecraft` at that version.
///
/// # Examples
///
/// ```
/// use coblet_loader::kit::{SplitRule, VersionJsonImport};
/// use coblet_mojang::RawVersionJson;
///
/// let raw = RawVersionJson::from_slice(br#"{
///     "id": "fabric-loader-0.16.9-1.20.1",
///     "inheritsFrom": "1.20.1",
///     "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
///     "libraries": [
///         { "name": "net.fabricmc:intermediary:1.20.1", "url": "https://maven.fabricmc.net/" },
///         { "name": "net.fabricmc:fabric-loader:0.16.9", "url": "https://maven.fabricmc.net/" }
///     ]
/// }"#)?;
/// let import = VersionJsonImport::new("net.fabricmc.fabric-loader", "0.16.9")
///     .with_split(SplitRule::new("net.fabricmc:intermediary:*", "net.fabricmc.intermediary"));
/// let components = import.components(raw);
/// assert_eq!(components[0].uid(), "net.fabricmc.intermediary");
/// assert_eq!(components[0].version(), "1.20.1");
/// assert_eq!(components[1].patch.libraries.len(), 1);
/// assert_eq!(components[1].patch.requires[0].uid, "net.fabricmc.intermediary");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VersionJsonImport {
    /// UID of the loader component.
    pub uid: String,
    /// Version of the loader component.
    pub version: String,
    /// Name of the loader component shown to users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Libraries to move into components of their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub splits: Vec<SplitRule>,
}

impl VersionJsonImport {
    /// Imports a profile as the component `uid` at `version`.
    #[must_use]
    pub fn new(uid: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            version: version.into(),
            name: None,
            splits: Vec::new(),
        }
    }

    /// Sets the name of the loader component.
    #[must_use]
    pub fn named(self, name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..self
        }
    }

    /// Adds a split rule.
    #[must_use]
    pub fn with_split(mut self, rule: SplitRule) -> Self {
        self.splits.push(rule);
        self
    }

    /// Converts the profile into components in stack order.
    #[must_use]
    pub fn components(&self, raw: RawVersionJson) -> Vec<Component> {
        let game = raw
            .inherits_from
            .as_ref()
            .map(|game| Requirement::new(MINECRAFT_UID).equals(game.as_str()));
        let mut split: Vec<Component> = Vec::new();
        let mut libraries = Vec::new();
        for library in raw.libraries {
            let Some(rule) = self.splits.iter().find(|rule| rule.matches(&library)) else {
                libraries.push(library);
                continue;
            };
            if let Some(existing) = split
                .iter_mut()
                .find(|component| component.uid() == rule.uid)
            {
                existing.patch.libraries.push(library);
                continue;
            }
            let mut patch = ComponentPatch::new(&rule.uid, library.name.version());
            patch.name.clone_from(&rule.name);
            patch.requires.extend(game.clone());
            patch.libraries.push(library);
            let mut component = Component::new(patch);
            component.dependency_only = rule.dependency_only;
            split.push(component);
        }

        let mut patch = ComponentPatch::new(&self.uid, &self.version);
        patch.name.clone_from(&self.name);
        if split.is_empty() {
            patch.requires.extend(game);
        } else {
            patch.requires.extend(
                split
                    .iter()
                    .map(|component| Requirement::new(component.uid())),
            );
        }
        patch.main_class = raw.main_class;
        patch.libraries = libraries;
        patch.minecraft_arguments = raw.minecraft_arguments;
        patch.arguments = raw
            .arguments
            .filter(|arguments| !arguments.game.is_empty() || !arguments.jvm.is_empty());
        split.push(Component::new(patch));
        split
    }
}

#[cfg(test)]
mod tests {
    use coblet_mojang::RawVersionJson;

    use super::{SplitRule, VersionJsonImport};

    fn import() -> VersionJsonImport {
        VersionJsonImport::new("org.quiltmc.quilt-loader", "0.27.1")
            .named("Quilt Loader")
            .with_split(
                SplitRule::new("net.fabricmc:intermediary:*", "net.fabricmc.intermediary")
                    .named("Intermediary Mappings"),
            )
    }

    #[test]
    fn splits_only_what_the_metadata_lists() {
        let obfuscated = RawVersionJson::from_slice(
            br#"{
                "id": "quilt-loader-0.27.1-1.21.11", "inheritsFrom": "1.21.11",
                "mainClass": "org.quiltmc.loader.impl.launch.knot.KnotClient",
                "arguments": { "game": [], "jvm": ["-Dloader.side=client"] },
                "libraries": [
                    { "name": "net.fabricmc:intermediary:1.21.11", "url": "https://maven.fabricmc.net/" },
                    { "name": "org.quiltmc:quilt-loader:0.27.1", "url": "https://maven.quiltmc.org/repository/release/" }
                ]
            }"#,
        )
        .unwrap();
        let components = import().components(obfuscated);
        assert_eq!(components.len(), 2);
        let mappings = &components[0];
        assert!(mappings.dependency_only);
        assert_eq!(
            mappings.patch.name.as_deref(),
            Some("Intermediary Mappings")
        );
        assert_eq!(
            mappings.patch.requires[0].equals.as_deref(),
            Some("1.21.11")
        );
        let loader = &components[1];
        assert_eq!(loader.patch.name.as_deref(), Some("Quilt Loader"));
        assert_eq!(loader.patch.arguments.as_ref().unwrap().jvm.len(), 1);
        assert_eq!(loader.patch.requires.len(), 1);

        let unobfuscated = RawVersionJson::from_slice(
            br#"{
                "id": "quilt-loader-0.27.1-26.3", "inheritsFrom": "26.3",
                "mainClass": "org.quiltmc.loader.impl.launch.knot.KnotClient",
                "arguments": { "game": [] },
                "libraries": [{ "name": "org.quiltmc:quilt-loader:0.27.1" }]
            }"#,
        )
        .unwrap();
        let components = import().components(unobfuscated);
        assert_eq!(components.len(), 1);
        let loader = &components[0];
        assert_eq!(loader.patch.requires[0].uid, "net.minecraft");
        assert_eq!(loader.patch.requires[0].equals.as_deref(), Some("26.3"));
        assert!(loader.patch.arguments.is_none());
    }
}
