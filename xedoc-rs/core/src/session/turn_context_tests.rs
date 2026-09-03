use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use xedoc_api::AuthProvider;
use xedoc_keyring_store::tests::MockKeyringStore;
use xedoc_login::ProviderCredentialStore;
use xedoc_model_provider::ModelProvider;
use xedoc_model_provider_info::ANTHROPIC_PROVIDER_ID;
use xedoc_model_provider_info::built_in_model_providers;

#[tokio::test]
async fn turn_provider_uses_stored_key_for_selected_anthropic_provider() {
    let xedoc_home = tempfile::tempdir().expect("create Xedoc home");
    let credential_store = ProviderCredentialStore::new_with_keyring_store(
        xedoc_home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    credential_store
        .set_api_key(ANTHROPIC_PROVIDER_ID, "stored-anthropic-key")
        .expect("store Anthropic key");
    let auth_manager = AuthManager::from_auth_for_testing_with_provider_credentials(
        XedocAuth::from_api_key("unused-openai-key"),
        xedoc_home.path().to_path_buf(),
        credential_store,
    );
    let provider_info = built_in_model_providers(/*openai_base_url*/ None)
        .remove(ANTHROPIC_PROVIDER_ID)
        .expect("built-in Anthropic provider");

    let provider = create_turn_model_provider(
        ANTHROPIC_PROVIDER_ID.to_string(),
        provider_info,
        Some(auth_manager),
    );

    assert_eq!(
        provider
            .api_auth()
            .await
            .expect("provider authentication")
            .to_auth_headers()
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("stored-anthropic-key")
    );
}
