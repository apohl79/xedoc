//! Guardian review decides whether an `on-request` approval should be granted
//! automatically instead of shown to the user.
//!
//! High-level approach:
//! 1. Reconstruct a compact transcript that preserves user intent plus the most
//!    relevant recent assistant and tool context.
//! 2. Ask a dedicated guardian review session to assess the exact planned
//!    action and return strict JSON.
//!    The guardian clones the parent config, so it inherits any managed
//!    network proxy / allowlist that the parent turn already had.
//! 3. Fail closed on timeout, execution failure, or malformed output.
//! 4. Apply the guardian's explicit allow/deny outcome.

mod approval_request;
mod prompt;
mod review;
mod review_session;

use std::time::Duration;

pub(crate) use approval_request::GuardianApprovalRequest;
pub(crate) use approval_request::GuardianMcpAnnotations;
pub(crate) use approval_request::GuardianNetworkAccessTrigger;
#[cfg(test)]
pub(crate) use approval_request::guardian_approval_request_to_json;
pub(crate) use codex_core_guardian_approval::AUTO_REVIEW_DENIAL_WINDOW_SIZE;
pub(crate) use codex_core_guardian_approval::GuardianAssessment;
pub(crate) use codex_core_guardian_approval::GuardianRejectionCircuitBreaker;
pub(crate) use codex_core_guardian_approval::GuardianRejectionCircuitBreakerAction;
pub(crate) use codex_protocol::protocol::GuardianAssessmentOutcome;
pub(crate) use review::guardian_timeout_message;
pub(crate) use review::is_guardian_reviewer_source;
pub(crate) use review::new_guardian_review_id;
#[cfg(test)]
pub(crate) use review::record_guardian_denial_for_test;
pub(crate) use review::review_approval_request;
#[cfg(test)]
pub(crate) use review::review_approval_request_with_cancel;
pub(crate) use review::routes_approval_to_guardian;
pub(crate) use review::routes_approval_to_guardian_with_reviewer;
pub(crate) use review::spawn_approval_request_review;
pub(crate) use review_session::GuardianReviewSessionManager;
pub(crate) use review_session::prompt_cache_key_override_for_review_session;

pub(crate) const GUARDIAN_REVIEW_TIMEOUT: Duration = Duration::from_secs(90);
pub(crate) const GUARDIAN_REVIEWER_NAME: &str = "guardian";
pub(crate) const AUTO_REVIEW_DENIED_ACTION_APPROVAL_DEVELOPER_PREFIX: &str =
    "The user has manually approved a specific action that was previously `Rejected`.";

#[cfg(test)]
use approval_request::format_guardian_action_pretty;
#[cfg(test)]
use approval_request::guardian_assessment_action;
#[cfg(test)]
use approval_request::guardian_request_turn_id;
#[cfg(test)]
use codex_core_guardian_approval::guardian_truncate_text;
#[cfg(test)]
use prompt::GuardianPromptMode;
#[cfg(test)]
use prompt::GuardianTranscriptCursor;
#[cfg(test)]
use prompt::GuardianTranscriptEntry;
#[cfg(test)]
use prompt::GuardianTranscriptEntryKind;
#[cfg(test)]
use prompt::build_guardian_prompt_items;
#[cfg(test)]
use prompt::build_guardian_prompt_items_with_parent_turn;
#[cfg(test)]
use prompt::collect_guardian_transcript_entries;
#[cfg(test)]
use prompt::guardian_output_schema;
#[cfg(test)]
use prompt::parse_guardian_assessment;
#[cfg(test)]
use prompt::render_guardian_transcript_entries;
#[cfg(test)]
use review::GuardianReviewOutcome;
#[cfg(test)]
use review::run_guardian_review_session_with_retry as run_guardian_review_session_for_test;
#[cfg(test)]
use review_session::build_guardian_review_session_config as build_guardian_review_session_config_for_test;

#[cfg(test)]
mod tests;
