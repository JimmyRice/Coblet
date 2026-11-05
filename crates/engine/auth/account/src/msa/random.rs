use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::AuthError;

/// Returns `bytes` random bytes from the operating system, base64url encoded.
pub(crate) fn token(bytes: usize) -> Result<String, AuthError> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|error| AuthError::Failed(format!("no secure random source: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(&buffer))
}

/// PKCE verifier and its S256 challenge.
pub(crate) struct Pkce {
    pub(crate) verifier: String,
    pub(crate) challenge: String,
}

impl Pkce {
    pub(crate) fn generate() -> Result<Self, AuthError> {
        let verifier = token(32)?;
        let challenge = challenge(&verifier);
        Ok(Self {
            verifier,
            challenge,
        })
    }
}

/// Returns the S256 challenge of a verifier.
pub(crate) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{Pkce, challenge, token};

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
}
