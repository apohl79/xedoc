use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::session::session::Session;
use crate::session::turn_context::TurnMultiAgentRuntime;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelPreset;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use std::sync::Arc;
use tracing::debug;

const MAX_SESSION_NAME_CHARS: usize = 32;
const MIN_SESSION_NAME_WORDS: usize = 2;
const MAX_SESSION_NAME_WORDS: usize = 7;
const MAX_TRANSCRIPT_CHARS: usize = 6_000;
const MAX_TRANSCRIPT_MESSAGES: usize = 48;
const SENSITIVE_SESSION_NAME_FALLBACK: &str = "Sensitive Session";
const ANTHROPIC_PROVIDER_ID: &str = "anthropic";
const OPENAI_SESSION_NAME_MODEL_KEYWORD: &str = "mini";
const ANTHROPIC_SESSION_NAME_MODEL_KEYWORD: &str = "haiku";

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
            turn_context.provider.info().name.as_str(),
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
        let mut client_session = self.services.model_client.new_session();
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

struct SessionNameModelSelection<'a> {
    model: &'a str,
    reason: SessionNameModelSelectionReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionNameModelSelectionReason {
    OpenAiMini,
    AnthropicHaiku,
    DefaultModel,
}

impl SessionNameModelSelectionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiMini => "openai_mini",
            Self::AnthropicHaiku => "anthropic_haiku",
            Self::DefaultModel => "default_model",
        }
    }
}

fn select_session_name_model<'a>(
    provider_id: &str,
    provider_name: &str,
    default_model: &'a str,
    available_models: &'a [ModelPreset],
) -> SessionNameModelSelection<'a> {
    if provider_id.eq_ignore_ascii_case(OPENAI_PROVIDER_ID)
        && let Some(model) =
            first_available_model_matching(available_models, OPENAI_SESSION_NAME_MODEL_KEYWORD)
    {
        return SessionNameModelSelection {
            model,
            reason: SessionNameModelSelectionReason::OpenAiMini,
        };
    }

    if is_anthropic_provider(provider_id, provider_name)
        && let Some(model) =
            first_available_model_matching(available_models, ANTHROPIC_SESSION_NAME_MODEL_KEYWORD)
    {
        return SessionNameModelSelection {
            model,
            reason: SessionNameModelSelectionReason::AnthropicHaiku,
        };
    }

    SessionNameModelSelection {
        model: default_model,
        reason: SessionNameModelSelectionReason::DefaultModel,
    }
}

fn first_available_model_matching<'a>(
    available_models: &'a [ModelPreset],
    keyword: &str,
) -> Option<&'a str> {
    available_models
        .iter()
        .filter(|preset| preset.show_in_picker)
        .find(|preset| {
            contains_ascii_case(&preset.model, keyword)
                || contains_ascii_case(&preset.display_name, keyword)
        })
        .map(|preset| preset.model.as_str())
}

fn is_anthropic_provider(provider_id: &str, provider_name: &str) -> bool {
    provider_id.eq_ignore_ascii_case(ANTHROPIC_PROVIDER_ID)
        || contains_ascii_case(provider_name, ANTHROPIC_PROVIDER_ID)
        || contains_ascii_case(provider_name, "claude")
}

fn contains_ascii_case(value: &str, needle: &str) -> bool {
    value.to_ascii_lowercase().contains(needle)
}

fn session_name_prompt(current_name: Option<&str>, transcript: &str) -> String {
    let current_name = current_name
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("Current name: {}\n", name.trim()))
        .unwrap_or_default();
    format!(
        "Name this Codex session.\n\
         Return only one short session name.\n\
         Max {MAX_SESSION_NAME_CHARS} characters.\n\
         Use {MIN_SESSION_NAME_WORDS}-{MAX_SESSION_NAME_WORDS} words. No quotes. No markdown.\n\
         Do not include secrets, tokens, keys, passwords, emails, exact URLs, file paths, IDs, or other unique identifiers.\n\
         If the transcript contains sensitive data, use a generic topic name.\n\
         {current_name}Transcript:\n{transcript}"
    )
}

fn transcript_excerpt_with_partial_response(
    items: &[ResponseItem],
    partial_response: Option<&str>,
) -> Option<String> {
    let mut remaining = MAX_TRANSCRIPT_CHARS;
    let mut messages = Vec::new();
    let partial_entry = partial_response
        .and_then(|text| transcript_entry("assistant", text))
        .map(|entry| take_last_chars(&entry, remaining));
    let history_message_limit = if let Some(partial_entry) = &partial_entry {
        remaining = remaining.saturating_sub(partial_entry.chars().count());
        MAX_TRANSCRIPT_MESSAGES.saturating_sub(1)
    } else {
        MAX_TRANSCRIPT_MESSAGES
    };

    for item in items.iter().rev() {
        if messages.len() >= history_message_limit || remaining == 0 {
            break;
        }
        let ResponseItem::Message { role, content, .. } = item else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = message_text(content);
        let Some(entry) = transcript_entry(role, &text) else {
            continue;
        };
        push_transcript_entry(&mut messages, &mut remaining, entry);
    }
    messages.reverse();
    if let Some(partial_entry) = partial_entry {
        messages.push(partial_entry);
    }
    if messages.is_empty() {
        None
    } else {
        Some(messages.join("\n"))
    }
}

