//! Progress reporting for compaction runs.
//!
//! Compaction can take a long time, especially after a cross-provider model switch that forces a
//! full-history replay. This module owns the user-visible progress vocabulary so every compaction
//! path reports the same way: which stage it is in and how far along that stage is.
//!
//! Progress is emitted as [`EventMsg::Warning`] messages prefixed with
//! [`COMPACTION_PROGRESS_PREFIX`]. Clients detect that prefix and render the remainder as a
//! transient status instead of appending it to history.

use codex_protocol::protocol::COMPACTION_PROGRESS_PREFIX;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

/// The stage a compaction run is currently in.
///
/// Single-pass compaction reports preparation, summary generation, and history installation.
/// Hierarchical compaction additionally reports map and reduce progress so long runs stay legible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CompactionStage {
    /// Preparing the compaction request before history analysis or model work begins.
    Preparing,
    /// Preparing the run; `chunks` is how many history chunks will be summarized.
    Planning { chunks: usize },
    /// Summarizing history chunks; `completed` of `total` chunks are done.
    Mapping { completed: usize, total: usize },
    /// Merging summaries; `layer` is the reduction depth over `groups` groups.
    Reducing { layer: usize, groups: usize },
    /// Summarizing history in a single request.
    Summarizing,
    /// The model has started streaming the compacted summary.
    WritingSummary,
    /// Replacing local history with the compacted summary.
    InstallingSummary,
    /// The run finished successfully.
    Complete,
    /// The run failed.
    Failed,
}

impl CompactionStage {
    fn details(&self) -> String {
        match self {
            Self::Preparing => String::from("preparing compaction"),
            Self::Planning { chunks } => format!("planning {chunks} history chunks"),
            Self::Mapping { completed, total } => format!("summarizing {completed}/{total}"),
            Self::Reducing { layer, groups } => format!("merging layer {layer} ({groups} groups)"),
            Self::Summarizing => String::from("summarizing history"),
            Self::WritingSummary => String::from("writing summary"),
            Self::InstallingSummary => String::from("installing summary"),
            Self::Complete => String::from("complete"),
            Self::Failed => String::from("failed"),
        }
    }
}

/// Renders one progress line with the current compaction stage.
///
/// Example: `• Compacting summarizing 2/5`.
pub(crate) fn progress_message(stage: &CompactionStage) -> String {
    format!("{COMPACTION_PROGRESS_PREFIX} {}", stage.details())
}

/// Emits one compaction progress update to the client.
pub(crate) async fn send_progress(
    sess: &Session,
    turn_context: &TurnContext,
    stage: CompactionStage,
) {
    sess.send_event(
        turn_context,
        EventMsg::Warning(WarningEvent {
            message: progress_message(&stage),
        }),
    )
    .await;
}

#[cfg(test)]
#[path = "compact_progress_tests.rs"]
mod tests;
