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

/// Returns a random version 4 UUID in hyphenated form.
pub(crate) fn uuid() -> Result<String, AuthError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| AuthError::Failed(format!("no secure random source: {error}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes
        .iter()
        .fold(String::with_capacity(32), |mut hex, byte| {
            hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
            hex
        });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
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
#[path = "../../tests/msa/random.rs"]
mod tests;
