use std::collections::BTreeMap;

use super::{Condition, OsCondition, Rule, RuleAction, RuleContext, evaluate};

fn os(name: &str, version: Option<&str>) -> OsCondition {
    OsCondition {
        name: Some(name.to_owned()),
        arch: None,
        version: version.map(str::to_owned),
    }
}

#[test]
fn evaluates_lwjgl2_rules_by_os_version() {
    let rules = [
        Rule::new(RuleAction::Allow),
        Rule::new(RuleAction::Disallow).with_os(os("osx", Some(r"^10\.5\.\d$"))),
    ];
    let leopard = RuleContext::new("osx", "x86_64").with_os_version("10.5.8");
    let sequoia = RuleContext::new("osx", "x86_64").with_os_version("15.5");
    let unknown = RuleContext::new("osx", "x86_64");
    assert_eq!(evaluate(&rules, &leopard), Condition::Never);
    assert_eq!(evaluate(&rules, &sequoia), Condition::Always);
    assert_eq!(evaluate(&rules, &unknown), Condition::Always);
    assert_eq!(evaluate(&[], &unknown), Condition::Always);
    let only_osx = [Rule::new(RuleAction::Allow).with_os(OsCondition::named("osx"))];
    assert_eq!(
        evaluate(&only_osx, &RuleContext::new("linux", "x86_64")),
        Condition::Never
    );
}

#[test]
fn matches_architecture_and_rejects_invalid_patterns() {
    let x86 = [Rule::new(RuleAction::Allow).with_os(OsCondition {
        arch: Some("x86".to_owned()),
        ..OsCondition::default()
    })];
    assert_eq!(
        evaluate(&x86, &RuleContext::new("windows", "x86")),
        Condition::Always
    );
    assert_eq!(
        evaluate(&x86, &RuleContext::new("windows", "x86_64")),
        Condition::Never
    );
    let broken = [Rule::new(RuleAction::Allow).with_os(os("osx", Some("(")))];
    let cx = RuleContext::new("osx", "aarch64").with_os_version("15.5");
    assert_eq!(evaluate(&broken, &cx), Condition::Never);
}

#[test]
fn keeps_feature_conditions_open() {
    let demo = [Rule::new(RuleAction::Allow).with_feature("is_demo_user", true)];
    let condition = evaluate(&demo, &RuleContext::new("linux", "x86_64"));
    assert!(!condition.is_allowed_by_default());
    assert_eq!(condition.features().collect::<Vec<_>>(), ["is_demo_user"]);
    let on = BTreeMap::from([("is_demo_user".to_owned(), true)]);
    assert!(condition.allows(&on));

    // A later unconditional rule overrides earlier feature clauses.
    let overridden = [
        Rule::new(RuleAction::Allow).with_feature("is_demo_user", true),
        Rule::new(RuleAction::Disallow),
    ];
    assert_eq!(
        evaluate(&overridden, &RuleContext::new("linux", "x86_64")),
        Condition::Never
    );
    // Feature clauses after an unconditional allow can still disallow.
    let mixed = [
        Rule::new(RuleAction::Allow),
        Rule::new(RuleAction::Disallow).with_feature("has_custom_resolution", true),
    ];
    let condition = evaluate(&mixed, &RuleContext::new("linux", "x86_64"));
    assert!(condition.is_allowed_by_default());
    assert!(!condition.allows(&BTreeMap::from([(
        "has_custom_resolution".to_owned(),
        true
    )])));
}
