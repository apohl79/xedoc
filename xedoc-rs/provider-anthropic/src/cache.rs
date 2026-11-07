//! Anthropic prompt-cache breakpoints and automatic cache TTL selection.
//!
//! Anthropic only caches prefixes that end at a `cache_control` marker. Entries live for five
//! minutes by default and are refreshed for free by every hit. A one-hour entry costs more to
//! write, so it only pays off for sessions that idle between requests. A session switches to the
//! one-hour TTL the first time it is seen idling past the default TTL and stays there.

use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

const TTL_MODE_ENV_VAR: &str = "XEDOC_ANTHROPIC_CACHE_TTL";
const IDLE_GAP_FOR_LONG_TTL: Duration = Duration::from_secs(4 * 60);
const SESSION_FORGET_AFTER: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_TRACKED_SESSIONS: usize = 512;

static SESSIONS: LazyLock<Mutex<HashMap<String, SessionCacheState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheTtl {
    FiveMinutes,
    OneHour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheTtlMode {
    Auto,
    Fixed(CacheTtl),
}

struct SessionCacheState {
    last_request: Instant,
    long_ttl: bool,
}

impl CacheTtlMode {
    fn from_env() -> Self {
        match std::env::var(TTL_MODE_ENV_VAR).as_deref() {
            Ok("5m") => Self::Fixed(CacheTtl::FiveMinutes),
            Ok("1h") => Self::Fixed(CacheTtl::OneHour),
            Ok(_) | Err(_) => Self::Auto,
        }
    }
}

/// TTL chosen for a request, and whether this request is the one that switched the session to
/// the one-hour TTL.
pub(crate) struct TtlSelection {
    pub(crate) ttl: CacheTtl,
    pub(crate) switched_to_long: bool,
}

/// Picks the TTL for the next request of `session_key` and records the request time.
pub(crate) fn select_ttl(session_key: Option<&str>) -> TtlSelection {
    select_ttl_at(session_key, CacheTtlMode::from_env(), Instant::now())
}

fn select_ttl_at(session_key: Option<&str>, mode: CacheTtlMode, now: Instant) -> TtlSelection {
    if let CacheTtlMode::Fixed(ttl) = mode {
        return TtlSelection {
            ttl,
            switched_to_long: false,
        };
    }
    let Some(key) = session_key else {
        return TtlSelection {
            ttl: CacheTtl::FiveMinutes,
            switched_to_long: false,
        };
    };
    let mut sessions = SESSIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if sessions.len() >= MAX_TRACKED_SESSIONS {
        sessions.retain(|_, state| now.duration_since(state.last_request) < SESSION_FORGET_AFTER);
    }
    let state = sessions
        .entry(key.to_string())
        .or_insert_with(|| SessionCacheState {
            last_request: now,
            long_ttl: false,
        });
    let switched_to_long =
        !state.long_ttl && now.duration_since(state.last_request) > IDLE_GAP_FOR_LONG_TTL;
    state.long_ttl |= switched_to_long;
    state.last_request = now;
    TtlSelection {
        ttl: if state.long_ttl {
            CacheTtl::OneHour
        } else {
            CacheTtl::FiveMinutes
        },
        switched_to_long,
    }
}

/// Marks the last tool, the last system block and the last cacheable block of the final message.
pub(crate) fn apply_cache_control(body: &mut Value, ttl: CacheTtl) {
    let marker = match ttl {
        CacheTtl::FiveMinutes => json!({"type": "ephemeral"}),
        CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
    };
    let mark = |block: Option<&mut Value>| {
        if let Some(object) = block.and_then(Value::as_object_mut) {
            object.insert("cache_control".to_string(), marker.clone());
        }
    };
    mark(body.get_mut("tools").and_then(last_item));
    mark(body.get_mut("system").and_then(last_item));
    mark(
        body.get_mut("messages")
            .and_then(last_item)
            .and_then(|message| message.get_mut("content"))
            .and_then(last_cacheable_block),
    );
}

fn last_item(value: &mut Value) -> Option<&mut Value> {
    value.as_array_mut().and_then(|items| items.last_mut())
}

fn last_cacheable_block(content: &mut Value) -> Option<&mut Value> {
    content.as_array_mut()?.iter_mut().rev().find(|block| {
        !matches!(
            block.get("type").and_then(Value::as_str),
            Some("thinking" | "redacted_thinking")
        )
    })
}
