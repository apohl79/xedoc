//! Per-thread routing, replay, and pending-request state for the TUI application.

#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]

mod app_server_event_targets;
mod app_server_requests;
mod loaded_threads;
mod pending_interactive_replay;
pub mod replay_filter;
mod thread_events;

use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use codex_tui_chatwidget::ThreadInputState;
use codex_tui_events::AppCommand;
use codex_tui_events::HistoryLookupResponse;
use codex_tui_transcript::session_state::ThreadSessionState;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

pub use app_server_event_targets::*;
pub use app_server_requests::*;
pub use loaded_threads::*;
pub use pending_interactive_replay::PendingInteractiveReplayState;
pub use thread_events::*;

/// The unresolved interaction kind retained in a thread's replay buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingThreadInteraction {
    UserInput,
    Approval,
}
