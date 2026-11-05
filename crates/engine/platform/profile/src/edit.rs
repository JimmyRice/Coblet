use std::path::PathBuf;

use coblet_types::{Arch, ExtensionId, MavenCoordinate};

use crate::{
    Draft, EffectiveLaunchProfile, EffectiveLibrary, EnvValue, FileSource, JavaSource, LibraryFile,
    Natives, ProfileArg, ProfileError, glob_matches,
};

/// Where the native code of a library comes from, set by
/// [`Draft::handle_natives`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativesSource {
    /// Keeps the natives Mojang's rules selected and vouches for them.
    Artifact,
    /// The natives are not needed on this platform.
    NotNeeded,
    /// The natives are installed on the system in this directory.
    System(PathBuf),
    /// The natives are the same library with another classifier in a Maven repository.
    Maven {
        /// Repository base URL.
        repository: String,
        /// Classifier, such as `natives-freebsd`.
        classifier: String,
    },
}

/// One change to a profile under construction, as data.
///
/// [`Draft::apply`] makes the change with the method of the same name, so
/// profiles that collect changes first, such as ones written in other
/// languages, get the same checks and explanation lines.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileEdit {
    /// [`Draft::append_jvm_arg`].
    AppendJvmArg(ProfileArg),
    /// [`Draft::append_game_arg`].
    AppendGameArg(ProfileArg),
    /// [`Draft::remove_jvm_args`] with a glob pattern.
    RemoveJvmArgs(String),
    /// [`Draft::remove_game_args`] with a glob pattern.
    RemoveGameArgs(String),
    /// [`Draft::set_env`].
    SetEnv(String, EnvValue),
    /// [`Draft::add_native_path`].
    AddNativePath(PathBuf),
    /// [`Draft::handle_natives`].
    HandleNatives {
        /// Glob pattern matched against [`EffectiveLibrary::native_key`].
        pattern: String,
        /// Where the natives come from.
        source: NativesSource,
    },
    /// [`Draft::add_library`].
    AddLibrary {
        /// The library.
        library: Box<EffectiveLibrary>,
        /// Glob pattern of the library to insert it before; last when absent or unmatched.
        before: Option<String>,
    },
    /// [`Draft::remove_libraries`] with a glob pattern.
    RemoveLibraries(String),
    /// [`Draft::set_java_major`].
    JavaMajor(u32),
    /// [`Draft::set_java_source`].
    JavaSource(JavaSource),
    /// [`Draft::set_java_runtime`].
    JavaRuntime(String),
    /// [`Draft::set_java_arch`].
    JavaArch(Arch),
    /// [`Draft::warn`].
    Warn(String),
    /// [`Draft::refuse`].
    Refuse(String),
}

/// Draft that owns a copy of the profile under construction.
///
/// Profiles that edit through handles another language keeps, such as the
/// C and UniFFI bindings, take one with [`Draft::detach`], change it
/// through [`DetachedDraft::draft`] with the usual checks and explanation
/// lines, and hand it back with [`Draft::attach`]; a draft that is never
/// attached changes nothing.
#[derive(Clone, Debug)]
pub struct DetachedDraft {
    profile: EffectiveLaunchProfile,
    source: ExtensionId,
    refusals: Vec<(ExtensionId, String)>,
}

impl DetachedDraft {
    /// Returns a draft over the copy.
    pub fn draft(&mut self) -> Draft<'_> {
        Draft {
            profile: &mut self.profile,
            source: self.source.clone(),
            refusals: &mut self.refusals,
        }
    }

    /// Returns the copy as changed so far.
    #[must_use]
    pub fn profile(&self) -> &EffectiveLaunchProfile {
        &self.profile
    }
}

