use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header_regex;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_model_provider_info::GEMINI_PROVIDER_ID;
use xedoc_model_provider_info::built_in_model_providers;
use xedoc_models_manager::manager::RefreshStrategy;
use xedoc_models_manager::model_info::model_info_from_provider_catalog_slug;
use xedoc_protocol::openai_models::ModelVisibility;
use xedoc_protocol::openai_models::ModelsResponse;

use crate::create_model_provider;

#[tokio::test]
async fn gemini_provider_refreshes_native_google_catalog() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(query_param("pageSize", "1000"))
        .and(header_regex("x-goog-api-key", ".+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{
                "name": "models/gemini-3.6-flash",
                "displayName": "Gemini 3.6 Flash",
                "description": "Fast coding model",
                "inputTokenLimit": 1_048_576,
                "supportedGenerationMethods": ["generateContent"]
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut provider_info = built_in_model_providers(/*openai_base_url*/ None)
        .remove(GEMINI_PROVIDER_ID)
        .expect("built-in Gemini provider");
    provider_info.base_url = Some(server.uri());
    provider_info.env_key = Some("PATH".to_string());
    let provider = create_model_provider(provider_info, /*auth_manager*/ None);

    let actual = provider
        .models_manager_without_cache(/*config_model_catalog*/ None)
        .raw_model_catalog(
            RefreshStrategy::Online,
            HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
        )
        .await;
    let mut expected = model_info_from_provider_catalog_slug("gemini-3.6-flash", "Gemini");
    expected.display_name = "Gemini 3.6 Flash".to_string();
    expected.description = Some("Fast coding model".to_string());
    expected.context_window = Some(1_048_576);
    expected.max_context_window = Some(1_048_576);
    expected.priority = 0;
    expected.visibility = ModelVisibility::List;

    assert_eq!(
        actual,
        ModelsResponse {
            models: vec![expected],
        }
    );
}
