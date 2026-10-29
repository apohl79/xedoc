use std::sync::Arc;

use xedoc_api::AllowedCaller;
use xedoc_api::ApproximateLocation;
use xedoc_api::ExternalWebAccess;
use xedoc_api::ExternalWebAccessMode;
use xedoc_api::LocationType;
use xedoc_api::SearchContextSize;
use xedoc_api::SearchFilters;
use xedoc_api::SearchSettings;
use xedoc_core::config::Config;
use xedoc_extension_api::ConfigContributor;
use xedoc_extension_api::ExtensionData;
use xedoc_extension_api::ExtensionFuture;
use xedoc_extension_api::ExtensionRegistryBuilder;
use xedoc_extension_api::ThreadLifecycleContributor;
use xedoc_extension_api::ThreadOriginator;
use xedoc_extension_api::ThreadStartInput;
use xedoc_extension_api::ToolContributor;
use xedoc_http_client::HttpClientFactory;
use xedoc_login::AuthManager;
use xedoc_model_provider::create_model_provider;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_protocol::config_types::WebSearchContextSize;
use xedoc_protocol::config_types::WebSearchMode;

use crate::direct::DirectWebFetchTool;
use crate::direct::DirectWebSearchTool;
use crate::tool::WebSearchTool;

#[derive(Clone)]
struct WebSearchExtension {
    auth_manager: Arc<AuthManager>,
}

#[derive(Clone)]
struct WebSearchExtensionConfig {
    available: bool,
    provider: ModelProviderInfo,
    settings: SearchSettings,
    http_client_factory: HttpClientFactory,
}

impl From<&Config> for WebSearchExtensionConfig {
    fn from(config: &Config) -> Self {
        let web_search_mode = config.web_search_mode.value();
        Self {
            available: web_search_mode != WebSearchMode::Disabled,
            provider: config.model_provider.clone(),
            settings: search_settings(config, web_search_mode),
            http_client_factory: config.http_client_factory(),
        }
    }
}

fn search_settings(config: &Config, web_search_mode: WebSearchMode) -> SearchSettings {
    let web_search_config = config.web_search_config.as_ref();
    SearchSettings {
        user_location: web_search_config
            .and_then(|config| config.user_location.as_ref())
            .map(|location| ApproximateLocation {
                r#type: LocationType::Approximate,
                country: location.country.clone(),
                region: location.region.clone(),
                city: location.city.clone(),
                timezone: location.timezone.clone(),
            }),
        search_context_size: web_search_config
            .and_then(|config| config.search_context_size)
            .map(|size| match size {
                WebSearchContextSize::Low => SearchContextSize::Low,
                WebSearchContextSize::Medium => SearchContextSize::Medium,
                WebSearchContextSize::High => SearchContextSize::High,
            }),
        filters: web_search_config
            .and_then(|config| config.filters.as_ref())
            .map(|filters| SearchFilters {
                allowed_domains: filters.allowed_domains.clone(),
                blocked_domains: None,
            }),
        allowed_callers: Some(vec![AllowedCaller::Direct]),
        external_web_access: Some(external_web_access_for_mode(web_search_mode)),
        ..Default::default()
    }
}

fn external_web_access_for_mode(web_search_mode: WebSearchMode) -> ExternalWebAccess {
    match web_search_mode {
        WebSearchMode::Disabled | WebSearchMode::Cached => ExternalWebAccess::Boolean(false),
        WebSearchMode::Indexed => ExternalWebAccess::Mode(ExternalWebAccessMode::Indexed),
        WebSearchMode::Live => ExternalWebAccess::Boolean(true),
    }
}

impl ThreadLifecycleContributor<Config> for WebSearchExtension {
    fn on_thread_start<'a>(
        &'a self,
        input: ThreadStartInput<'a, Config>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            input
                .thread_store
                .insert(WebSearchExtensionConfig::from(input.config));
        })
    }
}

impl ConfigContributor<Config> for WebSearchExtension {
    fn on_config_changed(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
        _previous_config: &Config,
        new_config: &Config,
    ) {
        thread_store.insert(WebSearchExtensionConfig::from(new_config));
    }
}

impl ToolContributor for WebSearchExtension {
    fn tools(
        &self,
        session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn xedoc_extension_api::ToolExecutor<xedoc_extension_api::ToolCall>>> {
        let Some(config) = thread_store.get::<WebSearchExtensionConfig>() else {
            return Vec::new();
        };
        if !config.available {
            return Vec::new();
        }

        let hosted_web_search = config.provider.namespace_tools
            && (config.provider.is_openai() || config.provider.uses_openai_actor_authorization());
        if hosted_web_search {
            return vec![Arc::new(WebSearchTool {
                session_id: session_store.level_id().to_string(),
                provider: create_model_provider(
                    config.provider.clone(),
                    Some(self.auth_manager.clone()),
                ),
                settings: config.settings.clone(),
                originator: thread_store
                    .get::<ThreadOriginator>()
                    .map(|originator| originator.0.clone()),
            })];
        }

        vec![
            Arc::new(DirectWebSearchTool::new(config.http_client_factory.clone())),
            Arc::new(DirectWebFetchTool::new(config.http_client_factory.clone())),
        ]
    }
}

pub fn install(registry: &mut ExtensionRegistryBuilder<Config>, auth_manager: Arc<AuthManager>) {
    let extension = Arc::new(WebSearchExtension { auth_manager });
    registry.thread_lifecycle_contributor(extension.clone());
    registry.config_contributor(extension.clone());
    registry.tool_contributor(extension);
}
