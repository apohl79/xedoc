use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use super::ANTHROPIC_ACCOUNTS_PATH;
use super::ANTHROPIC_PROVIDER_ID;
use super::AnthropicOAuthCredential;
use super::AnthropicOauthLoginState;
use super::ModelManagerRequestProcessor;
use super::ProviderCredentialStore;
use super::complete_anthropic_oauth_login;
use super::replace_anthropic_oauth_with_api_key;
use pretty_assertions::assert_eq;
use serial_test::serial;
use tempfile::TempDir;
use xedoc_app_server_protocol::ModelProviderApiKeySetParams;
use xedoc_app_server_protocol::ModelProviderOauthDeleteParams;
use xedoc_app_server_protocol::ModelProviderOauthStartParams;
use xedoc_app_server_protocol::ModelProviderOauthStartResponse;
use xedoc_core::config::ConfigBuilder;
use xedoc_keyring_store::tests::MockKeyringStore;
use xedoc_login::AuthManager;
use xedoc_login::XedocAuth;
use xedoc_provider_anthropic::load_anthropic_oauth_credentials;
use xedoc_provider_anthropic::store_anthropic_oauth_credential;

fn credentials_for_test(xedoc_home: &std::path::Path) -> ProviderCredentialStore {
    ProviderCredentialStore::new_with_keyring_store(
        xedoc_home.to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    )
}

fn anthropic_credential() -> AnthropicOAuthCredential {
    AnthropicOAuthCredential {
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        email: "user@example.com".to_string(),
        expires_at: "2030-01-01T00:00:00.000Z".to_string(),
        account_id: "account-id".to_string(),
        last_refresh_at: None,
    }
}

fn anthropic_accounts_directory(xedoc_home: &std::path::Path) -> PathBuf {
    ANTHROPIC_ACCOUNTS_PATH
        .into_iter()
        .fold(PathBuf::from(xedoc_home), |path, component| {
            path.join(component)
        })
}

async fn processor_for_test() -> anyhow::Result<(TempDir, ModelManagerRequestProcessor)> {
    let xedoc_home = tempfile::tempdir()?;
    let config = ConfigBuilder::default()
        .xedoc_home(xedoc_home.path().to_path_buf())
        .fallback_cwd(Some(xedoc_home.path().to_path_buf()))
        .build()
        .await?;
    let auth_manager = AuthManager::from_auth_for_testing_with_provider_credentials(
        XedocAuth::from_api_key("unused-openai-api-key"),
        xedoc_home.path().to_path_buf(),
        credentials_for_test(xedoc_home.path()),
    );
    Ok((
        xedoc_home,
        ModelManagerRequestProcessor::new(Arc::new(config), auth_manager),
    ))
}

