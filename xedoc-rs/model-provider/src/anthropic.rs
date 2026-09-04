use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

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
use xedoc_provider_anthropic::ANTHROPIC_OAUTH_TOKEN_ENDPOINT;
use xedoc_provider_anthropic::AnthropicAccountPool;
use xedoc_provider_anthropic::AnthropicCredentialLoad;
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
    oauth_accounts: Mutex<CachedOAuthAccounts>,
    credential_directory: Option<PathBuf>,
}

impl fmt::Debug for AnthropicModelProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicModelProvider")
            .field("configured", &self.configured)
            .field(
                "api_key_configured",
                &self.configured.provider_api_key().ok().flatten().is_some(),
            )
            .field(
                "oauth_accounts",
                &self
                    .oauth_accounts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .accounts,
            )
            .field("credential_directory", &self.credential_directory)
            .finish()
    }
}

#[derive(Clone, Debug)]
enum OAuthAccounts {
    Ready(Arc<AnthropicAccountPool>),
    Unavailable(io::ErrorKind),
}

#[derive(Debug)]
struct CachedOAuthAccounts {
    loaded: Option<AnthropicCredentialLoad>,
    accounts: OAuthAccounts,
}

impl AnthropicModelProvider {
    pub(crate) fn new(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        let credential_directory = credential_directory(auth_manager.as_ref());
        let configured = ConfiguredModelProvider::new(provider_info, auth_manager);
        Self::from_configured(configured, credential_directory)
    }

    pub(crate) fn new_for_configured_id(
        provider_id: String,
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> Self {
        let credential_directory = credential_directory(auth_manager.as_ref());
        let configured = ConfiguredModelProvider::new_for_configured_id(
            provider_id,
            provider_info,
            auth_manager,
        );
        Self::from_configured(configured, credential_directory)
    }

    #[cfg(test)]
    fn from_runtime_sources(
        provider_info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
        api_key: Option<String>,
        credential_directory: io::Result<PathBuf>,
    ) -> Self {
        let configured =
            ConfiguredModelProvider::new_with_fixed_api_key(provider_info, auth_manager, api_key);
        Self::from_configured(configured, credential_directory)
    }

    fn from_configured(
        configured: ConfiguredModelProvider,
        credential_directory: io::Result<PathBuf>,
    ) -> Self {
        let (credential_directory, unavailable_kind) = match credential_directory {
            Ok(directory) => (Some(directory), io::ErrorKind::NotFound),
            Err(error) => (None, error.kind()),
        };
        Self {
            configured,
            oauth_accounts: Mutex::new(CachedOAuthAccounts {
                loaded: None,
                accounts: OAuthAccounts::Unavailable(unavailable_kind),
            }),
            credential_directory,
        }
    }

    fn uses_oauth(&self) -> bool {
        self.configured.provider_api_key().ok().flatten().is_none()
            && matches!(
                self.current_oauth_accounts(),
                OAuthAccounts::Ready(accounts) if accounts.account_count() > 0
            )
    }

    fn oauth_auth(&self) -> Result<SharedAuthProvider> {
        let oauth_accounts = self.current_oauth_accounts();
        let OAuthAccounts::Ready(accounts) = &oauth_accounts else {
            return Err(self.missing_credentials_error(&oauth_accounts));
        };
        let credential = accounts
            .select()
            .map_err(|_| self.missing_credentials_error(&oauth_accounts))?;
        Ok(Arc::new(AnthropicOAuthAuthProvider::new(
            credential,
            Arc::clone(accounts),
            ANTHROPIC_OAUTH_TOKEN_ENDPOINT.to_string(),
        )))
    }

    fn current_oauth_accounts(&self) -> OAuthAccounts {
        let Some(directory) = self.credential_directory.as_deref() else {
            return self
                .oauth_accounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .accounts
                .clone();
        };
        let loaded = match load_anthropic_oauth_credentials(directory) {
            Ok(loaded) => loaded,
            Err(error) => return OAuthAccounts::Unavailable(error.kind()),
        };
        let mut cached = self
            .oauth_accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached.loaded.as_ref() != Some(&loaded) {
            cached.accounts = accounts_from_load(&loaded);
            cached.loaded = Some(loaded);
        }
        cached.accounts.clone()
    }

    fn missing_credentials_error(&self, oauth_accounts: &OAuthAccounts) -> XedocErr {
        match oauth_accounts {
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
            account: self
                .configured
                .provider_api_key()
                .ok()
                .flatten()
                .map(|_| ProviderAccount::ApiKey),
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
            match self.configured.provider_api_key()? {
                Some(api_key) => {
                    Ok(Arc::new(AnthropicApiKeyAuthProvider::new(api_key)) as SharedAuthProvider)
                }
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

fn credential_directory(auth_manager: Option<&Arc<AuthManager>>) -> io::Result<PathBuf> {
    let home = match auth_manager {
        Some(auth_manager) => auth_manager.xedoc_home().to_path_buf(),
        None => find_xedoc_home()?.into_path_buf(),
    };
    Ok(ANTHROPIC_ACCOUNTS_PATH
        .into_iter()
        .fold(home, |path, component| path.join(component)))
}

pub(super) fn has_oauth_credentials(auth_manager: &Arc<AuthManager>) -> Result<bool> {
    let loaded = load_anthropic_oauth_credentials(&credential_directory(Some(auth_manager))?)?;
    Ok(!loaded.accounts.is_empty())
}

fn accounts_from_load(loaded: &AnthropicCredentialLoad) -> OAuthAccounts {
    if loaded.accounts.is_empty() && !loaded.failures.is_empty() {
        return OAuthAccounts::Unavailable(io::ErrorKind::InvalidData);
    }
    if !loaded.failures.is_empty() {
        tracing::warn!(
            invalid_credentials = loaded.failures.len(),
            "ignored invalid Anthropic OAuth credential files"
        );
    }
    match AnthropicAccountPool::new(loaded.accounts.clone()) {
        Ok(accounts) => OAuthAccounts::Ready(Arc::new(accounts)),
        Err(_) => OAuthAccounts::Unavailable(io::ErrorKind::InvalidData),
    }
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
