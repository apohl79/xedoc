use codex_core_guardian_approval::build_guardian_prompt_items as build_prompt_from_history;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

pub(crate) use codex_core_guardian_approval::BUNDLED_GUARDIAN_POLICY;
pub(crate) use codex_core_guardian_approval::BUNDLED_GUARDIAN_POLICY_TEMPLATE;
pub(crate) use codex_core_guardian_approval::GuardianPromptItems;
pub(crate) use codex_core_guardian_approval::GuardianPromptMode;
pub(crate) use codex_core_guardian_approval::GuardianTranscriptCursor;
#[cfg(test)]
pub(crate) use codex_core_guardian_approval::GuardianTranscriptEntry;
#[cfg(test)]
pub(crate) use codex_core_guardian_approval::GuardianTranscriptEntryKind;
#[cfg(test)]
pub(crate) use codex_core_guardian_approval::collect_guardian_transcript_entries;
pub(crate) use codex_core_guardian_approval::guardian_output_schema;
pub(crate) use codex_core_guardian_approval::guardian_policy_prompt_with_config_and_template;
pub(crate) use codex_core_guardian_approval::parse_guardian_assessment;
#[cfg(test)]
pub(crate) use codex_core_guardian_approval::render_guardian_transcript_entries;

use super::GuardianApprovalRequest;

#[cfg(test)]
pub(crate) async fn build_guardian_prompt_items(
    session: &Session,
    retry_reason: Option<String>,
    request: GuardianApprovalRequest,
    mode: GuardianPromptMode,
) -> serde_json::Result<GuardianPromptItems> {
    build_guardian_prompt_items_with_parent_turn(
        session,
        /*parent_turn*/ None,
        retry_reason,
        request,
        mode,
    )
    .await
}

pub(crate) async fn build_guardian_prompt_items_with_parent_turn(
    session: &Session,
    parent_turn: Option<&TurnContext>,
    retry_reason: Option<String>,
    request: GuardianApprovalRequest,
    mode: GuardianPromptMode,
) -> serde_json::Result<GuardianPromptItems> {
    let history = session.clone_history().await;
    let thread_id = session.thread_id.to_string();
    build_prompt_from_history(
        &thread_id,
        history.raw_items(),
        history.history_version(),
        parent_turn.and_then(parent_turn_denied_reads_context),
        retry_reason,
        request,
        mode,
    )
}

fn parent_turn_denied_reads_context(turn: &TurnContext) -> Option<String> {
    #[allow(deprecated)]
    let cwd = &turn.cwd;
    let file_system_policy = turn.permission_profile.file_system_sandbox_policy();
    let mut entries = file_system_policy
        .get_unreadable_roots_with_cwd(cwd)
        .into_iter()
        .map(|root| format!("- path `{}`", root.to_string_lossy()))
        .collect::<Vec<_>>();
    entries.extend(
        file_system_policy
            .get_unreadable_globs_with_cwd(cwd)
            .into_iter()
            .map(|glob| format!("- glob `{glob}`")),
    );
    if entries.is_empty() {
        return None;
    }

    Some(format!(
        "The parent turn's active permission profile denies reading these paths/globs. These are policy restrictions; do not approve escalation whose purpose is to read them.\n{}\n",
        entries.join("\n")
    ))
}
