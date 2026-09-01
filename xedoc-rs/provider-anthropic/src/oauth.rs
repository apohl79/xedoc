use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;
use xedoc_utils_path::write_atomically;

const STICKY_ACCOUNT_DURATION: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicOAuthCredential {
    pub access_token: String,
    pub refresh_token: String,
    pub email: String,
    pub expires_at: String,
    pub account_id: String,
    pub last_refresh_at: Option<String>,
}

impl fmt::Debug for AnthropicOAuthCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicOAuthCredential")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("email", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("account_id", &"<redacted>")
            .field("last_refresh_at", &self.last_refresh_at)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicOAuthAccount {
    pub credential: AnthropicOAuthCredential,
    pub source_path: PathBuf,
}

impl fmt::Debug for AnthropicOAuthAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicOAuthAccount")
            .field("credential", &self.credential)
            .field("source_path", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicCredentialLoad {
    pub accounts: Vec<AnthropicOAuthAccount>,
    pub failures: Vec<AnthropicCredentialLoadFailure>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicCredentialLoadFailure {
    pub path: PathBuf,
    pub message: String,
}

impl fmt::Debug for AnthropicCredentialLoadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicCredentialLoadFailure")
            .field("path", &"<redacted>")
            .field("message", &self.message)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicAccountFailureKind {
    RateLimit,
    Auth,
    Forbidden,
    Server,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicAccountUnavailable {
    pub failure_kind: Option<AnthropicAccountFailureKind>,
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicAccountPoolError {
    DuplicateAccount,
}

impl fmt::Display for AnthropicAccountPoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateAccount => f.write_str("duplicate Anthropic OAuth account"),
        }
    }
}

impl std::error::Error for AnthropicAccountPoolError {}

pub struct AnthropicAccountPool {
    state: Mutex<PoolState>,
}

impl fmt::Debug for AnthropicAccountPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f.debug_struct("AnthropicAccountPool")
            .field("account_count", &state.accounts.len())
            .finish()
    }
}

struct PoolState {
    accounts: Vec<AccountState>,
    current_index: Option<usize>,
    sticky_until: Instant,
}

struct AccountState {
    account: AnthropicOAuthAccount,
    refresh_lock: Arc<AsyncMutex<()>>,
    cooldown_until: Instant,
    failure_count: u32,
    last_failure_kind: Option<AnthropicAccountFailureKind>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredAnthropicOAuthCredential {
    access_token: String,
    refresh_token: String,
    email: String,
    #[serde(alias = "expired")]
    expires_at: String,
    #[serde(default, alias = "account_uuid")]
    account_id: String,
    #[serde(default, alias = "last_refresh")]
    last_refresh_at: Option<String>,
    #[serde(default, rename = "type")]
    credential_type: Option<String>,
}

impl AnthropicAccountPool {
    pub fn new(accounts: Vec<AnthropicOAuthAccount>) -> Result<Self, AnthropicAccountPoolError> {
        let mut emails = std::collections::HashSet::new();
        for account in &accounts {
            if !emails.insert(account.credential.email.clone()) {
                return Err(AnthropicAccountPoolError::DuplicateAccount);
            }
        }

        let now = Instant::now();
        Ok(Self {
            state: Mutex::new(PoolState {
                accounts: accounts
                    .into_iter()
                    .map(|account| AccountState {
                        account,
                        refresh_lock: Arc::new(AsyncMutex::new(())),
                        cooldown_until: now,
                        failure_count: 0,
                        last_failure_kind: None,
                    })
                    .collect(),
                current_index: None,
                sticky_until: now,
            }),
        })
    }

    pub fn account_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accounts
            .len()
    }

    pub fn select(&self) -> Result<AnthropicOAuthAccount, AnthropicAccountUnavailable> {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(index) = state.current_index
            && now < state.sticky_until
            && state.accounts[index].cooldown_until <= now
        {
            return Ok(state.accounts[index].account.clone());
        }

        let count = state.accounts.len();
        let start = state
            .current_index
            .map_or(0, |index| (index + 1) % count.max(1));
        for offset in 0..count {
            let index = (start + offset) % count;
            if state.accounts[index].cooldown_until <= now {
                state.current_index = Some(index);
                state.sticky_until = now + STICKY_ACCOUNT_DURATION;
                return Ok(state.accounts[index].account.clone());
            }
        }

        Err(unavailable_account(&state, now))
    }

    pub fn record_failure(&self, source_path: &Path, kind: AnthropicAccountFailureKind) -> bool {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = state
            .accounts
            .iter()
            .position(|account| account.account.source_path == source_path)
        else {
            return false;
        };

        let account = &mut state.accounts[index];
        account.failure_count = account.failure_count.saturating_add(1);
        account.last_failure_kind = Some(kind);
        account.cooldown_until = now + failure_backoff(kind, account.failure_count);
        if state.current_index == Some(index) {
            state.sticky_until = now;
        }
        true
    }

    pub fn record_success(&self, source_path: &Path) -> bool {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(account) = state
            .accounts
            .iter_mut()
            .find(|account| account.account.source_path == source_path)
        else {
            return false;
        };

        account.cooldown_until = now;
        account.failure_count = 0;
        account.last_failure_kind = None;
        true
    }

