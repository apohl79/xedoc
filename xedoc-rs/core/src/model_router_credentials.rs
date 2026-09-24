//! Encrypted credential storage for router-owned external classifiers.

use std::path::Path;

use xedoc_secrets::SecretName;
use xedoc_secrets::SecretScope;
use xedoc_secrets::SecretsBackendKind;
use xedoc_secrets::SecretsManager;

const JEV_API_KEY_SECRET: &str = "MODEL_ROUTER_JEV_API_KEY";

pub(crate) fn load_jev_api_key(xedoc_home: &Path) -> Option<String> {
    let secret_name = SecretName::new(JEV_API_KEY_SECRET).ok()?;
    SecretsManager::new(xedoc_home.to_path_buf(), SecretsBackendKind::Local)
        .get(&SecretScope::Global, &secret_name)
        .ok()
        .flatten()
}

pub(crate) fn store_jev_api_key(xedoc_home: &Path, api_key: &str) -> Result<(), ()> {
    let secret_name = SecretName::new(JEV_API_KEY_SECRET).map_err(|_| ())?;
    SecretsManager::new(xedoc_home.to_path_buf(), SecretsBackendKind::Local)
        .set(&SecretScope::Global, &secret_name, api_key)
        .map_err(|_| ())
}
