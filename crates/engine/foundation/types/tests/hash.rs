use super::{DigestError, FileHash, FileHashError, HashAlgorithm, Sha1Digest, Sha256Digest};

const EMPTY_SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[test]
fn parses_and_prints_hex() {
    let digest: Sha1Digest = EMPTY_SHA1.parse().unwrap();
    assert_eq!(digest.as_bytes()[0], 0xda);
    assert_eq!(digest.as_bytes()[19], 0x09);
    assert_eq!(digest.to_string(), EMPTY_SHA1);
    assert_eq!(
        Sha256Digest::from_hex(EMPTY_SHA256).unwrap().to_string(),
        EMPTY_SHA256
    );
}

#[test]
fn rejects_bad_hex() {
    assert_eq!(
        Sha1Digest::from_hex("abcd"),
        Err(DigestError::WrongLength {
            expected: 40,
            actual: 4
        })
    );
    let bad = format!("{}g", &EMPTY_SHA1[..39]);
    assert_eq!(Sha1Digest::from_hex(&bad), Err(DigestError::NotHex('g')));
    let multibyte = format!("{}é", &EMPTY_SHA1[..39]);
    assert_eq!(
        Sha1Digest::from_hex(&multibyte),
        Err(DigestError::NotHex('é'))
    );
}

#[test]
fn parses_file_hashes() {
    let hash: FileHash = format!("SHA-256:{EMPTY_SHA256}").parse().unwrap();
    assert_eq!(hash.algorithm(), HashAlgorithm::Sha256);
    assert_eq!(hash.to_string(), format!("sha256:{EMPTY_SHA256}"));

    let other: FileHash = "murmur2:12345".parse().unwrap();
    assert_eq!(
        other,
        FileHash::Other {
            algorithm: "murmur2".to_owned(),
            value: "12345".to_owned()
        }
    );
    assert!(matches!(
        "sha1:abc".parse::<FileHash>(),
        Err(FileHashError::Digest { .. })
    ));
    assert!(matches!(
        "sha1".parse::<FileHash>(),
        Err(FileHashError::Malformed(_))
    ));
    assert!(matches!(
        ":abc".parse::<FileHash>(),
        Err(FileHashError::Malformed(_))
    ));
}

#[test]
fn round_trips_through_serde() {
    let hash = FileHash::new("sha1", EMPTY_SHA1).unwrap();
    let json = serde_json::to_string(&hash).unwrap();
    assert_eq!(json, format!("\"sha1:{EMPTY_SHA1}\""));
    assert_eq!(serde_json::from_str::<FileHash>(&json).unwrap(), hash);
}
