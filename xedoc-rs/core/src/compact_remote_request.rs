use std::sync::Arc;
use std::sync::OnceLock;

use super::trim_function_call_history_to_fit_context_window;
use crate::Prompt;
use crate::client::CompactConversationRequestSettings;
use crate::compact::RemoteCompactionHistoryEncryption;
use crate::compact::ensure_fixed_instructions_fit;
use crate::responses_metadata::CompactionTurnMetadata;
use crate::responses_metadata::XedocResponsesRequestKind;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn::built_tools;
use tokio_util::sync::CancellationToken;
use tracing::info;
use xedoc_protocol::auth::AuthMode;
use xedoc_protocol::error::Result as XedocResult;
use xedoc_protocol::models::ResponseItem;

pub(super) async fn run_remote_compact_attempt(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    turn_state: Option<Arc<OnceLock<String>>>,
    history_encryption: RemoteCompactionHistoryEncryption,
    compaction_metadata: CompactionTurnMetadata,
) -> XedocResult<Vec<ResponseItem>> {
    let turn_context = &step_context.turn;
    let base_instructions = sess.get_base_instructions().await;
    ensure_fixed_instructions_fit(turn_context.as_ref(), &base_instructions)?;
    let mut history = sess.clone_history().await;
    let rewritten_outputs = trim_function_call_history_to_fit_context_window(
        &mut history,
        turn_context.as_ref(),
        &base_instructions,
    );
    if rewritten_outputs > 0 {
        info!(
            turn_id = %turn_context.sub_id,
            rewritten_outputs,
            "rewrote history outputs before remote compaction"
        );
    }
    let prompt_input = match history_encryption {
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
    let prompt = Prompt {
        input: prompt_input,
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
        XedocResponsesRequestKind::Compaction(compaction_metadata),
    );
    sess.services
        .model_client
        .load()
        .compact_conversation_history(
            &prompt,
            &turn_context.model_info,
            turn_state,
            CompactConversationRequestSettings {
                effort: turn_context.reasoning_effort.clone(),
                summary: turn_context.reasoning_summary,
                service_tier: if sess.services.auth_manager.auth_mode() == Some(AuthMode::ApiKey) {
                    None
                } else {
                    turn_context.config.service_tier.clone()
                },
            },
            &turn_context.session_telemetry,
            &responses_metadata,
        )
        .await
}
