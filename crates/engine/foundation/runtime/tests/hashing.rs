use coblet_types::{FileHash, HashAlgorithm};

use super::{StreamHasher, VerifyError};
use crate::Expectation;

const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

#[test]
fn computes_requested_digests() {
    let mut stream = StreamHasher::new(&[HashAlgorithm::Sha256]);
    stream.update(b"abc");
    let hashes = stream.finish();
    assert_eq!(hashes.len, 3);
    assert_eq!(
        hashes.get(&HashAlgorithm::Sha256).unwrap().to_string(),
        format!("sha256:{ABC_SHA256}")
    );
    assert_eq!(hashes.get(&HashAlgorithm::Sha1), None);
}

#[test]
fn reports_mismatches() {
    let good = FileHash::new("sha256", ABC_SHA256).unwrap();
    let bad = FileHash::new("sha256", &"0".repeat(64)).unwrap();
    let mut stream = StreamHasher::new(&[HashAlgorithm::Sha256]);
    stream.update(b"abc");
    let hashes = stream.finish();
    assert!(hashes.check(&Expectation::hash(good, Some(3))).is_ok());
    assert!(matches!(
        hashes.check(&Expectation::hash(bad.clone(), None)),
        Err(VerifyError::Hash { .. })
    ));
    assert!(matches!(
        hashes.check(&Expectation::hash(bad, Some(4))),
        Err(VerifyError::Size {
            expected: 4,
            actual: 3
        })
    ));
    let murmur = FileHash::new("murmur2", "1").unwrap();
    assert!(matches!(
        hashes.check(&Expectation::hash(murmur, None)),
        Err(VerifyError::UnsupportedAlgorithm(_))
    ));
    assert!(
        hashes
            .check(&Expectation::Unverified { size: Some(3) })
            .is_ok()
    );
}
