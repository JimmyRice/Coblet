use coblet_types::GameVersionId;

use super::{TraitTable, VersionTrait, VersionTraits};
use crate::RawVersionJson;

fn fixture(name: &str) -> RawVersionJson {
    let path = format!(
        "{}/../../../../fixtures/mojang/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    RawVersionJson::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn names(traits: &VersionTraits) -> Vec<&str> {
    traits.iter().map(VersionTrait::as_str).collect()
}

#[test]
fn builtin_table_parses() {
    let table = TraitTable::parse(super::BUILTIN_TABLE).unwrap();
    assert_eq!(&table, TraitTable::builtin());
    assert_eq!(table.applet_tweakers.len(), 2);
}

#[test]
fn derives_traits_from_fixtures() {
    let table = TraitTable::builtin();
    assert_eq!(
        names(&table.for_version(&fixture("c0.30_01c"))),
        [
            "applet",
            "jre-legacy",
            "legacy-online-services",
            "legacy-macos-rendering"
        ]
    );
    assert_eq!(
        names(&table.for_version(&fixture("b1.7.3"))),
        [
            "jre-legacy",
            "legacy-online-services",
            "legacy-macos-rendering"
        ]
    );
    assert_eq!(
        names(&table.for_version(&fixture("1.5.2"))),
        [
            "jre-legacy",
            "legacy-online-services",
            "legacy-macos-rendering"
        ]
    );
    assert_eq!(
        names(&table.for_version(&fixture("1.12.2"))),
        ["jre-legacy"]
    );
    assert!(table.for_version(&fixture("1.20.1")).is_empty());
    assert!(table.for_version(&fixture("26.3")).is_empty());
}

#[test]
fn manifest_entries_skip_rules_that_need_the_version_json() {
    let table = TraitTable::builtin();
    let id = GameVersionId::new("b1.7.3").unwrap();
    let released = "2011-07-07T22:00:00+00:00".parse().ok();
    assert_eq!(
        names(&table.for_manifest_entry(&id, released)),
        ["legacy-online-services"]
    );
    assert!(table.for_manifest_entry(&id, None).is_empty());
}

#[test]
fn rejects_unknown_fields() {
    assert!(TraitTable::parse("[[rule]]\ntraits = []\nversions = []").is_err());
    let table = TraitTable::parse("[[rule]]\ntraits = [\"acme-x\"]\nids = [\"1.0\"]").unwrap();
    let id = GameVersionId::new("1.0").unwrap();
    assert_eq!(names(&table.for_manifest_entry(&id, None)), ["acme-x"]);
}
