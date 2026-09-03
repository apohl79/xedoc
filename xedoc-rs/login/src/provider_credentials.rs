use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;
use serde::Serialize;
use xedoc_secrets::SecretName;
use xedoc_secrets::SecretScope;
use xedoc_secrets::SecretsBackendKind;
use xedoc_secrets::SecretsManager;

const PROVIDER_CREDENTIALS_SCHEMA_VERSION: u32 = 1;
const PROVIDER_CREDENTIALS_SECRET: &str = "XEDOC_MODEL_PROVIDER_CREDENTIALS";

static CREDENTIAL_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct ProviderCredentialStore {
    xedoc_home: PathBuf,
    secrets: SecretsManager,
}

impl fmt::Debug for ProviderCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCredentialStore")
            .field("xedoc_home", &self.xedoc_home)
            .finish_non_exhaustive()
    }
}

#[derive(Default, Deserialize, Serialize)]
struct ProviderCredentials {
    schema_version: u32,
    api_keys: BTreeMap<String, String>,
}

impl ProviderCredentialStore {
    pub fn new(xedoc_home: PathBuf) -> Self {
        Self {
            secrets: SecretsManager::new(xedoc_home.clone(), SecretsBackendKind::Local),
            xedoc_home,
        }
    }

    #[doc(hidden)]
    pub fn new_with_keyring_store(
        xedoc_home: PathBuf,
        keyring_store: std::sync::Arc<dyn xedoc_keyring_store::KeyringStore>,
    ) -> Self {
        Self {
            secrets: SecretsManager::new_with_keyring_store(
                xedoc_home.clone(),
                SecretsBackendKind::Local,
                keyring_store,
            ),
            xedoc_home,
        }
    }

    pub fn api_key(&self, provider_id: &str) -> std::io::Result<Option<String>> {
        validate_provider_id(provider_id)?;
        Ok(self.load()?.api_keys.get(provider_id).cloned())
    }

    pub fn has_api_key(&self, provider_id: &str) -> std::io::Result<bool> {
        self.api_key(provider_id).map(|api_key| api_key.is_some())
    }

    pub fn set_api_key(&self, provider_id: &str, api_key: &str) -> std::io::Result<()> {
        validate_provider_id(provider_id)?;
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "provider API key must not be empty",
            ));
        }
        let _guard = CREDENTIAL_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut credentials = self.load()?;
        credentials
            .api_keys
            .insert(provider_id.to_string(), api_key.to_string());
        self.save(&credentials)
    }

    pub fn delete_api_key(&self, provider_id: &str) -> std::io::Result<bool> {
        validate_provider_id(provider_id)?;
        let _guard = CREDENTIAL_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut credentials = self.load()?;
        let removed = credentials.api_keys.remove(provider_id).is_some();
        if !removed {
            return Ok(false);
        }
        self.save(&credentials)?;
        Ok(true)
    }

    fn load(&self) -> std::io::Result<ProviderCredentials> {
        let secret_name = credential_secret_name()?;
        let Some(serialized) = self
            .secrets
            .get(&SecretScope::Global, &secret_name)
            .map_err(provider_credentials_error)?
        else {
            return Ok(ProviderCredentials {
                schema_version: PROVIDER_CREDENTIALS_SCHEMA_VERSION,
                api_keys: BTreeMap::new(),
            });
        };
        let credentials: ProviderCredentials =
            serde_json::from_str(&serialized).map_err(provider_credentials_error)?;
        if credentials.schema_version != PROVIDER_CREDENTIALS_SCHEMA_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "unsupported provider credential schema {}; expected {PROVIDER_CREDENTIALS_SCHEMA_VERSION}",
                    credentials.schema_version
                ),
            ));
        }
        Ok(credentials)
    }

    fn save(&self, credentials: &ProviderCredentials) -> std::io::Result<()> {
        let secret_name = credential_secret_name()?;
        if credentials.api_keys.is_empty() {
            self.secrets
                .delete(&SecretScope::Global, &secret_name)
                .map_err(provider_credentials_error)?;
            return Ok(());
        }
        let serialized = serde_json::to_string(credentials).map_err(provider_credentials_error)?;
        self.secrets
            .set(&SecretScope::Global, &secret_name, &serialized)
            .map_err(provider_credentials_error)
    }
}

fn credential_secret_name() -> std::io::Result<SecretName> {
    SecretName::new(PROVIDER_CREDENTIALS_SECRET).map_err(provider_credentials_error)
}

fn validate_provider_id(provider_id: &str) -> std::io::Result<()> {
    if provider_id.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider ID must not be empty",
        ));
    }
    Ok(())
}

fn provider_credentials_error(error: impl fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("provider credential storage failed: {error}"))
}

#[cfg(test)]
#[path = "provider_credentials_tests.rs"]
mod tests;
