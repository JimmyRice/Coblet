use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;

use super::{PlaceholderResolver, UnknownPlaceholder, Values};
use crate::CommandError;

struct Brand;

impl PlaceholderResolver for Brand {
    fn resolve(&self, name: &str) -> Option<OsString> {
        (name == "acme_brand").then(|| "acme".into())
    }
}

#[test]
fn substitutes_with_table_resolvers_and_policy() {
    let resolvers: Vec<Arc<dyn PlaceholderResolver>> = vec![Arc::new(Brand)];
    let mut values = Values {
        table: BTreeMap::from([("version_name", OsString::from("1.20.1"))]),
        resolvers: &resolvers,
        unknown: UnknownPlaceholder::Error,
    };
    assert_eq!(
        values
            .substitute("-Dv=${version_name}-${acme_brand}")
            .unwrap(),
        "-Dv=1.20.1-acme"
    );
    assert_eq!(
        values.substitute("${nope}"),
        Err(CommandError::UnknownPlaceholder("nope".to_owned()))
    );
    assert!(matches!(
        values.substitute("${open"),
        Err(CommandError::Unterminated(_))
    ));
    values.unknown = UnknownPlaceholder::Keep;
    assert_eq!(values.substitute("a${nope}b").unwrap(), "a${nope}b");
    values.unknown = UnknownPlaceholder::Empty;
    assert_eq!(values.substitute("a${nope}b").unwrap(), "ab");
}
