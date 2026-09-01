use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header_regex;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_model_provider_info::ModelProviderInfo;
use xedoc_model_provider_info::WireApi;
use xedoc_models_manager::client_version_to_whole;
use xedoc_models_manager::manager::ModelsManager;
use xedoc_models_manager::manager::OpenAiModelsManager;
use xedoc_models_manager::manager::RefreshStrategy;
use xedoc_models_manager::model_info::model_info_from_provider_catalog_slug;
use xedoc_protocol::openai_models::ModelVisibility;
use xedoc_protocol::openai_models::ModelsResponse;

use super::DeepSeekModelsEndpoint;

#[tokio::test]
async fn refreshes_the_authoritative_deepseek_catalog() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("client_version", client_version_to_whole()))
        .and(header_regex("x-api-key", ".+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "deepseek-v4-flash", "owned_by": "deepseek"},
                {"id": "unrelated-model", "owned_by": "other"}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let endpoint = DeepSeekModelsEndpoint::new(
        ModelProviderInfo {
            name: "DeepSeek".to_string(),
            base_url: Some(format!("{}/anthropic/v1", server.uri())),
            env_key: Some("PATH".to_string()),
            wire_api: WireApi::Anthropic,
            ..ModelProviderInfo::default()
        },
        /*auth_manager*/ None,
    );

    let manager =
        OpenAiModelsManager::new_without_cache(Arc::new(endpoint), /*auth_manager*/ None);
    let actual = manager
        .raw_model_catalog(
            RefreshStrategy::Online,
            HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
        )
        .await;
    let mut expected = model_info_from_provider_catalog_slug("deepseek-v4-flash", "DeepSeek");
    expected.priority = 0;
    expected.visibility = ModelVisibility::List;

    assert_eq!(
        actual,
        ModelsResponse {
            models: vec![expected],
        }
    );
}
