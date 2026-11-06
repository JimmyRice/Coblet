use super::{Pkce, challenge, token, uuid};

#[test]
fn matches_the_rfc_7636_example() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn generates_distinct_verifiers() {
    let a = Pkce::generate().unwrap();
    let b = Pkce::generate().unwrap();
    assert_eq!(a.verifier.len(), 43);
    assert_ne!(a.verifier, b.verifier);
    assert_eq!(token(16).unwrap().len(), 22);
}

#[test]
fn generates_version_4_uuids() {
    let id = uuid().unwrap();
    assert_eq!(id.len(), 36);
    let parts: Vec<_> = id.split('-').map(str::len).collect();
    assert_eq!(parts, [8, 4, 4, 4, 12]);
    assert_eq!(&id[14..15], "4");
    assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
    assert_ne!(id, uuid().unwrap());
}