fn transcript_entry(role: &str, text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let label = if role == "assistant" {
        "Assistant"
    } else {
        "User"
    };
    Some(format!("{label}: {}", collapse_whitespace(text.trim())))
}

fn push_transcript_entry(messages: &mut Vec<String>, remaining: &mut usize, mut entry: String) {
    if entry.chars().count() > *remaining {
        entry = take_last_chars(&entry, *remaining);
    }
    *remaining = remaining.saturating_sub(entry.chars().count());
    messages.push(entry);
}

fn message_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_message_text(output: &mut String, item: &ResponseItem) {
    let ResponseItem::Message { role, content, .. } = item else {
        return;
    };
    if role != "assistant" {
        return;
    }
    if !output.is_empty() {
        output.push(' ');
    }
    output.push_str(&message_text(content));
}

fn normalize_generated_session_name(name: &str) -> Option<String> {
    let name = sanitize_generated_session_name(name.trim().trim_matches(&['"', '\'', '`'][..]));
    let mut name_within_limits = String::new();
    for word in name.split_whitespace().take(MAX_SESSION_NAME_WORDS) {
        let separator_chars = usize::from(!name_within_limits.is_empty());
        let next_chars =
            name_within_limits.chars().count() + separator_chars + word.chars().count();
        if next_chars > MAX_SESSION_NAME_CHARS {
            break;
        }
        if !name_within_limits.is_empty() {
            name_within_limits.push(' ');
        }
        name_within_limits.push_str(word);
    }
    let name = name_within_limits;
    let name = crate::util::normalize_thread_name(&name)?;
    if generated_session_name_looks_sensitive(&name) {
        Some(SENSITIVE_SESSION_NAME_FALLBACK.to_string())
    } else {
        Some(name)
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sanitize_generated_session_name(value: &str) -> String {
    let mut sanitized = String::new();
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }
        if is_disallowed_session_name_char(ch) {
            continue;
        }
        if pending_space {
            sanitized.push(' ');
            pending_space = false;
        }
        sanitized.push(ch);
    }
    sanitized
}

fn is_disallowed_session_name_char(ch: char) -> bool {
    if ch.is_control() {
        return true;
    }

    matches!(
        ch,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

fn generated_session_name_looks_sensitive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.contains("://") || looks_like_email(name) {
        return true;
    }
    let secret_markers = [
        "api_key=",
        "apikey=",
        "token=",
        "secret=",
        "password=",
        "passwd=",
        "sk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
    ];
    secret_markers.iter().any(|marker| lower.contains(marker))
        || looks_like_aws_access_key(name)
        || name.split_whitespace().any(is_token_like_segment)
        || name.chars().filter(char::is_ascii_digit).count() >= 12
}

fn looks_like_email(value: &str) -> bool {
    value.split_whitespace().any(|segment| {
        let Some((local, domain)) = segment.split_once('@') else {
            return false;
        };
        !local.is_empty() && domain.contains('.') && domain.chars().any(char::is_alphabetic)
    })
}

fn is_token_like_segment(segment: &str) -> bool {
    let segment = segment.trim_matches(|ch: char| {
        !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_' && ch != '.'
    });
    if segment.len() < 16 {
        return false;
    }
    let alpha_count = segment.chars().filter(char::is_ascii_alphabetic).count();
    let digit_count = segment.chars().filter(char::is_ascii_digit).count();
    let alnum_count = alpha_count + digit_count;
    alnum_count >= 12
        && alpha_count >= 6
        && digit_count >= 4
        && (segment.contains('-') || segment.contains('_') || segment.contains('.'))
}

fn looks_like_aws_access_key(value: &str) -> bool {
    value.split_whitespace().any(|segment| {
        let segment = segment.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
        if segment.len() < 16 {
            return false;
        }
        let upper = segment.to_ascii_uppercase();
        (upper.starts_with("AKIA") || upper.starts_with("ASIA"))
            && segment.chars().all(|ch| ch.is_ascii_alphanumeric())
    })
}

fn take_last_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

#[cfg(test)]
#[path = "session_name_tests.rs"]
mod tests;
