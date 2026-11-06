use std::collections::BTreeMap;

use super::{FeatureClause, FeatureCondition};

#[test]
fn decides_feature_conditions() {
    let demo = FeatureCondition::when([("is_demo_user".to_owned(), true)]);
    assert!(!demo.holds(&BTreeMap::new()));
    assert!(demo.holds(&BTreeMap::from([("is_demo_user".to_owned(), true)])));
    assert!(FeatureCondition::Always.holds(&BTreeMap::new()));
    let not_custom = FeatureCondition::Clauses(vec![
        FeatureClause {
            allow: true,
            features: BTreeMap::new(),
        },
        FeatureClause {
            allow: false,
            features: BTreeMap::from([("has_custom_resolution".to_owned(), true)]),
        },
    ]);
    assert!(not_custom.holds(&BTreeMap::new()));
    assert!(!not_custom.holds(&BTreeMap::from([(
        "has_custom_resolution".to_owned(),
        true
    )])));
}
