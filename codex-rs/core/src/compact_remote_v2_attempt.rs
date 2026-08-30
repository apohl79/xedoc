use std::sync::Arc;

use super::RemoteCompactionV2Output;
use super::run_remote_compaction_request_v2;
use crate::Prompt;
use crate::client::ModelClientSession;
use crate::compact::RemoteCompactionHistoryEncryption;
use crate::compact_remote::trim_function_call_history_to_fit_context_window;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::responses_metadata::CompactionTurnMetadata;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn::built_tools;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RawResponseCompletedEvent;
use codex_protocol::protocol::TokenUsage;
use tokio_util::sync::CancellationToken;
use tracing::info;

pub(super) struct RemoteCompactV2Attempt {
    pub(super) prompt_input: Vec<ResponseItem>,
    pub(super) compaction_output: ResponseItem,
    pub(super) token_usage: Option<TokenUsage>,
    /// Keeps a session created for standalone compaction alive through lifecycle completion.
    pub(super) owned_client_session: Option<ModelClientSession>,
}

pub(super) async fn run_remote_compact_v2_attempt(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    client_session: Option<&mut ModelClientSession>,
    history_encryption: RemoteCompactionHistoryEncryption,
    compaction_metadata: CompactionTurnMetadata,
) -> CodexResult<RemoteCompactV2Attempt> {
    let turn_context = &step_context.turn;
    let mut history = sess.clone_history().await;
    let base_instructions = sess.get_base_instructions().await;
    let rewritten_outputs = trim_function_call_history_to_fit_context_window(
        &mut history,
        turn_context.as_ref(),
        &base_instructions,
    );
    if rewritten_outputs > 0 {
        info!(
            turn_id = %turn_context.sub_id,
            rewritten_outputs,
            "rewrote history outputs before remote compaction v2"
        );
    }

    let mut input = match history_encryption {
        RemoteCompactionHistoryEncryption::Preserve => {
            history.for_prompt(&turn_context.model_info.input_modalities)
        }
        RemoteCompactionHistoryEncryption::Strip => {
            history.for_prompt_without_encrypted_content(&turn_context.model_info.input_modalities)
        }
    };
    let tool_router = built_tools(
        sess.as_ref(),
        step_context.as_ref(),
        &CancellationToken::new(),
    )
    .await?;
    input.push(ResponseItem::CompactionTrigger {});
    let prompt = Prompt {
        input,
        tools: tool_router.model_visible_specs(),
        parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
        base_instructions,
        output_schema: None,
        output_schema_strict: true,
    };

    let window_id = sess.current_window_id().await;
    let responses_metadata = turn_context.turn_metadata_state.to_responses_metadata(
        sess.installation_id.clone(),
        window_id,
        CodexResponsesRequestKind::Compaction(compaction_metadata),
    );
    let mut owned_client_session = None;
    let client_session = match client_session {
        Some(client_session) => client_session,
        None => owned_client_session.insert(sess.services.model_client.load().new_session()),
    };
    let compaction_output_result = run_remote_compaction_request_v2(
        sess,
        turn_context.as_ref(),
        client_session,
        &prompt,
        &responses_metadata,
    )
    .await?;
    let RemoteCompactionV2Output {
        compaction_output,
        response_id,
        token_usage,
    } = compaction_output_result;
    // TODO: Emit this before compaction output validation so malformed completed
    // responses still surface their raw upstream usage.
    sess.send_event(
        turn_context,
        EventMsg::RawResponseCompleted(RawResponseCompletedEvent {
            response_id,
            token_usage: token_usage.clone(),
        }),
    )
    .await;
    let mut prompt_input = prompt.input;
    prompt_input.pop();
    Ok(RemoteCompactV2Attempt {
        prompt_input,
        compaction_output,
        token_usage,
        owned_client_session,
    })
}
