use crate::Prompt;
use crate::client_common::ResponseEvent;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::session::session::Session;
use crate::session::turn_context::TurnMultiAgentRuntime;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;

const MAX_SESSION_NAME_CHARS: usize = 32;
const MAX_TRANSCRIPT_CHARS: usize = 6_000;
const MAX_TRANSCRIPT_MESSAGES: usize = 24;
const SENSITIVE_SESSION_NAME_FALLBACK: &str = "Sensitive Session";

impl Session {
    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) async fn generate_session_name(
        &self,
        current_name: Option<&str>,
    ) -> CodexResult<Option<String>> {
        let history = self.clone_history().await;
        let Some(transcript) = transcript_excerpt(history.raw_items()) else {
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
                Ok(ResponseEvent::Completed { .. }) => {
                    return Ok(normalize_generated_session_name(&generated));
                }
                Ok(_) => {}
                Err(err) => return Err(err),
            }
        }
        Err(CodexErr::Stream(
            "stream closed before response.completed".into(),
            None,
        ))
    }
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
         Use 2-5 words. No quotes. No markdown.\n\
         Do not include secrets, tokens, keys, passwords, emails, exact URLs, file paths, IDs, or other unique identifiers.\n\
         If the transcript contains sensitive data, use a generic topic name.\n\
         {current_name}Transcript:\n{transcript}"
    )
}

fn transcript_excerpt(items: &[ResponseItem]) -> Option<String> {
    let mut remaining = MAX_TRANSCRIPT_CHARS;
    let mut messages = Vec::new();
    for item in items.iter().rev() {
        if messages.len() >= MAX_TRANSCRIPT_MESSAGES || remaining == 0 {
            break;
        }
        let ResponseItem::Message { role, content, .. } = item else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = message_text(content);
        if text.trim().is_empty() {
            continue;
        }
        let label = if role == "assistant" {
            "Assistant"
        } else {
            "User"
        };
        let mut entry = format!("{label}: {}", collapse_whitespace(text.trim()));
        if entry.chars().count() > remaining {
            entry = take_last_chars(&entry, remaining);
        }
        remaining = remaining.saturating_sub(entry.chars().count());
        messages.push(entry);
    }
    if messages.is_empty() {
        None
    } else {
        messages.reverse();
        Some(messages.join("\n"))
    }
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
    let name = take_first_chars(&name, MAX_SESSION_NAME_CHARS);
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

fn take_first_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn take_last_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

#[cfg(test)]
#[path = "session_name_tests.rs"]
mod tests;
