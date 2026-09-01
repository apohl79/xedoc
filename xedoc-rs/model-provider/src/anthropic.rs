use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use xedoc_api::Provider;
use xedoc_api::SharedAuthProvider;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_protocol::account::ProviderAccount;
use xedoc_protocol::error::EnvVarError;
use xedoc_protocol::error::Result;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::openai_models::ModelsResponse;
use xedoc_provider_anthropic::AnthropicAccountPool;
use xedoc_provider_anthropic::AnthropicOAuthAuthProvider;
use xedoc_provider_anthropic::load_anthropic_oauth_credentials;
use xedoc_utils_home_dir::find_xedoc_home;

use crate::anthropic_api_key_auth_provider::AnthropicApiKeyAuthProvider;
use crate::provider::ConfiguredModelProvider;
use crate::provider::ModelProvider;
use crate::provider::ModelProviderFuture;
use crate::provider::ProviderAccountResult;
use crate::provider::ProviderAccountState;
use crate::provider::ProviderCapabilities;

const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_ACCOUNTS_PATH: [&str; 3] = ["providers", "anthropic", "accounts"];

pub(crate) struct AnthropicModelProvider {
    configured: ConfiguredModelProvider,
    api_key: Option<String>,
    oauth_accounts: OAuthAccounts,
    credential_directory: Option<PathBuf>,
}

impl fmt::Debug for AnthropicModelProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicModelProvider")
            .field("configured", &self.configured)
            .field("api_key_configured", &self.api_key.is_some())
            .field("oauth_accounts", &self.oauth_accounts)
            .field("credential_directory", &self.credential_directory)
            .finish()
    }
}

#[derive(Debug)]
enum OAuthAccounts {
    Ready(Arc<AnthropicAccountPool>),
    Unavailable(io::ErrorKind),
}

impl AnthropicModelProvider {
    pub(crate) fn new(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        let api_key = std::env::var(ANTHROPIC_API_KEY_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty());
        let credential_directory = find_xedoc_home().map(|home| {
            ANTHROPIC_ACCOUNTS_PATH
                .into_iter()
                .fold(home, |path, component| path.join(component))
                .into_path_buf()
        });
        Self::from_runtime_sources(provider_info, auth_manager, api_key, credential_directory)
    }

    fn from_runtime_sources(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
        api_key: Option<String>,
        credential_directory: io::Result<PathBuf>,
    ) -> Self {
        let configured = ConfiguredModelProvider::new(provider_info, auth_manager);
        let (oauth_accounts, credential_directory) = match credential_directory {
            Ok(directory) => {
                let accounts = load_accounts(&directory);
                (accounts, Some(directory))
            }
            Err(error) => (OAuthAccounts::Unavailable(error.kind()), None),
        };
        Self {
            configured,
            api_key,
            oauth_accounts,
            credential_directory,
        }
    }

    fn uses_oauth(&self) -> bool {
        self.api_key.is_none()
            && matches!(
                &self.oauth_accounts,
                OAuthAccounts::Ready(accounts) if accounts.account_count() > 0
            )
    }

    fn oauth_auth(&self) -> Result<SharedAuthProvider> {
        let OAuthAccounts::Ready(accounts) = &self.oauth_accounts else {
            return Err(self.missing_credentials_error());
        };
        let credential = accounts
            .select()
            .map_err(|_| self.missing_credentials_error())?;
        Ok(Arc::new(AnthropicOAuthAuthProvider::new(
            credential.credential.access_token,
        )))
    }

    fn missing_credentials_error(&self) -> XedocErr {
        match &self.oauth_accounts {
            OAuthAccounts::Unavailable(kind) => XedocErr::Io(io::Error::new(
                *kind,
                "failed to load Anthropic OAuth credentials",
            )),
            OAuthAccounts::Ready(_) => XedocErr::EnvVar(EnvVarError {
                var: ANTHROPIC_API_KEY_ENV.to_string(),
                instructions: Some(match self.credential_directory.as_ref() {
                    Some(directory) => format!(
                        "Set {ANTHROPIC_API_KEY_ENV} or add an Anthropic OAuth account under {}.",
                        directory.display()
                    ),
                    None => format!(
                        "Set {ANTHROPIC_API_KEY_ENV} or configure an Anthropic OAuth account."
                    ),
                }),
            }),
        }
    }
}

impl ModelProvider for AnthropicModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        self.configured.info()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            namespace_tools: self.info().namespace_tools,
            image_generation: false,
            web_search: false,
        }
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        None
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<XedocAuth>> {
        Box::pin(async { None })
    }

    fn account_state(&self) -> ProviderAccountResult {
        Ok(ProviderAccountState {
            account: self.api_key.as_ref().map(|_| ProviderAccount::ApiKey),
            requires_openai_auth: false,
        })
    }

    fn api_provider(&self) -> ModelProviderFuture<'_, Result<Provider>> {
        Box::pin(async move {
            let mut provider = self.info().to_api_provider(/*auth_mode*/ None)?;
            if self.uses_oauth() {
                provider
                    .query_params
                    .get_or_insert_default()
                    .insert("beta".to_string(), "true".to_string());
            }
            Ok(provider)
        })
    }

    fn api_auth(&self) -> ModelProviderFuture<'_, Result<SharedAuthProvider>> {
        Box::pin(async move {
            match self.api_key.as_ref() {
                Some(api_key) => Ok(Arc::new(AnthropicApiKeyAuthProvider::new(api_key.clone()))
                    as SharedAuthProvider),
                None => self.oauth_auth(),
            }
        })
    }

    fn models_manager(
        &self,
        xedoc_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        self.configured
            .models_manager(xedoc_home, config_model_catalog)
    }

    fn models_manager_without_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        self.configured
            .models_manager_without_cache(config_model_catalog)
    }
}

fn load_accounts(directory: &std::path::Path) -> OAuthAccounts {
    let loaded = match load_anthropic_oauth_credentials(directory) {
        Ok(loaded) => loaded,
        Err(error) => return OAuthAccounts::Unavailable(error.kind()),
    };
    if loaded.accounts.is_empty() && !loaded.failures.is_empty() {
        return OAuthAccounts::Unavailable(io::ErrorKind::InvalidData);
    }
    if !loaded.failures.is_empty() {
        tracing::warn!(
            invalid_credentials = loaded.failures.len(),
            "ignored invalid Anthropic OAuth credential files"
        );
    }
    match AnthropicAccountPool::new(loaded.accounts) {
        Ok(accounts) => OAuthAccounts::Ready(Arc::new(accounts)),
        Err(_) => OAuthAccounts::Unavailable(io::ErrorKind::InvalidData),
    }
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