    pub fn credential(&self, source_path: &Path) -> Option<AnthropicOAuthCredential> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accounts
            .iter()
            .find(|account| account.account.source_path == source_path)
            .map(|account| account.account.credential.clone())
    }

    pub fn update_credential(
        &self,
        source_path: &Path,
        credential: AnthropicOAuthCredential,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(account) = state
            .accounts
            .iter_mut()
            .find(|account| account.account.source_path == source_path)
        else {
            return false;
        };
        account.account.credential = credential;
        true
    }

    pub fn refresh_lock(&self, source_path: &Path) -> Option<Arc<AsyncMutex<()>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accounts
            .iter()
            .find(|account| account.account.source_path == source_path)
            .map(|account| Arc::clone(&account.refresh_lock))
    }

    pub fn has_available_account(&self) -> bool {
        let now = Instant::now();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accounts
            .iter()
            .any(|account| account.cooldown_until <= now)
    }
}

pub fn load_anthropic_oauth_credentials(directory: &Path) -> io::Result<AnthropicCredentialLoad> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AnthropicCredentialLoad {
                accounts: Vec::new(),
                failures: Vec::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_anthropic_credential_path(path))
        .collect::<Vec<_>>();
    paths.sort();

    let mut accounts = Vec::new();
    let mut failures = Vec::new();
    for path in paths {
        match load_credential(&path) {
            Ok(credential) => accounts.push(AnthropicOAuthAccount {
                credential,
                source_path: path,
            }),
            Err(error) => failures.push(AnthropicCredentialLoadFailure {
                path,
                message: error.to_string(),
            }),
        }
    }
    Ok(AnthropicCredentialLoad { accounts, failures })
}

pub fn persist_anthropic_oauth_credential(
    path: &Path,
    credential: &AnthropicOAuthCredential,
) -> io::Result<()> {
    let stored = StoredAnthropicOAuthCredential {
        access_token: credential.access_token.clone(),
        refresh_token: credential.refresh_token.clone(),
        email: credential.email.clone(),
        expires_at: credential.expires_at.clone(),
        account_id: credential.account_id.clone(),
        last_refresh_at: credential.last_refresh_at.clone(),
        credential_type: Some("anthropic".to_string()),
    };
    let contents = serde_json::to_string_pretty(&stored)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomically(path, &contents)
}

fn is_anthropic_credential_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    path.is_file()
        && name.ends_with(".json")
        && (name.starts_with("anthropic-") || name.starts_with("claude-"))
}

fn load_credential(path: &Path) -> io::Result<AnthropicOAuthCredential> {
    let contents = fs::read_to_string(path)?;
    let stored: StoredAnthropicOAuthCredential = serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored
        .credential_type
        .as_deref()
        .is_some_and(|kind| kind != "anthropic" && kind != "claude")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "credential type must be `anthropic` or `claude`",
        ));
    }
    for (field, value) in [
        ("access_token", stored.access_token.as_str()),
        ("refresh_token", stored.refresh_token.as_str()),
        ("email", stored.email.as_str()),
        ("expires_at", stored.expires_at.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{field} must not be empty"),
            ));
        }
    }

    Ok(AnthropicOAuthCredential {
        access_token: stored.access_token,
        refresh_token: stored.refresh_token,
        account_id: if stored.account_id.is_empty() {
            stored.email.clone()
        } else {
            stored.account_id
        },
        email: stored.email,
        expires_at: stored.expires_at,
        last_refresh_at: stored.last_refresh_at,
    })
}

fn unavailable_account(state: &PoolState, now: Instant) -> AnthropicAccountUnavailable {
    let selected = state.accounts.iter().min_by_key(|account| {
        (
            failure_priority(
                account
                    .last_failure_kind
                    .unwrap_or(AnthropicAccountFailureKind::Network),
            ),
            account.cooldown_until.saturating_duration_since(now),
        )
    });
    let failure_kind = selected.and_then(|account| account.last_failure_kind);
    let retry_after = selected.and_then(|account| {
        let kind = account.last_failure_kind?;
        (!matches!(
            kind,
            AnthropicAccountFailureKind::Auth | AnthropicAccountFailureKind::Forbidden
        ))
        .then(|| account.cooldown_until.saturating_duration_since(now))
    });
    AnthropicAccountUnavailable {
        failure_kind,
        retry_after,
    }
}

fn failure_priority(kind: AnthropicAccountFailureKind) -> u8 {
    match kind {
        AnthropicAccountFailureKind::RateLimit => 0,
        AnthropicAccountFailureKind::Server => 1,
        AnthropicAccountFailureKind::Network => 2,
        AnthropicAccountFailureKind::Forbidden => 3,
        AnthropicAccountFailureKind::Auth => 4,
    }
}

fn failure_backoff(kind: AnthropicAccountFailureKind, failure_count: u32) -> Duration {
    let (base, max) = match kind {
        AnthropicAccountFailureKind::RateLimit => {
            (Duration::from_secs(60), Duration::from_secs(900))
        }
        AnthropicAccountFailureKind::Auth | AnthropicAccountFailureKind::Forbidden => {
            (Duration::from_secs(600), Duration::from_secs(3600))
        }
        AnthropicAccountFailureKind::Server | AnthropicAccountFailureKind::Network => {
            (Duration::from_secs(5), Duration::from_secs(300))
        }
    };
    base.saturating_mul(2u32.saturating_pow(failure_count.saturating_sub(1)))
        .min(max)
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
