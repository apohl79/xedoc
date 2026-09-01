use super::AnthropicModelProvider;
use crate::provider::ModelProvider;
use crate::provider::ProviderCapabilities;
use crate::provider::create_model_provider;
use pretty_assertions::assert_eq;
use std::fs;
use xedoc_model_provider_info::ANTHROPIC_PROVIDER_ID;
use xedoc_model_provider_info::built_in_model_providers;
use xedoc_protocol::account::ProviderAccount;

fn provider_info() -> xedoc_model_provider_info::ModelProviderInfo {
    built_in_model_providers(/*openai_base_url*/ None)
        .remove(ANTHROPIC_PROVIDER_ID)
        .expect("built-in Anthropic provider")
}

fn write_oauth_credential(directory: &std::path::Path) {
    fs::create_dir_all(directory).expect("create account directory");
    fs::write(
        directory.join("anthropic-user@example.com.json"),
        serde_json::json!({
            "access_token": "oauth-access",
            "refresh_token": "oauth-refresh",
            "email": "user@example.com",
            "expires_at": "2030-01-01T00:00:00.000Z",
            "account_id": "account-id",
            "type": "anthropic"
        })
        .to_string(),
    )
    .expect("write OAuth credential");
}

#[test]
fn factory_selects_native_anthropic_runtime() {
    let provider = create_model_provider(provider_info(), /*auth_manager*/ None);

    assert_eq!(
        provider.capabilities(),
        ProviderCapabilities {
            namespace_tools: false,
            image_generation: false,
            web_search: false,
        }
    );
}

#[tokio::test]
async fn api_key_takes_precedence_over_oauth_account() {
    let home = tempfile::tempdir().expect("tempdir");
    let accounts = home.path().join("accounts");
    write_oauth_credential(&accounts);
    let provider = AnthropicModelProvider::from_runtime_sources(
        provider_info(),
        /*auth_manager*/ None,
        Some("api-key".to_string()),
        Ok(accounts),
    );

    let api_provider = ModelProvider::api_provider(&provider)
        .await
        .expect("API provider");
    let auth = ModelProvider::api_auth(&provider).await.expect("API auth");

    assert_eq!(api_provider.query_params, None);
    assert_eq!(
        auth.to_auth_headers()
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("api-key")
    );
    assert_eq!(
        ModelProvider::account_state(&provider)
            .expect("account state")
            .account,
        Some(ProviderAccount::ApiKey)
    );
}

#[tokio::test]
async fn oauth_account_uses_bearer_profile_and_beta_endpoint() {
    let home = tempfile::tempdir().expect("tempdir");
    let accounts = home.path().join("accounts");
    write_oauth_credential(&accounts);
    let provider = AnthropicModelProvider::from_runtime_sources(
        provider_info(),
        /*auth_manager*/ None,
        /*api_key*/ None,
        Ok(accounts),
    );

    let api_provider = ModelProvider::api_provider(&provider)
        .await
        .expect("API provider");
    let auth = ModelProvider::api_auth(&provider).await.expect("API auth");
    let headers = auth.to_auth_headers();

    assert_eq!(
        api_provider
            .query_params
            .as_ref()
            .and_then(|params| params.get("beta"))
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer oauth-access")
    );
    assert_eq!(
        headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok()),
        Some("oauth-2025-04-20")
    );
    assert_eq!(
        headers
            .get("anthropic-dangerous-direct-browser-access")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
}

#[tokio::test]
async fn missing_native_credentials_reports_both_supported_sources() {
    let home = tempfile::tempdir().expect("tempdir");
    let accounts = home.path().join("accounts");
    let provider = AnthropicModelProvider::from_runtime_sources(
        provider_info(),
        /*auth_manager*/ None,
        /*api_key*/ None,
        Ok(accounts.clone()),
    );

    let error = ModelProvider::api_auth(&provider)
        .await
        .err()
        .expect("missing credentials");
    let message = error.to_string();

    assert!(message.contains("ANTHROPIC_API_KEY"));
    assert!(message.contains(&accounts.display().to_string()));
}
