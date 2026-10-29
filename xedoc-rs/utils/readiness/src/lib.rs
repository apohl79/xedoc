//! Readiness flag with token-based authorization and async waiting (Tokio).

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::watch;
use tokio::time;

/// Opaque subscription token returned by `subscribe()`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Token(i32);

const LOCK_TIMEOUT: Duration = Duration::from_millis(1000);

pub trait Readiness: Send + Sync + 'static {
    /// Returns true if the flag is currently marked ready. At least one token needs to be marked
    /// as ready before.
    /// `true` is not reversible.
    fn is_ready(&self) -> bool;

    /// Subscribe to readiness and receive an authorization token.
    ///
    /// If the flag is already ready, returns `FlagAlreadyReady`.
    fn subscribe(
        &self,
    ) -> impl std::future::Future<Output = Result<Token, errors::ReadinessError>> + Send;

    /// Attempt to mark the flag ready, validated by the provided token.
    ///
    /// Returns `true` iff:
    /// - `token` is currently subscribed, and
    /// - the flag was not already ready.
    fn mark_ready(
        &self,
        token: Token,
    ) -> impl std::future::Future<Output = Result<bool, errors::ReadinessError>> + Send;

    /// Asynchronously wait until the flag becomes ready.
    fn wait_ready(&self) -> impl std::future::Future<Output = ()> + Send;
}

pub struct ReadinessFlag {
    /// Atomic for cheap reads.
    ready: AtomicBool,
    /// Used to generate the next i32 token.
    next_id: AtomicI32,
    /// Set of active subscriptions.
    tokens: Mutex<HashSet<Token>>,
    /// Broadcasts readiness to async waiters.
    tx: watch::Sender<bool>,
}

impl ReadinessFlag {
    /// Create a new, not-yet-ready flag.
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            ready: AtomicBool::new(false),
            next_id: AtomicI32::new(1), // Reserve 0.
            tokens: Mutex::new(HashSet::new()),
            tx,
        }
    }

    async fn with_tokens<R>(
        &self,
        f: impl FnOnce(&mut HashSet<Token>) -> R,
    ) -> Result<R, errors::ReadinessError> {
        let mut guard = time::timeout(LOCK_TIMEOUT, self.tokens.lock())
            .await
            .map_err(|_| errors::ReadinessError::TokenLockFailed)?;
        Ok(f(&mut guard))
    }

    fn load_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

impl Default for ReadinessFlag {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ReadinessFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadinessFlag")
            .field("ready", &self.load_ready())
            .finish()
    }
}

impl Readiness for ReadinessFlag {
    fn is_ready(&self) -> bool {
        if self.load_ready() {
            return true;
        }

        if let Ok(tokens) = self.tokens.try_lock()
            && tokens.is_empty()
        {
            let was_ready = self.ready.swap(true, Ordering::AcqRel);
            drop(tokens);
            if !was_ready {
                let _ = self.tx.send(true);
            }
            return true;
        }

        self.load_ready()
    }

    async fn subscribe(&self) -> Result<Token, errors::ReadinessError> {
        if self.load_ready() {
            return Err(errors::ReadinessError::FlagAlreadyReady);
        }

        // Recheck readiness while holding the lock so mark_ready can't flip the flag between the
        // check above and inserting the token. Also ensure the token is non-zero and unique in
        // the presence of `i32` wrap-around.
        let token = self
            .with_tokens(|tokens| {
                if self.load_ready() {
                    return None;
                }

                loop {
                    let token = Token(self.next_id.fetch_add(1, Ordering::Relaxed));
                    if token.0 != 0 && tokens.insert(token) {
                        return Some(token);
                    }
                }
            })
            .await?;

        token.ok_or(errors::ReadinessError::FlagAlreadyReady)
    }

    async fn mark_ready(&self, token: Token) -> Result<bool, errors::ReadinessError> {
        if self.load_ready() {
            return Ok(false);
        }
        if token.0 == 0 {
            return Ok(false); // Never authorize.
        }

        let marked = self
            .with_tokens(|set| {
                if !set.remove(&token) {
                    return false; // invalid or already used
                }
                self.ready.store(true, Ordering::Release);
                set.clear(); // no further tokens needed once ready
                true
            })
            .await?;
        if !marked {
            return Ok(false);
        }
        // Best-effort broadcast; ignore error if there are no receivers.
        let _ = self.tx.send(true);
        Ok(true)
    }

    async fn wait_ready(&self) {
        if self.is_ready() {
            return;
        }
        let mut rx = self.tx.subscribe();
        // Fast-path check before awaiting.
        if *rx.borrow() {
            return;
        }
        // Await changes until true is observed.
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                break;
            }
        }
    }
}

mod errors {
    use thiserror::Error;

    #[derive(Debug, Error)]
    pub enum ReadinessError {
        #[error("Failed to acquire readiness token lock")]
        TokenLockFailed,
        #[error("Flag is already ready. Impossible to subscribe")]
        FlagAlreadyReady,
    }
}
