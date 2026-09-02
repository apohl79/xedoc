use std::fmt;
use std::sync::Arc;

use xedoc_login::AuthManager;
use xedoc_login::ProviderCredentialStore;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_protocol::error::Result;

#[derive(Clone)]
pub(crate) struct ProviderApiKeySource {
    provider_id: Option<String>,
    credential_store: Option<ProviderCredentialStore>,
    fixed_api_key: Option<Option<String>>,
}

impl fmt::Debug for ProviderApiKeySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderApiKeySource")
            .field("provider_id", &self.provider_id)
            .field(
                "credential_store_configured",
                &self.credential_store.is_some(),
            )
            .field("fixed_api_key_configured", &self.fixed_api_key.is_some())
            .finish()
    }
}

impl ProviderApiKeySource {
    pub(crate) fn new(
        provider_id: Option<String>,
        auth_manager: Option<&Arc<AuthManager>>,
    ) -> Self {
        let credential_store = auth_manager.map(|auth_manager| {
            ProviderCredentialStore::new(auth_manager.xedoc_home().to_path_buf())
        });
        Self {
            provider_id,
            credential_store,
            fixed_api_key: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed(api_key: Option<String>) -> Self {
        Self {
            provider_id: None,
            credential_store: None,
            fixed_api_key: Some(api_key),
        }
    }

    pub(crate) fn resolve(&self, provider: &ModelProviderInfo) -> Result<Option<String>> {
        if let Some(api_key) = &self.fixed_api_key {
            return Ok(api_key.clone());
        }
        if let Some(env_key) = &provider.env_key
            && let Some(api_key) = std::env::var(env_key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        {
            return Ok(Some(api_key));
        }
        let (Some(provider_id), Some(credential_store)) =
            (&self.provider_id, &self.credential_store)
        else {
            return Ok(None);
        };
        credential_store.api_key(provider_id).map_err(Into::into)
    }
}
