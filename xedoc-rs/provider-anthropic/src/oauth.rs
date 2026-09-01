use serde::Deserialize;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

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
            .field("email", &self.email)
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .field("last_refresh_at", &self.last_refresh_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicCredentialLoad {
    pub credentials: Vec<AnthropicOAuthCredential>,
    pub failures: Vec<AnthropicCredentialLoadFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicCredentialLoadFailure {
    pub path: PathBuf,
    pub message: String,
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
    DuplicateEmail(String),
}

impl fmt::Display for AnthropicAccountPoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateEmail(email) => {
                write!(f, "duplicate Anthropic OAuth account email: {email}")
            }
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
    credential: AnthropicOAuthCredential,
    cooldown_until: Instant,
    failure_count: u32,
    last_failure_kind: Option<AnthropicAccountFailureKind>,
}

#[derive(Deserialize)]
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
    pub fn new(
        credentials: Vec<AnthropicOAuthCredential>,
    ) -> Result<Self, AnthropicAccountPoolError> {
        let mut emails = std::collections::HashSet::new();
        for credential in &credentials {
            if !emails.insert(credential.email.clone()) {
                return Err(AnthropicAccountPoolError::DuplicateEmail(
                    credential.email.clone(),
                ));
            }
        }

        let now = Instant::now();
        Ok(Self {
            state: Mutex::new(PoolState {
                accounts: credentials
                    .into_iter()
                    .map(|credential| AccountState {
                        credential,
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

    pub fn select(&self) -> Result<AnthropicOAuthCredential, AnthropicAccountUnavailable> {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(index) = state.current_index
            && now < state.sticky_until
            && state.accounts[index].cooldown_until <= now
        {
            return Ok(state.accounts[index].credential.clone());
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
                return Ok(state.accounts[index].credential.clone());
            }
        }

        Err(unavailable_account(&state, now))
    }

    pub fn record_failure(&self, email: &str, kind: AnthropicAccountFailureKind) -> bool {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(index) = state
            .accounts
            .iter()
            .position(|account| account.credential.email == email)
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

    pub fn record_success(&self, email: &str) -> bool {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(account) = state
            .accounts
            .iter_mut()
            .find(|account| account.credential.email == email)
        else {
            return false;
        };

        account.cooldown_until = now;
        account.failure_count = 0;
        account.last_failure_kind = None;
        true
    }
}

pub fn load_anthropic_oauth_credentials(directory: &Path) -> io::Result<AnthropicCredentialLoad> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AnthropicCredentialLoad {
                credentials: Vec::new(),
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

    let mut credentials = Vec::new();
    let mut failures = Vec::new();
    for path in paths {
        match load_credential(&path) {
            Ok(credential) => credentials.push(credential),
            Err(error) => failures.push(AnthropicCredentialLoadFailure {
                path,
                message: error.to_string(),
            }),
        }
    }
    Ok(AnthropicCredentialLoad {
        credentials,
        failures,
    })
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
