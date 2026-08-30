//! Per-thread routing, replay, and pending-request state for the TUI application.

#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]

mod app_server_event_targets;
mod app_server_requests;
mod loaded_threads;
mod pending_interactive_replay;
pub mod replay_filter;
mod thread_events;

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use xedoc_app_server_protocol::ServerNotification;
use xedoc_app_server_protocol::ServerRequest;
use xedoc_app_server_protocol::ThreadItem;
use xedoc_app_server_protocol::Turn;
use xedoc_app_server_protocol::TurnStatus;
use xedoc_protocol::ThreadId;
use xedoc_tui_chatwidget::ThreadInputState;
use xedoc_tui_events::AppCommand;
use xedoc_tui_events::HistoryLookupResponse;
use xedoc_tui_transcript::session_state::ThreadSessionState;

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
