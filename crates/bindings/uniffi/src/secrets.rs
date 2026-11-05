use std::sync::Arc;

use coblet::auth::EncryptionKey;
use coblet::runtime::{BoxFuture, Secret, SecretKey, SecretStoreError};

use crate::{CobletError, ErrorKind};

/// Storage for refresh tokens and other secrets, written in the host
/// language, such as one backed by the macOS Keychain or Android Keystore.
///
/// Keys look like `coblet:microsoft/<account id>/refresh_token`. Methods
/// run on a runtime thread and should return quickly.
#[uniffi::export(with_foreign)]
pub trait SecretStore: Send + Sync {
    /// Returns the value stored for `key`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read.
    fn get(&self, key: String) -> Result<Option<String>, CobletError>;

    /// Stores `value` for `key`, replacing the previous value.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be written.
    fn put(&self, key: String, value: String) -> Result<(), CobletError>;

    /// Removes `key`; removing a missing key succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be written.
    fn delete(&self, key: String) -> Result<(), CobletError>;
}

/// Adapts a foreign [`SecretStore`] to coblet's secret store.
pub(crate) struct ForeignSecrets(pub(crate) Arc<dyn SecretStore>);

fn backend(error: CobletError) -> SecretStoreError {
    SecretStoreError::Backend(Box::new(error))
}

impl coblet::runtime::SecretStore for ForeignSecrets {
    fn get<'a>(
        &'a self,
        key: &'a SecretKey,
    ) -> BoxFuture<'a, Result<Option<Secret>, SecretStoreError>> {
        Box::pin(async move {
            let value = self.0.get(key.as_str().to_owned()).map_err(backend)?;
            Ok(value.map(Secret::new))
        })
    }

    fn put<'a>(
        &'a self,
        key: &'a SecretKey,
        secret: Secret,
    ) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async move {
            self.0
                .put(key.as_str().to_owned(), secret.expose().to_owned())
                .map_err(backend)
        })
    }

    fn delete<'a>(&'a self, key: &'a SecretKey) -> BoxFuture<'a, Result<(), SecretStoreError>> {
        Box::pin(async move { self.0.delete(key.as_str().to_owned()).map_err(backend) })
    }
}

/// Returns a new random 32-byte key for
/// `CobletBuilder.set_encrypted_secret_file`.
///
/// # Errors
///
/// Fails when the system random source is unavailable.
#[uniffi::export]
pub fn generate_secret_key() -> Result<Vec<u8>, CobletError> {
    let key = EncryptionKey::generate()
        .map_err(|error| CobletError::new(ErrorKind::Other, error.to_string()))?;
    Ok(key.as_bytes().to_vec())
}

pub(crate) fn encryption_key(bytes: Vec<u8>) -> Result<EncryptionKey, CobletError> {
    let bytes: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        CobletError::new(
            ErrorKind::InvalidArgument,
            format!("the secret key has {} bytes, it needs 32", bytes.len()),
        )
    })?;
    Ok(EncryptionKey::from_bytes(bytes))
}
