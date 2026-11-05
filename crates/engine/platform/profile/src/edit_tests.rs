use std::path::PathBuf;
use std::sync::Arc;

use coblet_mojang::{RawVersionJson, TraitTable};
use coblet_runtime::Extension;
use coblet_types::{Arch, ExtensionId, OsFamily, OsName, Platform};

use crate::{
    Draft, JavaSource, Missing, NativesSource, PlatformProfile, ProfileArg, ProfileEdit,
    ProfileEngine, ProfileError, ResolvedProfile, RuleSettings, Target,
};

fn resolved(name: &str) -> ResolvedProfile {
    let path = format!(
        "{}/../../../../fixtures/mojang/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    ResolvedProfile::from_version(raw, traits).unwrap()
}

fn haiku() -> Platform {
    Platform::new(OsFamily::from(OsName::new("haiku").unwrap()), Arch::X86_64)
}

/// Code profile that collects its changes as data, as foreign profiles do.
struct Haiku {
    id: ExtensionId,
    edits: Vec<ProfileEdit>,
}

impl Extension for Haiku {
    fn id(&self) -> &ExtensionId {
        &self.id
    }
    fn display_name(&self) -> &'static str {
        "Haiku"
    }
}

impl PlatformProfile for Haiku {
    fn matches(&self, target: &Target<'_>) -> bool {
        target.platform.os().as_str() == "haiku"
    }
    fn rules(&self, _target: &Target<'_>, settings: &mut RuleSettings) {
        settings.os_name = Some("linux".to_owned());
    }
    fn rewrite(&self, draft: &mut Draft<'_>) -> Result<(), ProfileError> {
        for edit in self.edits.clone() {
            draft.apply(edit)?;
        }
        Ok(())
    }
}

fn engine(edits: Vec<ProfileEdit>) -> ProfileEngine {
    let mut engine = ProfileEngine::builtin();
    let id = ExtensionId::new("acme:haiku").unwrap();
    engine.register(Arc::new(Haiku { id, edits })).unwrap();
    engine
}

fn system(pattern: &str) -> ProfileEdit {
    ProfileEdit::HandleNatives {
        pattern: pattern.to_owned(),
        source: NativesSource::System(PathBuf::from("/boot/system/lib")),
    }
}

#[test]
fn edits_as_data_adapt_a_new_platform() {
    let engine = engine(vec![
        system("org.lwjgl:*:*:natives-linux*"),
        ProfileEdit::HandleNatives {
            pattern: "*".to_owned(),
            source: NativesSource::NotNeeded,
        },
        ProfileEdit::AppendJvmArg(ProfileArg::always(
            "-Dorg.lwjgl.librarypath=/boot/system/lib",
        )),
        ProfileEdit::JavaSource(JavaSource::SystemOnly),
        ProfileEdit::Warn("Haiku support is experimental".to_owned()),
    ]);
    let profile = engine.resolve(&resolved("1.20.1"), &haiku()).unwrap();
    assert_eq!(profile.native_paths, [PathBuf::from("/boot/system/lib")]);
    assert_eq!(profile.java.source, JavaSource::SystemOnly);
    assert_eq!(profile.warnings.len(), 1);
    let explain = profile.explain();
    // The first edit that matches a library wins.
    assert!(
        explain.contains("natives from system path `/boot/system/lib`"),
        "{explain}"
    );
    assert!(
        explain.contains(
            "[rewrite] acme:haiku: jvm `-Dorg.lwjgl.librarypath=/boot/system/lib` appended"
        )
    );
}

#[test]
fn unhandled_natives_stay_unsupported() {
    let engine = engine(vec![system("org.lwjgl:lwjgl:*:natives-linux*")]);
    let error = engine.resolve(&resolved("1.20.1"), &haiku()).unwrap_err();
    let natives = |name: &str| Missing::Natives {
        library: format!("org.lwjgl:{name}:3.3.1:natives-linux"),
    };
    assert!(error.missing().contains(&natives("lwjgl-glfw")));
    assert!(!error.missing().contains(&natives("lwjgl")));
}

#[test]
fn edits_keep_the_checks_and_refusals() {
    let engine_with = |edit| engine(vec![edit]);
    let error = engine_with(ProfileEdit::AppendGameArg(ProfileArg::always(
        "--accessToken",
    )))
    .resolve(&resolved("1.20.1"), &haiku())
    .unwrap_err();
    assert!(
        matches!(error, ProfileError::AuthArgument { .. }),
        "{error}"
    );
    let error = engine_with(ProfileEdit::Refuse("no GPU driver".to_owned()))
        .resolve(&resolved("1.20.1"), &haiku())
        .unwrap_err();
    assert!(error.missing().contains(&Missing::Refused {
        profile: ExtensionId::new("acme:haiku").unwrap(),
        reason: "no GPU driver".to_owned(),
    }));
}

#[test]
fn libraries_can_be_replaced_in_place() {
    let engine = engine(vec![
        ProfileEdit::RemoveLibraries("com.mojang:blocklist:*".to_owned()),
        ProfileEdit::AddLibrary {
            library: Box::new(crate::EffectiveLibrary::new(
                "acme:shim:1.0".parse().unwrap(),
                None,
            )),
            before: Some("org.lwjgl:*".to_owned()),
        },
    ]);
    let error = engine.resolve(&resolved("1.20.1"), &haiku()).unwrap_err();
    let ProfileError::Unsupported { explain, .. } = &error else {
        panic!("{error}");
    };
    assert!(
        explain.contains("[rewrite] acme:haiku: library com.mojang:blocklist:1.0.10 removed"),
        "{explain}"
    );
    assert!(explain.contains("[rewrite] acme:haiku: library acme:shim:1.0 added"));
}
