//! Status output formatting and display adapters for the TUI.
//!
//! This module turns protocol-level snapshots into stable display structures used by `/status`
//! output and footer/status-line helpers, while keeping rendering concerns out of transport-facing
//! code.
//!
//! `rate_limits` is the main integration point for status-line usage-limit items: it converts raw
//! window snapshots into local-time labels and classifies data as available, stale, or missing.
mod account;
mod card;
mod format;
mod helpers;
pub mod rate_limits;
pub mod remote_connection;

pub use crate::text_formatting::format_tokens_compact;
pub use account::StatusAccountDisplay;
pub use card::StatusHistoryHandle;
#[cfg(any(test, feature = "test-support"))]
pub use card::new_status_output;
#[cfg(any(test, feature = "test-support"))]
pub use card::new_status_output_with_rate_limits;
pub use card::new_status_output_with_rate_limits_handle;
pub use helpers::compose_agents_summary;
pub use helpers::format_directory_display;
pub use helpers::plan_type_display_name;
pub use rate_limits::RateLimitSnapshotDisplay;
pub use rate_limits::RateLimitWindowDisplay;
#[cfg(any(test, feature = "test-support"))]
pub use rate_limits::rate_limit_snapshot_display;
pub use rate_limits::rate_limit_snapshot_display_for_limit;
