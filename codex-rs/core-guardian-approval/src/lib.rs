//! Guardian approval requests and their bounded model-review serialization.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod approval;
mod metrics;
mod prompt;

pub use approval::AUTO_REVIEW_DENIAL_WINDOW_SIZE;
pub use approval::FormattedGuardianAction;
pub use approval::GuardianApprovalRequest;
pub use approval::GuardianAssessment;
pub use approval::GuardianMcpAnnotations;
pub use approval::GuardianNetworkAccessTrigger;
pub use approval::GuardianRejectionCircuitBreaker;
pub use approval::GuardianRejectionCircuitBreakerAction;
pub use approval::MAX_CONSECUTIVE_GUARDIAN_DENIALS_PER_TURN;
pub use approval::MAX_RECENT_AUTO_REVIEW_DENIALS_PER_TURN;
pub use approval::format_guardian_action_pretty;
pub use approval::guardian_approval_request_to_json;
pub use approval::guardian_assessment_action;
pub use approval::guardian_request_target_item_id;
pub use approval::guardian_request_turn_id;
pub use approval::guardian_reviewed_action;
pub use approval::guardian_truncate_text;
pub use metrics::emit_guardian_review_metrics;
pub use prompt::BUNDLED_GUARDIAN_POLICY;
pub use prompt::BUNDLED_GUARDIAN_POLICY_TEMPLATE;
pub use prompt::GuardianPromptItems;
pub use prompt::GuardianPromptMode;
pub use prompt::GuardianTranscriptCursor;
pub use prompt::GuardianTranscriptEntry;
pub use prompt::GuardianTranscriptEntryKind;
pub use prompt::build_guardian_prompt_items;
pub use prompt::collect_guardian_transcript_entries;
pub use prompt::guardian_output_schema;
pub use prompt::guardian_policy_prompt_with_config_and_template;
pub use prompt::parse_guardian_assessment;
pub use prompt::render_guardian_transcript_entries;
