use std::collections::HashSet;
use std::fmt;
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

/// Describes whether a failed OAuth credential write may have reached disk.
#[derive(Debug)]
pub enum AnthropicOAuthCredentialStoreError {
    DestinationUnchanged(io::Error),
    MayHavePersisted(io::Error),
}

impl AnthropicOAuthCredentialStoreError {
    pub fn destination_is_unchanged(&self) -> bool {
        matches!(self, Self::DestinationUnchanged(_))
    }

    pub fn into_io_error(self) -> io::Error {
        match self {
            Self::DestinationUnchanged(error) | Self::MayHavePersisted(error) => error,
        }
    }
}

impl fmt::Display for AnthropicOAuthCredentialStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationUnchanged(error) | Self::MayHavePersisted(error) => {
                error.fmt(formatter)
            }
        }
    }
}

impl std::error::Error for AnthropicOAuthCredentialStoreError {}

/// Describes whether a failed OAuth import left the destination unchanged.
#[derive(Debug)]
pub enum AnthropicOAuthImportError {
    RolledBack(io::Error),
    RollbackFailed(io::Error),
}

impl AnthropicOAuthImportError {
    pub fn destination_is_unchanged(&self) -> bool {
        matches!(self, Self::RolledBack(_))
    }

    pub fn into_io_error(self) -> io::Error {
        match self {
            Self::RolledBack(error) | Self::RollbackFailed(error) => error,
        }
    }
}

impl fmt::Display for AnthropicOAuthImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RolledBack(error) | Self::RollbackFailed(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AnthropicOAuthImportError {}

pub fn import_anthropic_oauth_credentials(
    source: &Path,
    destination_directory: &Path,
) -> io::Result<usize> {
    import_anthropic_oauth_credentials_with_rollback_status(source, destination_directory)
        .map_err(AnthropicOAuthImportError::into_io_error)
}

pub fn import_anthropic_oauth_credentials_with_rollback_status(
    source: &Path,
    destination_directory: &Path,
) -> Result<usize, AnthropicOAuthImportError> {
    let accounts = load_source_accounts(source).map_err(AnthropicOAuthImportError::RolledBack)?;
    validate_source_accounts(&accounts).map_err(AnthropicOAuthImportError::RolledBack)?;
    let imports = plan_imports(accounts, destination_directory)
        .map_err(AnthropicOAuthImportError::RolledBack)?;
    let mut applied_imports: Vec<PlannedImport> = Vec::new();
    for import in imports {
        if let Err(import_error) =
            persist_anthropic_oauth_credential(&import.destination, &import.credential)
        {
            for rollback_import in std::iter::once(&import).chain(applied_imports.iter().rev()) {
                let rollback = match &rollback_import.previous_credential {
                    Some(credential) => {
                        persist_anthropic_oauth_credential(&rollback_import.destination, credential)
                    }
                    None => match fs::remove_file(&rollback_import.destination) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(error),
                    },
                };
                if let Err(rollback_error) = rollback {
                    return Err(AnthropicOAuthImportError::RollbackFailed(io::Error::other(
                        format!(
                            "failed to import Anthropic OAuth credentials: {import_error}; failed to roll back {}: {rollback_error}",
                            rollback_import.destination.display()
                        ),
                    )));
                }
            }
            return Err(AnthropicOAuthImportError::RolledBack(import_error));
        }
        applied_imports.push(import);
    }
    Ok(applied_imports.len())
}

pub fn store_anthropic_oauth_credential(
    destination_directory: &Path,
    credential: &AnthropicOAuthCredential,
) -> io::Result<()> {
    store_anthropic_oauth_credential_with_status(destination_directory, credential)
        .map_err(AnthropicOAuthCredentialStoreError::into_io_error)
}

pub fn store_anthropic_oauth_credential_with_status(
    destination_directory: &Path,
    credential: &AnthropicOAuthCredential,
) -> Result<(), AnthropicOAuthCredentialStoreError> {
    let destination = destination_directory.join(credential_file_name(&credential.email));
    validate_destination(&destination, credential)
        .map_err(AnthropicOAuthCredentialStoreError::DestinationUnchanged)?;
    persist_anthropic_oauth_credential(&destination, credential)
        .map_err(AnthropicOAuthCredentialStoreError::MayHavePersisted)
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
) -> io::Result<Vec<PlannedImport>> {
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
                let previous_credential = destination_credential(&destination)?;
                if previous_credential
                    .as_ref()
                    .is_some_and(|credential| credential.email != account.credential.email)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "an Anthropic credential filename is already used by another account",
                    ));
                }
                imports.push(PlannedImport {
                    destination,
                    credential: account.credential,
                    previous_credential,
                });
                Ok((destinations, imports))
            },
        )
        .map(|(_, imports)| imports)
}

fn validate_destination(
    destination: &Path,
    credential: &AnthropicOAuthCredential,
) -> io::Result<()> {
    let Some(existing) = destination_credential(destination)? else {
        return Ok(());
    };
    if existing.email == credential.email {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "an Anthropic credential filename is already used by another account",
    ))
}

fn destination_credential(destination: &Path) -> io::Result<Option<AnthropicOAuthCredential>> {
    if !destination.exists() {
        return Ok(None);
    }
    load_anthropic_oauth_credential(destination).map(Some)
}

struct PlannedImport {
    destination: PathBuf,
    credential: AnthropicOAuthCredential,
    previous_credential: Option<AnthropicOAuthCredential>,
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