#[tokio::test]
async fn repeated_anthropic_oauth_start_reuses_the_active_login() -> anyhow::Result<()> {
    let (_xedoc_home, processor) = processor_for_test().await?;
    let expected = ModelProviderOauthStartResponse {
        auth_url: "https://example.com/existing-login".to_string(),
    };
    processor
        .anthropic_oauth_login_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .active_auth_url = Some(expected.auth_url.clone());

    let actual = processor
        .start_oauth(ModelProviderOauthStartParams {
            provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;

    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test]
#[serial(login_port)]
async fn credential_changes_release_the_anthropic_oauth_callback_port() -> anyhow::Result<()> {
    let (_xedoc_home, processor) = processor_for_test().await?;
    let oauth_params = || ModelProviderOauthStartParams {
        provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
    };
    let first = processor
        .start_oauth(oauth_params())
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;

    processor
        .delete_oauth(ModelProviderOauthDeleteParams {
            provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let second = processor
        .start_oauth(oauth_params())
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;

    processor
        .set_api_key(ModelProviderApiKeySetParams {
            provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
            api_key: "api-key".to_string(),
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let third = processor
        .start_oauth(oauth_params())
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;

    assert_ne!(first.auth_url, second.auth_url);
    assert_ne!(second.auth_url, third.auth_url);
    processor
        .delete_oauth(ModelProviderOauthDeleteParams {
            provider_id: ANTHROPIC_PROVIDER_ID.to_string(),
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.message))?;
    Ok(())
}

#[test]
fn completing_anthropic_oauth_login_removes_the_api_key() -> Result<(), Box<dyn std::error::Error>>
{
    let xedoc_home = tempfile::tempdir()?;
    let credentials = credentials_for_test(xedoc_home.path());
    credentials.set_api_key(ANTHROPIC_PROVIDER_ID, "api-key")?;
    let credential = anthropic_credential();
    let accounts_directory = anthropic_accounts_directory(xedoc_home.path());
    let oauth_login_state = Mutex::new(AnthropicOauthLoginState {
        generation: 1,
        ..Default::default()
    });

    assert!(complete_anthropic_oauth_login(
        &accounts_directory,
        &credentials,
        &oauth_login_state,
        /*oauth_login_generation*/ 1,
        &credential,
    )?);

    assert_eq!(credentials.api_key(ANTHROPIC_PROVIDER_ID)?, None);
    assert_eq!(
        load_anthropic_oauth_credentials(&accounts_directory)?.accounts,
        vec![xedoc_provider_anthropic::AnthropicOAuthAccount {
            credential,
            source_path: accounts_directory.join("anthropic-user@example.com.json"),
        }]
    );
    Ok(())
}

#[test]
fn setting_anthropic_api_key_removes_the_oauth_login() -> Result<(), Box<dyn std::error::Error>> {
    let xedoc_home = tempfile::tempdir()?;
    let credentials = credentials_for_test(xedoc_home.path());
    let accounts_directory = anthropic_accounts_directory(xedoc_home.path());
    let oauth_login_state = Mutex::new(AnthropicOauthLoginState::default());
    store_anthropic_oauth_credential(&accounts_directory, &anthropic_credential())?;

    replace_anthropic_oauth_with_api_key(
        &accounts_directory,
        &credentials,
        &oauth_login_state,
        "api-key",
    )?;

    assert_eq!(
        credentials.api_key(ANTHROPIC_PROVIDER_ID)?,
        Some("api-key".to_string())
    );
    assert!(
        load_anthropic_oauth_credentials(&accounts_directory)?
            .accounts
            .is_empty()
    );
    Ok(())
}

#[test]
fn failed_anthropic_oauth_removal_restores_the_previous_api_key()
-> Result<(), Box<dyn std::error::Error>> {
    let xedoc_home = tempfile::tempdir()?;
    let credentials = credentials_for_test(xedoc_home.path());
    let oauth_login_state = Mutex::new(AnthropicOauthLoginState::default());
    let accounts_path = xedoc_home.path().join("not-a-directory");
    std::fs::write(&accounts_path, "not a directory")?;
    credentials.set_api_key(ANTHROPIC_PROVIDER_ID, "previous-api-key")?;

    let error = replace_anthropic_oauth_with_api_key(
        &accounts_path,
        &credentials,
        &oauth_login_state,
        "new-api-key",
    )
    .expect_err("OAuth removal should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
    assert_eq!(
        credentials.api_key(ANTHROPIC_PROVIDER_ID)?,
        Some("previous-api-key".to_string())
    );
    Ok(())
}

#[test]
fn stale_anthropic_oauth_login_cannot_replace_a_newer_api_key()
-> Result<(), Box<dyn std::error::Error>> {
    let xedoc_home = tempfile::tempdir()?;
    let credentials = credentials_for_test(xedoc_home.path());
    let accounts_directory = anthropic_accounts_directory(xedoc_home.path());
    let oauth_login_state = Mutex::new(AnthropicOauthLoginState {
        generation: 2,
        ..Default::default()
    });
    credentials.set_api_key(ANTHROPIC_PROVIDER_ID, "api-key")?;

    assert!(!complete_anthropic_oauth_login(
        &accounts_directory,
        &credentials,
        &oauth_login_state,
        /*oauth_login_generation*/ 1,
        &anthropic_credential(),
    )?);

    assert_eq!(
        credentials.api_key(ANTHROPIC_PROVIDER_ID)?,
        Some("api-key".to_string())
    );
    assert!(
        load_anthropic_oauth_credentials(&accounts_directory)?
            .accounts
            .is_empty()
    );
    Ok(())
}
