use std::path::{Path, PathBuf};

use coblet_mojang::{RawVersionJson, TraitTable};
use coblet_platform::{
    EffectiveLaunchProfile, EffectiveLibrary, EnvValue, FileSource, LibraryFile, PipelineStep,
    ProfileArg, ProfileEngine, ResolvedProfile,
};
use coblet_types::{Arch, ExtensionId, MavenCoordinate, OsFamily, Platform};

use super::record;

fn profile() -> EffectiveLaunchProfile {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../fixtures/mojang/1.12.2.json");
    let raw = RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let traits = TraitTable::builtin().for_version(&raw);
    let resolved = ResolvedProfile::from_version(raw, traits).unwrap();
    let platform = Platform::new(OsFamily::Linux, Arch::X86_64);
    ProfileEngine::builtin()
        .resolve(&resolved, &platform)
        .unwrap()
}

fn customize_lines(profile: &EffectiveLaunchProfile) -> Vec<String> {
    profile
        .explain()
        .lines()
        .filter(|line| line.starts_with("[customize]"))
        .map(str::to_owned)
        .collect()
}

fn id() -> ExtensionId {
    ExtensionId::new("acme:fix").unwrap()
}

#[test]
fn records_nothing_when_nothing_changed() {
    let before = profile();
    let mut after = before.clone();
    record(&before, &mut after, &id());
    assert_eq!(after, before);
}

#[test]
fn records_every_kind_of_edit() {
    let before = profile();
    let mut after = before.clone();
    after.main_class = "acme.Wrapper".to_owned();
    after.libraries.push(EffectiveLibrary::new(
        MavenCoordinate::new("acme", "wrapper", "1.0").unwrap(),
        Some(LibraryFile::new(
            "acme/wrapper/1.0/wrapper-1.0.jar",
            FileSource::Existing,
        )),
    ));
    let removed = after.libraries.remove(0).native_key();
    after.libraries[0].classpath = None;
    let changed = after.libraries[0].native_key();
    after
        .jvm_args
        .insert(0, ProfileArg::always("-javaagent:/opt/fix.jar"));
    let dropped = after.game_args.pop().unwrap().value;
    after
        .env
        .insert("ACME".to_owned(), EnvValue::Set("1".to_owned()));
    after.native_paths.push(PathBuf::from("/opt/natives"));
    after.java.major = 21;
    after.client = None;
    after.assets = "legacy".to_owned();
    record(&before, &mut after, &id());

    let lines = customize_lines(&after);
    for needle in [
        "[customize] acme:fix: main class set to `acme.Wrapper` (was `net.minecraft.client.main.Main`)".to_owned(),
        "[customize] acme:fix: library acme:wrapper:1.0 added".to_owned(),
        format!("[customize] acme:fix: library {removed} removed"),
        format!("[customize] acme:fix: library {changed} changed"),
        "[customize] acme:fix: jvm `-javaagent:/opt/fix.jar` added".to_owned(),
        format!("[customize] acme:fix: game `{dropped}` removed"),
        "[customize] acme:fix: env ACME set to \"1\"".to_owned(),
        "[customize] acme:fix: native path `/opt/natives` added".to_owned(),
        "[customize] acme:fix: java major version set to 21".to_owned(),
        "[customize] acme:fix: client jar removed".to_owned(),
        "[customize] acme:fix: assets changed".to_owned(),
    ] {
        assert!(lines.contains(&needle), "missing {needle:?} in {lines:#?}");
    }
    assert_eq!(lines.len(), 11, "{lines:#?}");
    assert!(
        after
            .changes
            .iter()
            .rev()
            .take(11)
            .all(|change| change.step == PipelineStep::Customize)
    );
}

#[test]
fn records_reordering_without_edits() {
    let before = profile();
    let mut after = before.clone();
    after.libraries.swap(0, 1);
    after.jvm_args.reverse();
    record(&before, &mut after, &id());
    assert_eq!(
        customize_lines(&after),
        [
            "[customize] acme:fix: libraries reordered",
            "[customize] acme:fix: jvm arguments reordered"
        ]
    );
}
