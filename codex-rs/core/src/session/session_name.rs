use std::sync::Arc;

use codex_core_session_name::append_message_text;
use codex_core_session_name::normalize_generated_session_name;
use codex_core_session_name::select_session_name_model;
use codex_core_session_name::session_name_prompt;
use codex_core_session_name::transcript_excerpt_with_partial_response;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use tracing::debug;

use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::session::session::Session;
use crate::session::turn_context::TurnMultiAgentRuntime;

impl Session {
    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) async fn generate_session_name(
        &self,
        current_name: Option<&str>,
    ) -> CodexResult<Option<String>> {
        self.generate_session_name_with_partial_response(current_name, None)
            .await
    }

    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) async fn generate_session_name_with_partial_response(
        &self,
        current_name: Option<&str>,
        partial_response: Option<&str>,
    ) -> CodexResult<Option<String>> {
        let history = self.clone_history().await;
        let Some(transcript) =
            transcript_excerpt_with_partial_response(history.raw_items(), partial_response)
        else {
            debug!(
                partial_response_present = partial_response.is_some(),
                "skipping generated session name: no transcript text available"
            );
            return Ok(None);
        };
        let session_configuration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        let turn_context = self
            .new_turn_context_from_configuration(
                "session-name".to_string(),
                session_configuration,
                /*final_output_json_schema*/ None,
                TurnMultiAgentRuntime::Preview,
            )
            .await;
        let default_model = turn_context.model_info.slug.clone();
        let model_selection = select_session_name_model(
            turn_context.config.model_provider_id.as_str(),
            turn_context.config.model_fast.as_deref(),
            default_model.as_str(),
            &turn_context.available_models,
        );
        let selected_model = model_selection.model.to_string();
        let selection_reason = model_selection.reason.as_str();
        let turn_context = if selected_model != default_model {
            Arc::new(
                turn_context
                    .with_model(selected_model, &self.services.models_manager)
                    .await,
            )
        } else {
            turn_context
        };
        let provider_name = turn_context.provider.info().name.clone();
        let model = turn_context.model_info.slug.clone();
        debug!(
            provider = %provider_name,
            default_model = %default_model,
            model = %model,
            selection_reason = selection_reason,
            partial_response_present = partial_response.is_some(),
            "starting generated session name request"
        );
        let prompt = Prompt {
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: session_name_prompt(current_name, &transcript),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            }],
            base_instructions: BaseInstructions::default(),
            ..Default::default()
        };
        let window_id = self.current_window_id().await;
        let responses_metadata = turn_context.turn_metadata_state.to_responses_metadata(
            self.installation_id.clone(),
            window_id,
            CodexResponsesRequestKind::SessionName,
        );
        let mut client_session = self.services.model_client.load().new_session();
        let mut stream = client_session
            .stream(
                &prompt,
                &turn_context.model_info,
                &turn_context.session_telemetry,
                turn_context.reasoning_effort.clone(),
                turn_context.reasoning_summary,
                turn_context.config.service_tier.clone(),
                &responses_metadata,
                &InferenceTraceContext::disabled(),
            )
            .await?;
        let mut generated = String::new();
        while let Some(event) = stream.next().await {
            match event {
                Ok(ResponseEvent::OutputItemDone(item)) => {
                    append_message_text(&mut generated, &item);
                }
                Ok(ResponseEvent::OutputTextDelta(delta)) => {
                    generated.push_str(&delta);
                }
                Ok(ResponseEvent::Completed { .. }) => {
                    let normalized = normalize_generated_session_name(&generated);
                    debug!(
                        provider = %provider_name,
                        model = %model,
                        generated_chars = generated.chars().count(),
                        normalized_chars = normalized.as_ref().map_or(0, |name| name.chars().count()),
                        generated_name_accepted = normalized.is_some(),
                        "completed generated session name request"
                    );
                    return Ok(normalized);
                }
                Ok(_) => {}
                Err(err) => return Err(err),
            }
        }
        debug!(
            provider = %provider_name,
            model = %model,
            generated_chars = generated.chars().count(),
            "generated session name stream closed before completion"
        );
        Err(CodexErr::Stream(
            "stream closed before response.completed".into(),
            None,
        ))
    }
}
