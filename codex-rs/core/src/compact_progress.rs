//! Progress reporting for compaction runs.
//!
//! Compaction can take a long time, especially after a cross-provider model switch that forces a
//! full-history replay. This module owns the user-visible progress vocabulary so every compaction
//! path reports the same way: why compaction started, which stage it is in, and how far along that
//! stage is.
//!
//! Progress is emitted as [`EventMsg::Warning`] messages prefixed with
//! [`COMPACTION_PROGRESS_PREFIX`]. Clients detect that prefix and render the remainder as a
//! transient status instead of appending it to history.

use codex_analytics::CompactionReason;
use codex_protocol::protocol::COMPACTION_PROGRESS_PREFIX;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

/// Why a compaction run started, rendered as a short user-facing cause.
///
/// This mirrors [`CompactionReason`] but stays in the presentation layer so analytics variants can
/// evolve without changing user-visible strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompactionCause {
    /// The user asked for compaction explicitly.
    UserRequested,
    /// The active context window or auto-compaction budget was exhausted.
    ContextLimit,
    /// The session switched to a different model and history must be reduced for it.
    ModelSwitch,
}

impl From<CompactionReason> for CompactionCause {
    fn from(reason: CompactionReason) -> Self {
        match reason {
            CompactionReason::UserRequested => Self::UserRequested,
            CompactionReason::ContextLimit => Self::ContextLimit,
            CompactionReason::ModelDownshift | CompactionReason::CompHashChanged => {
                Self::ModelSwitch
            }
        }
    }
}

impl CompactionCause {
    fn label(self) -> &'static str {
        match self {
            Self::UserRequested => "requested",
            Self::ContextLimit => "context limit",
            Self::ModelSwitch => "model switch",
        }
    }
}

/// The stage a compaction run is currently in.
///
/// Single-pass compaction only reports [`Self::Summarizing`] and [`Self::Complete`]. Hierarchical
/// compaction additionally reports map and reduce progress so long runs stay legible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompactionStage {
    /// Preparing the run; `chunks` is how many history chunks will be summarized.
    Planning { chunks: usize },
    /// Summarizing history chunks; `completed` of `total` chunks are done.
    Mapping { completed: usize, total: usize },
    /// Merging summaries; `layer` is the reduction depth over `groups` groups.
    Reducing { layer: usize, groups: usize },
    /// Summarizing history in a single request.
    Summarizing,
    /// The run finished successfully.
    Complete,
    /// The run failed.
    Failed,
}

impl CompactionStage {
    fn details(&self) -> String {
        match self {
            Self::Planning { chunks } => format!("planning {chunks} history chunks"),
            Self::Mapping { completed, total } => format!("summarizing {completed}/{total}"),
            Self::Reducing { layer, groups } => format!("merging layer {layer} ({groups} groups)"),
            Self::Summarizing => String::from("summarizing history"),
            Self::Complete => String::from("complete"),
            Self::Failed => String::from("failed"),
        }
    }
}

/// Renders one progress line, including the cause so the user knows why compaction is running.
///
/// Example: `• Compacting... (model switch) summarizing 2/5`.
pub(crate) fn progress_message(cause: CompactionCause, stage: &CompactionStage) -> String {
    format!(
        "{COMPACTION_PROGRESS_PREFIX} ({}) {}",
        cause.label(),
        stage.details()
    )
}

/// Emits one compaction progress update to the client.
pub(crate) async fn send_progress(
    sess: &Session,
    turn_context: &TurnContext,
    cause: CompactionCause,
    stage: CompactionStage,
) {
    sess.send_event(
        turn_context,
        EventMsg::Warning(WarningEvent {
            message: progress_message(cause, &stage),
        }),
    )
    .await;
}

#[cfg(test)]
#[path = "compact_progress_tests.rs"]
mod tests;
