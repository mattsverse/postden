//! Secret stores for tests: one in memory, and one that can't be written, as a password command.

use postden_core::secrets::{SecretError, SecretKey, SecretStore, SecretString};

/// A secret store in memory.
#[derive(Debug, Default)]
pub(crate) struct Memory(pub(crate) std::sync::Mutex<std::collections::HashMap<SecretKey, String>>);

impl Memory {
    fn secrets(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, std::collections::HashMap<SecretKey, String>>, SecretError>
    {
        self.0
            .lock()
            .map_err(|e| SecretError::Unavailable(e.to_string()))
    }
}

impl SecretStore for Memory {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>, SecretError> {
        Ok(self
            .secrets()?
            .get(key)
            .map(|s| SecretString::from(s.clone())))
    }
    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<(), SecretError> {
        use postden_core::secrets::ExposeSecret as _;
        self.secrets()?
            .insert(key.clone(), secret.expose_secret().to_owned());
        Ok(())
    }
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.secrets()?.remove(key);
        Ok(())
    }
}

/// A password command: it can be read but not written.
#[derive(Debug)]
pub(crate) struct ReadOnly;

impl SecretStore for ReadOnly {
    fn get(&self, _: &SecretKey) -> Result<Option<SecretString>, SecretError> {
        Ok(None)
    }
    fn set(&self, _: &SecretKey, _: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }
    fn is_read_only(&self) -> bool {
        true
    }
}
