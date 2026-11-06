use super::FILES;
use crate::{DataProfile, ProfileLayer};

#[test]
fn every_builtin_profile_parses() {
    for (name, text) in FILES {
        DataProfile::from_toml(text, ProfileLayer::Builtin)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    assert_eq!(super::profiles().len(), FILES.len());
}
