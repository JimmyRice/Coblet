use std::path::Path;
use std::sync::{Arc, Mutex};

use coblet::mojang::{RawVersionJson, TraitTable};
use coblet::platform::{JavaSource, ProfileEngine, ProfileError, ResolvedProfile};
use coblet::types::{Arch, ArchName, OsFamily, Platform};

use super::ForeignProfile;
use crate::{
    CobletBuilder, CobletError, ErrorKind, JavaSourceChoice, NativesChoice, PlatformProfile,
    ProfileDraft, ProfileTarget, RuleOverrides,
};

/// Where a test profile fails, if anywhere.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fail {
    Nowhere,
    Matches,
    Rewrite,
    AuthArgument,
}

/// Profile for loongarch64 as a Python or Kotlin author would write it.
struct LoongArch {
    fail: Fail,
    draft: Mutex<Option<Arc<ProfileDraft>>>,
}

impl LoongArch {
    fn new(fail: Fail) -> Arc<Self> {
        Arc::new(Self {
            fail,
            draft: Mutex::new(None),
        })
    }
}

fn error(message: &str) -> CobletError {
    CobletError::new(ErrorKind::Other, message)
}

impl PlatformProfile for LoongArch {
    fn id(&self) -> String {
        "acme:loongarch".to_owned()
    }

    fn display_name(&self) -> String {
        "LoongArch".to_owned()
    }

    fn priority(&self) -> i32 {
        100
    }

    fn matches(&self, target: ProfileTarget) -> Result<bool, CobletError> {
        if self.fail == Fail::Matches {
            return Err(error("cannot read /proc/cpuinfo"));
        }
        Ok(target.platform.arch == "loongarch64"
            && target.components.iter().any(|c| c.uid == "org.lwjgl3"))
    }

    fn rules(&self, _target: ProfileTarget) -> Result<RuleOverrides, CobletError> {
        Ok(RuleOverrides {
            mojang_os: Some("linux".to_owned()),
            ..RuleOverrides::default()
        })
    }

    fn rewrite(&self, draft: Arc<ProfileDraft>) -> Result<(), CobletError> {
        *self.draft.lock().unwrap() = Some(Arc::clone(&draft));
        if self.fail == Fail::Rewrite {
            return Err(error("no GPU driver found"));
        }
        if self.fail == Fail::AuthArgument {
            draft.append_game_arg("--accessToken".to_owned())?;
        }
        for library in draft.libraries()? {
            if !library.has_natives {
                continue;
            }
            let choice = if library.native_key.ends_with(":natives-linux") {
                NativesChoice::System {
                    path: "/usr/lib/lwjgl3".to_owned(),
                }
            } else {
                NativesChoice::NotNeeded
            };
            draft.handle_natives(library.native_key, choice)?;
        }
        draft.append_jvm_arg("-Dorg.lwjgl.librarypath=/usr/lib/lwjgl3".to_owned())?;
        draft.set_java_source(JavaSourceChoice::SystemOnly)?;
        Ok(())
    }
}

fn resolved() -> ResolvedProfile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/mojang/1.20.1.json");
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    ResolvedProfile::from_version(raw, traits).unwrap()
}

fn loongarch() -> Platform {
    Platform::new(
        OsFamily::Linux,
        Arch::from(ArchName::new("loongarch64").unwrap()),
    )
}

fn resolve(
    profile: Arc<LoongArch>,
) -> Result<coblet::platform::EffectiveLaunchProfile, ProfileError> {
    let mut engine = ProfileEngine::builtin();
    engine
        .register(Arc::new(ForeignProfile::new(profile).unwrap()))
        .unwrap();
    engine.resolve(&resolved(), &loongarch())
}

#[test]
fn a_foreign_profile_adapts_a_new_architecture() {
    assert!(
        ProfileEngine::builtin()
            .resolve(&resolved(), &loongarch())
            .is_err()
    );
    let author = LoongArch::new(Fail::Nowhere);
    let profile = resolve(Arc::clone(&author)).unwrap();
    assert_eq!(profile.java.source, JavaSource::SystemOnly);
    assert_eq!(
        profile.native_paths,
        [std::path::PathBuf::from("/usr/lib/lwjgl3")]
    );
    let explain = profile.explain();
    assert!(
        explain.contains("[match] acme:loongarch: profile matched (ThirdParty, priority 100)"),
        "{explain}"
    );
    assert!(explain.contains("[evaluate] acme:loongarch: rules evaluate as os `linux`"));
    assert!(explain.contains("natives from system path `/usr/lib/lwjgl3`"));
    assert!(explain.contains("jvm `-Dorg.lwjgl.librarypath=/usr/lib/lwjgl3` appended"));

    let draft = author.draft.lock().unwrap().clone().unwrap();
    let CobletError::Failed { kind, .. } = draft.libraries().unwrap_err();
    assert_eq!(kind, ErrorKind::InvalidState);
}

#[test]
fn foreign_errors_stop_the_launch() {
    let error = resolve(LoongArch::new(Fail::Matches)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "platform profile acme:loongarch failed: cannot read /proc/cpuinfo"
    );
    let error = resolve(LoongArch::new(Fail::Rewrite)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "platform profile acme:loongarch failed: no GPU driver found"
    );
    let error = resolve(LoongArch::new(Fail::AuthArgument)).unwrap_err();
    assert!(
        matches!(error, ProfileError::AuthArgument { .. }),
        "{error}"
    );
}

#[test]
fn foreign_profiles_are_registered_through_the_builder() {
    let builder = CobletBuilder::new(
        "/tmp/coblet-uniffi-foreign".to_owned(),
        "coblet-uniffi-test/1.0".to_owned(),
    )
    .unwrap();
    builder
        .add_platform_profile(LoongArch::new(Fail::Nowhere))
        .unwrap();
    let error = builder
        .add_platform_profile(LoongArch::new(Fail::Nowhere))
        .unwrap_err();
    assert!(error.to_string().contains("already registered"), "{error}");
    let coblet = builder.build().unwrap();
    assert!(
        coblet
            .inner
            .profile_engine()
            .profiles()
            .iter()
            .any(
                |profile| coblet::runtime::Extension::id(profile.as_ref()).as_str()
                    == "acme:loongarch"
            )
    );
}

#[test]
fn drafts_check_paths() {
    let draft = ProfileDraft::new(resolve(LoongArch::new(Fail::Nowhere)).unwrap());
    assert!(draft.add_native_path("relative/lib".to_owned()).is_err());
    assert!(
        draft
            .handle_natives(
                "*".to_owned(),
                NativesChoice::System {
                    path: "lib".to_owned()
                }
            )
            .is_err()
    );
    assert!(draft.set_java_arch("Not An Arch").is_err());
    draft.add_native_path("/opt/lib".to_owned()).unwrap();
    assert_eq!(draft.close().len(), 1);
}
