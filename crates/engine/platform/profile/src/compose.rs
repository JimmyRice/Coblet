use coblet_mojang::{Argument, Library};

use crate::{Component, ComponentPatch, ComponentRef, ProfileError, ResolvedProfile};

/// Identity of a library for replacement: one with the same group, artifact,
/// classifier and native layout as an earlier library replaces it.
fn library_key(library: &Library) -> (&str, &str, Option<&str>, bool) {
    let name = &library.name;
    (
        name.group(),
        name.artifact(),
        name.classifier(),
        !library.natives.is_empty(),
    )
}

/// Puts a library in place of the first one with the same key and drops the
/// other matches, or appends it.
fn put_library(libraries: &mut Vec<Library>, library: &Library) {
    let key = library_key(library);
    let Some(first) = libraries.iter().position(|old| library_key(old) == key) else {
        libraries.push(library.clone());
        return;
    };
    libraries[first] = library.clone();
    let mut index = 0;
    libraries.retain(|old| {
        let keep = index <= first || library_key(old) != key;
        index += 1;
        keep
    });
}

impl ResolvedProfile {
    /// Applies components on top of this profile, in order: the Compose step
    /// of the launch pipeline.
    ///
    /// Each component's [`requires`](ComponentPatch::requires) and
    /// [`conflicts`](ComponentPatch::conflicts) are checked against the whole
    /// stack, this profile's components included, before anything is applied.
    /// Patches then change the profile in order, so a later component
    /// overrides an earlier one: a library replaces an earlier library with
    /// the same group, artifact and classifier, `mainClass` replaces the main
    /// class, tweakers become `--tweakClass` game arguments and argument lists
    /// are appended.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::DuplicateComponent`],
    /// [`ProfileError::Requirement`] or [`ProfileError::Conflict`] when the
    /// stack is inconsistent, and [`ProfileError::Incomplete`] when the
    /// components accept no common Java version; the profile is not changed.
    ///
    /// # Examples
    ///
    /// ```
    /// use coblet_mojang::{RawVersionJson, VersionTraits};
    /// use coblet_platform::{Component, ComponentPatch, Requirement, ResolvedProfile};
    ///
    /// let raw = RawVersionJson::from_slice(br#"{ "id": "1.20.1", "mainClass": "net.minecraft.client.main.Main", "libraries": [] }"#)?;
    /// let base = ResolvedProfile::from_version(raw, VersionTraits::new())?;
    ///
    /// let mut loader = ComponentPatch::new("acme.loader", "1.0");
    /// loader.requires.push(Requirement::new("net.minecraft").equals("1.20.1"));
    /// loader.main_class = Some("acme.Main".to_owned());
    ///
    /// let profile = base.compose(&[Component::new(loader)])?;
    /// assert_eq!(profile.main_class, "acme.Main");
    /// assert!(profile.component("acme.loader").is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn compose<'a>(
        mut self,
        components: impl IntoIterator<Item = &'a Component>,
    ) -> Result<Self, ProfileError> {
        let components: Vec<&Component> = components.into_iter().collect();
        for component in &components {
            if self.component(component.uid()).is_some() {
                return Err(ProfileError::DuplicateComponent(component.uid().to_owned()));
            }
            self.components.push(ComponentRef {
                uid: component.uid().to_owned(),
                version: component.version().to_owned(),
            });
        }
        for component in &components {
            self.check(&component.patch)?;
        }
        for component in &components {
            self.apply(&component.patch)?;
        }
        Ok(self)
    }

    fn check(&self, patch: &ComponentPatch) -> Result<(), ProfileError> {
        for requirement in &patch.requires {
            let found = self.component(&requirement.uid);
            let satisfied = match (&requirement.equals, found) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(version), Some(found)) => &found.version == version,
            };
            if !satisfied {
                let text = match &requirement.equals {
                    Some(version) => format!("{} {version}", requirement.uid),
                    None => requirement.uid.clone(),
                };
                return Err(ProfileError::Requirement {
                    component: patch.uid.clone(),
                    requirement: text,
                    found: found.map(|found| found.version.clone()),
                });
            }
        }
        for conflict in &patch.conflicts {
            let clash = self.component(&conflict.uid).is_some_and(|found| {
                conflict
                    .equals
                    .as_ref()
                    .is_none_or(|version| &found.version == version)
            });
            if clash {
                return Err(ProfileError::Conflict {
                    component: patch.uid.clone(),
                    other: conflict.uid.clone(),
                });
            }
        }
        Ok(())
    }

    fn apply(&mut self, patch: &ComponentPatch) -> Result<(), ProfileError> {
        if !patch.compatible_java_majors.is_empty() {
            if self.compatible_java_majors.is_empty() {
                self.compatible_java_majors
                    .clone_from(&patch.compatible_java_majors);
            } else {
                self.compatible_java_majors
                    .retain(|major| patch.compatible_java_majors.contains(major));
                if self.compatible_java_majors.is_empty() {
                    return Err(ProfileError::Incomplete(format!(
                        "component {} accepts no java version the other components accept",
                        patch.uid
                    )));
                }
            }
        }
        if let Some(main_class) = &patch.main_class {
            self.main_class.clone_from(main_class);
        }
        for library in patch.libraries.iter().chain(&patch.plus_libraries) {
            put_library(&mut self.libraries, library);
        }
        for file in &patch.maven_files {
            put_library(&mut self.maven_files, file);
        }
        if let Some(arguments) = &patch.minecraft_arguments {
            self.game_args = arguments
                .split_whitespace()
                .map(|arg| Argument::Plain(arg.to_owned()))
                .collect();
            self.legacy_arguments = true;
        }
        if let Some(arguments) = &patch.arguments {
            self.jvm_args.extend(arguments.jvm.iter().cloned());
            self.game_args.extend(arguments.game.iter().cloned());
        }
        self.jvm_args
            .extend(patch.jvm_args.iter().cloned().map(Argument::Plain));
        for tweaker in &patch.tweakers {
            self.game_args
                .push(Argument::Plain("--tweakClass".to_owned()));
            self.game_args.push(Argument::Plain(tweaker.clone()));
        }
        self.jar_mods.extend(patch.jar_mods.iter().cloned());
        self.agents.extend(patch.agents.iter().cloned());
        for value in &patch.traits {
            self.traits.insert(value.clone());
        }
        if let Some(index) = &patch.asset_index {
            self.asset_index = Some(index.clone());
            self.assets.clone_from(&index.id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use coblet_mojang::{Argument, RawVersionJson, VersionTrait, VersionTraits};

    use crate::{Component, ComponentPatch, ProfileError, Requirement, ResolvedProfile};

    fn base() -> ResolvedProfile {
        let raw = RawVersionJson::from_slice(
            br#"{
                "id": "1.12.2",
                "mainClass": "net.minecraft.client.main.Main",
                "minecraftArguments": "--username ${auth_player_name}",
                "javaVersion": { "component": "jre-legacy", "majorVersion": 8 },
                "libraries": [
                    { "name": "com.google.guava:guava:21.0" },
                    { "name": "org.lwjgl.lwjgl:lwjgl:2.9.2-nightly-20140822", "rules": [{ "action": "allow", "os": { "name": "osx" } }] },
                    { "name": "org.lwjgl.lwjgl:lwjgl:2.9.4-nightly-20150209", "rules": [{ "action": "disallow", "os": { "name": "osx" } }] },
                    { "name": "org.ow2.asm:asm:5.0.3" }
                ]
            }"#,
        )
        .unwrap();
        ResolvedProfile::from_version(raw, VersionTraits::new()).unwrap()
    }

    fn patch(json: &str) -> Component {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn later_components_override_earlier_ones() {
        let forge = patch(
            r#"{
                "uid": "net.minecraftforge", "version": "14.23.5.2860",
                "requires": [{ "uid": "net.minecraft", "equals": "1.12.2" }],
                "mainClass": "net.minecraft.launchwrapper.Launch",
                "minecraftArguments": "--username ${auth_player_name} --version FML",
                "+tweakers": ["net.minecraftforge.fml.common.launcher.FMLTweaker"],
                "+jvmArgs": ["-Dfml.ignoreInvalidMinecraftCertificates=true"],
                "+traits": ["legacy-fml-libs"],
                "compatibleJavaMajors": [8],
                "libraries": [
                    { "name": "org.ow2.asm:asm-all:5.2" },
                    { "name": "com.google.guava:guava:21.0-forge" }
                ],
                "mavenFiles": [{ "name": "net.minecraftforge:forge:1.12.2-14.23.5.2860:installer" }]
            }"#,
        );
        let profile = base().compose([&forge]).unwrap();
        let names: Vec<String> = profile
            .libraries
            .iter()
            .map(|library| library.name.to_string())
            .collect();
        assert_eq!(
            names,
            [
                "com.google.guava:guava:21.0-forge",
                "org.lwjgl.lwjgl:lwjgl:2.9.2-nightly-20140822",
                "org.lwjgl.lwjgl:lwjgl:2.9.4-nightly-20150209",
                "org.ow2.asm:asm:5.0.3",
                "org.ow2.asm:asm-all:5.2",
            ]
        );
        assert_eq!(profile.main_class, "net.minecraft.launchwrapper.Launch");
        let game: Vec<&str> = profile
            .game_args
            .iter()
            .flat_map(Argument::values)
            .collect();
        assert_eq!(
            game,
            [
                "--username",
                "${auth_player_name}",
                "--version",
                "FML",
                "--tweakClass",
                "net.minecraftforge.fml.common.launcher.FMLTweaker",
            ]
        );
        assert_eq!(profile.jvm_args.len(), 1);
        assert!(profile.traits.contains(&VersionTrait::LegacyFmlLibs));
        assert_eq!(profile.compatible_java_majors, [8]);
        assert_eq!(profile.maven_files.len(), 1);
        assert_eq!(profile.components.last().unwrap().uid, "net.minecraftforge");
    }

    #[test]
    fn a_replacement_drops_every_earlier_match() {
        let lwjgl = patch(
            r#"{ "uid": "acme.lwjgl", "version": "2.9.4", "libraries": [{ "name": "org.lwjgl.lwjgl:lwjgl:2.9.4-acme" }] }"#,
        );
        let profile = base().compose([&lwjgl]).unwrap();
        let names: Vec<String> = profile
            .libraries
            .iter()
            .map(|library| library.name.to_string())
            .collect();
        assert_eq!(
            names,
            [
                "com.google.guava:guava:21.0",
                "org.lwjgl.lwjgl:lwjgl:2.9.4-acme",
                "org.ow2.asm:asm:5.0.3",
            ]
        );
    }

    #[test]
    fn requirements_see_the_whole_stack() {
        let loader = patch(
            r#"{ "uid": "acme.loader", "version": "1", "requires": [{ "uid": "acme.mappings" }] }"#,
        );
        let mappings = patch(
            r#"{ "uid": "acme.mappings", "version": "1.12.2", "requires": [{ "uid": "net.minecraft", "equals": "1.12.2" }] }"#,
        );
        let profile = base().compose([&loader, &mappings]).unwrap();
        assert_eq!(profile.components.len(), 4);

        let error = base().compose([&loader]).unwrap_err();
        assert!(matches!(error, ProfileError::Requirement { ref found, .. } if found.is_none()));
        assert_eq!(
            error.to_string(),
            "component acme.loader requires acme.mappings, which is missing"
        );

        let mut wrong = ComponentPatch::new("acme.other", "1");
        wrong
            .requires
            .push(Requirement::new("net.minecraft").equals("1.20.1"));
        let error = base().compose([&Component::new(wrong)]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "component acme.other requires net.minecraft 1.20.1, found 1.12.2"
        );
    }

    #[test]
    fn rejects_conflicts_duplicates_and_disjoint_java() {
        let a = patch(
            r#"{ "uid": "acme.a", "version": "1", "conflicts": [{ "uid": "acme.b" }], "compatibleJavaMajors": [8] }"#,
        );
        let b = patch(r#"{ "uid": "acme.b", "version": "1", "compatibleJavaMajors": [17] }"#);
        assert!(matches!(
            base().compose([&a, &b]),
            Err(ProfileError::Conflict { .. })
        ));
        assert!(matches!(
            base().compose([&a, &a]),
            Err(ProfileError::DuplicateComponent(_))
        ));
        let c = patch(r#"{ "uid": "acme.c", "version": "1", "compatibleJavaMajors": [17, 21] }"#);
        assert!(matches!(
            base().compose([&b, &c]).map(|p| p.compatible_java_majors),
            Ok(majors) if majors == [17]
        ));
        let d = patch(r#"{ "uid": "acme.d", "version": "1", "compatibleJavaMajors": [8] }"#);
        assert!(matches!(
            base().compose([&c, &d]),
            Err(ProfileError::Incomplete(_))
        ));
    }
}
