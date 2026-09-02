use std::sync::Arc;

use pretty_assertions::assert_eq;
use tempfile::TempDir;
use xedoc_keyring_store::tests::MockKeyringStore;

use super::ProviderCredentialStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn store(home: &TempDir) -> ProviderCredentialStore {
    ProviderCredentialStore::new_with_keyring_store(
        home.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    )
}

#[test]
fn provider_api_keys_round_trip_and_delete_independently() -> TestResult {
    let home = TempDir::new()?;
    let store = store(&home);

    store.set_api_key("anthropic", "anthropic-secret")?;
    store.set_api_key("google", "google-secret")?;

    assert_eq!(
        store.api_key("anthropic")?,
        Some("anthropic-secret".to_string())
    );
    assert_eq!(store.api_key("google")?, Some("google-secret".to_string()));
    assert!(store.delete_api_key("anthropic")?);
    assert_eq!(store.api_key("anthropic")?, None);
    assert_eq!(store.api_key("google")?, Some("google-secret".to_string()));
    Ok(())
}

#[test]
fn provider_api_key_is_encrypted_on_disk() -> TestResult {
    let home = TempDir::new()?;
    let store = store(&home);

    store.set_api_key("deepseek", "not-plaintext")?;

    let ciphertext = std::fs::read(home.path().join("secrets").join("local.age"))?;
    assert!(
        !String::from_utf8_lossy(&ciphertext).contains("not-plaintext"),
        "encrypted credentials file must not contain the plaintext API key"
    );
    Ok(())
}

#[test]
fn blank_provider_or_key_is_rejected() {
    let home = TempDir::new().expect("tempdir should be created");
    let store = store(&home);

    let provider_error = store
        .set_api_key("", "key")
        .expect_err("blank provider should fail");
    let key_error = store
        .set_api_key("google", " ")
        .expect_err("blank key should fail");

    assert_eq!(provider_error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(key_error.kind(), std::io::ErrorKind::InvalidInput);
}