impl Draft<'_> {
    /// Copies the profile under construction into a [`DetachedDraft`].
    #[must_use]
    pub fn detach(&self) -> DetachedDraft {
        DetachedDraft {
            profile: self.profile.clone(),
            source: self.source.clone(),
            refusals: Vec::new(),
        }
    }

    /// Replaces the profile under construction with a detached copy and
    /// keeps its refusals.
    pub fn attach(&mut self, detached: DetachedDraft) {
        *self.profile = detached.profile;
        self.refusals.extend(detached.refusals);
    }

    /// Makes one change described as data.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::AuthArgument`] for changes to authentication arguments.
    pub fn apply(&mut self, edit: ProfileEdit) -> Result<(), ProfileError> {
        match edit {
            ProfileEdit::AppendJvmArg(arg) => self.append_jvm_arg(arg)?,
            ProfileEdit::AppendGameArg(arg) => self.append_game_arg(arg)?,
            ProfileEdit::RemoveJvmArgs(pattern) => self.remove_jvm_args(&pattern)?,
            ProfileEdit::RemoveGameArgs(pattern) => self.remove_game_args(&pattern)?,
            ProfileEdit::SetEnv(name, value) => self.set_env(name, value),
            ProfileEdit::AddNativePath(path) => self.add_native_path(path),
            ProfileEdit::HandleNatives { pattern, source } => {
                self.handle_natives(&pattern, &source);
            }
            ProfileEdit::AddLibrary { library, before } => {
                self.add_library(*library, before.as_deref());
            }
            ProfileEdit::RemoveLibraries(pattern) => {
                self.remove_libraries(&pattern);
            }
            ProfileEdit::JavaMajor(major) => self.set_java_major(major),
            ProfileEdit::JavaSource(source) => self.set_java_source(source),
            ProfileEdit::JavaRuntime(runtime) => self.set_java_runtime(runtime),
            ProfileEdit::JavaArch(arch) => self.set_java_arch(arch),
            ProfileEdit::Warn(message) => self.warn(message),
            ProfileEdit::Refuse(reason) => self.refuse(reason),
        }
        Ok(())
    }

    /// Says where the native code of matching libraries comes from and
    /// marks it handled by this profile, so validation accepts it.
    ///
    /// `pattern` is a glob matched against [`EffectiveLibrary::native_key`],
    /// such as `org.lwjgl:lwjgl-glfw:*`. Libraries without native code, and
    /// ones this profile already handled, are skipped; the first call that
    /// matches a library wins. Returns how many libraries changed.
    pub fn handle_natives(&mut self, pattern: &str, source: &NativesSource) -> usize {
        let by = self.source.clone();
        let mut notes = Vec::new();
        for library in &mut self.profile.libraries {
            let handled_here =
                matches!(&library.natives, Natives::Handled { by: owner } if *owner == by);
            if library.natives == Natives::None || handled_here {
                continue;
            }
            let key = library.native_key();
            if !glob_matches(pattern, &key) {
                continue;
            }
            let action = match source {
                NativesSource::Artifact => "natives kept".to_owned(),
                NativesSource::NotNeeded => {
                    drop_natives(library);
                    "natives not needed".to_owned()
                }
                NativesSource::System(path) => {
                    drop_natives(library);
                    format!("natives from system path `{}`", path.display())
                }
                NativesSource::Maven {
                    repository,
                    classifier,
                } => {
                    replace_natives(library, repository, classifier);
                    format!("natives replaced by {classifier} from {repository}")
                }
            };
            library.natives = Natives::Handled { by: by.clone() };
            notes.push((format!("library {key}"), action));
        }
        let changed = notes.len();
        for (subject, action) in notes {
            self.note(subject, action);
        }
        if changed > 0
            && let NativesSource::System(path) = source
        {
            self.add_native_path(path.clone());
        }
        changed
    }

    /// Inserts a library before the first one whose name matches the glob
    /// `before`, or last.
    pub fn add_library(&mut self, library: EffectiveLibrary, before: Option<&str>) {
        let name = library.name.to_string();
        let libraries = &mut self.profile.libraries;
        let index = before
            .and_then(|pattern| {
                libraries
                    .iter()
                    .position(|known| glob_matches(pattern, &known.name.to_string()))
            })
            .unwrap_or(libraries.len());
        libraries.insert(index, library);
        self.note(format!("library {name}"), "added");
    }

    /// Removes libraries whose name matches a glob pattern; returns how many.
    pub fn remove_libraries(&mut self, pattern: &str) -> usize {
        let mut removed = Vec::new();
        self.profile.libraries.retain(|library| {
            let name = library.name.to_string();
            let hit = glob_matches(pattern, &name);
            if hit {
                removed.push(name);
            }
            !hit
        });
        let count = removed.len();
        for name in removed {
            self.note(format!("library {name}"), "removed");
        }
        count
    }

    /// Sets the Java major version.
    pub fn set_java_major(&mut self, major: u32) {
        self.profile.java.major = major;
        self.note("java", format!("major version set to {major}"));
    }

    /// Sets how the Java runtime may be obtained.
    pub fn set_java_source(&mut self, source: JavaSource) {
        self.profile.java.source = source;
        self.note("java", format!("source set to {source:?}"));
    }

    /// Sets the platform key of the Mojang Java runtime, such as `linux`.
    pub fn set_java_runtime(&mut self, runtime: impl Into<String>) {
        let runtime = runtime.into();
        self.note(
            "java",
            format!("Mojang runtime platform set to `{runtime}`"),
        );
        self.profile.java.runtime_platform = Some(runtime);
    }

    /// Sets the architecture the Java runtime must be built for.
    pub fn set_java_arch(&mut self, arch: Arch) {
        self.note("java", format!("architecture set to {}", arch.as_str()));
        self.profile.java.arch = arch;
    }
}

/// Removes the native jar of a library; a classifier jar that only holds
/// natives leaves the classpath too.
fn drop_natives(library: &mut EffectiveLibrary) {
    if library.extract.take().is_none() {
        library.classpath = None;
    }
}

fn replace_natives(library: &mut EffectiveLibrary, maven: &str, classifier: &str) {
    let base = maven.trim_end_matches('/');
    let coordinate = MavenCoordinate::new(
        library.name.group(),
        library.name.artifact(),
        library.name.version(),
    )
    .and_then(|name| name.with_classifier(classifier));
    let Ok(coordinate) = coordinate else {
        return;
    };
    let path = coordinate.relative_path();
    let file = LibraryFile::new(
        path.clone(),
        FileSource::Sidecar {
            url: format!("{base}/{path}"),
        },
    );
    if let Some(jar) = &mut library.extract {
        jar.file = file;
        classifier.clone_into(&mut jar.classifier);
    } else {
        library.name = coordinate;
        library.classpath = Some(file);
    }
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
