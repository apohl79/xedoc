use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use crate::AnthropicOAuthAccount;
use crate::AnthropicOAuthCredential;
use crate::load_anthropic_oauth_credentials;
use crate::oauth::load_anthropic_oauth_credential;
use crate::persist_anthropic_oauth_credential;

pub fn import_anthropic_oauth_credentials(
    source: &Path,
    destination_directory: &Path,
) -> io::Result<usize> {
    let accounts = load_source_accounts(source)?;
    validate_source_accounts(&accounts)?;
    let imports = plan_imports(accounts, destination_directory)?;
    imports.iter().try_for_each(|(_, credential)| {
        store_anthropic_oauth_credential(destination_directory, credential)
    })?;
    Ok(imports.len())
}

pub fn store_anthropic_oauth_credential(
    destination_directory: &Path,
    credential: &AnthropicOAuthCredential,
) -> io::Result<()> {
    let destination = destination_directory.join(credential_file_name(&credential.email));
    validate_destination(&destination, credential)?;
    persist_anthropic_oauth_credential(&destination, credential)
}

fn load_source_accounts(source: &Path) -> io::Result<Vec<AnthropicOAuthAccount>> {
    let metadata = fs::metadata(source)?;
    if metadata.is_file() {
        return load_anthropic_oauth_credential(source).map(|credential| {
            vec![AnthropicOAuthAccount {
                credential,
                source_path: source.to_path_buf(),
            }]
        });
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Anthropic credential source must be a file or directory",
        ));
    }

    let loaded = load_anthropic_oauth_credentials(source)?;
    let failure_count = loaded
        .failures
        .iter()
        .filter(|failure| !is_legacy_api_key_credential(&failure.path))
        .count();
    if failure_count != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{failure_count} Anthropic credential file(s) could not be loaded"),
        ));
    }
    if loaded.accounts.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no Anthropic OAuth credentials found",
        ));
    }
    Ok(loaded.accounts)
}

fn validate_source_accounts(accounts: &[AnthropicOAuthAccount]) -> io::Result<()> {
    let mut emails = HashSet::new();
    if accounts
        .iter()
        .all(|account| emails.insert(account.credential.email.as_str()))
    {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "source contains duplicate Anthropic accounts",
    ))
}

fn plan_imports(
    accounts: Vec<AnthropicOAuthAccount>,
    destination_directory: &Path,
) -> io::Result<Vec<(PathBuf, AnthropicOAuthCredential)>> {
    accounts
        .into_iter()
        .try_fold(
            (HashSet::new(), Vec::new()),
            |(mut destinations, mut imports), account| {
                let destination =
                    destination_directory.join(credential_file_name(&account.credential.email));
                if !destinations.insert(destination.clone()) {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "multiple Anthropic accounts use the same credential filename",
                    ));
                }
                validate_destination(&destination, &account.credential)?;
                imports.push((destination, account.credential));
                Ok((destinations, imports))
            },
        )
        .map(|(_, imports)| imports)
}

fn validate_destination(
    destination: &Path,
    credential: &AnthropicOAuthCredential,
) -> io::Result<()> {
    if !destination.exists() {
        return Ok(());
    }
    let existing = load_anthropic_oauth_credential(destination)?;
    if existing.email == credential.email {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "an Anthropic credential filename is already used by another account",
    ))
}

fn credential_file_name(email: &str) -> String {
    let sanitized = email
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "@._-".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .replace("..", "_");
    format!("anthropic-{sanitized}.json")
}

fn is_legacy_api_key_credential(path: &Path) -> bool {
    #[derive(Deserialize)]
    struct CredentialKind {
        #[serde(default, alias = "account_uuid")]
        account_id: String,
    }

    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str::<CredentialKind>(&contents).ok())
        .is_some_and(|credential| credential.account_id == "anthropic-api-key")
}

#[cfg(test)]
#[path = "oauth_import_tests.rs"]
mod tests;
