//! Session-title generation policy.

use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelPreset;

const MAX_SESSION_NAME_CHARS: usize = 32;
const MIN_SESSION_NAME_WORDS: usize = 2;
const MAX_SESSION_NAME_WORDS: usize = 7;
const MAX_TRANSCRIPT_CHARS: usize = 6_000;
const MAX_TRANSCRIPT_MESSAGES: usize = 48;
const SENSITIVE_SESSION_NAME_FALLBACK: &str = "Sensitive Session";
const OPENAI_SESSION_NAME_MODEL_KEYWORD: &str = "mini";

/// The model selected for a session-title request.
pub struct SessionNameModelSelection<'a> {
    /// Model identifier.
    pub model: &'a str,
    /// Selection rationale.
    pub reason: SessionNameModelSelectionReason,
}

/// Reason the session-title model was selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionNameModelSelectionReason {
    OpenAiMini,
    ConfiguredFastModel,
    DefaultModel,
}

impl SessionNameModelSelectionReason {
    /// Returns the stable telemetry label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiMini => "openai_mini",
            Self::ConfiguredFastModel => "configured_fast_model",
            Self::DefaultModel => "default_model",
        }
    }
}

/// Selects the model used for an automatically generated session title.
pub fn select_session_name_model<'a>(
    provider_id: &str,
    model_fast: Option<&'a str>,
    default_model: &'a str,
    available_models: &'a [ModelPreset],
) -> SessionNameModelSelection<'a> {
    if provider_id.eq_ignore_ascii_case(OPENAI_PROVIDER_ID) {
        let Some(model) =
            first_available_model_matching(available_models, OPENAI_SESSION_NAME_MODEL_KEYWORD)
        else {
            return SessionNameModelSelection {
                model: default_model,
                reason: SessionNameModelSelectionReason::DefaultModel,
            };
        };
        return SessionNameModelSelection {
            model,
            reason: SessionNameModelSelectionReason::OpenAiMini,
        };
    }

    if let Some(model) = model_fast.map(str::trim).filter(|model| !model.is_empty()) {
        return SessionNameModelSelection {
            model,
            reason: SessionNameModelSelectionReason::ConfiguredFastModel,
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

fn contains_ascii_case(value: &str, needle: &str) -> bool {
    value.to_ascii_lowercase().contains(needle)
}

/// Builds the model prompt for automatic session-title generation.
pub fn session_name_prompt(current_name: Option<&str>, transcript: &str) -> String {
    let current_name = current_name
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("Current name: {}\n", name.trim()))
        .unwrap_or_default();
    format!(
        "You are a session title generator. Your only job is to read a conversation transcript and output a short title.\n\
         Output ONLY the title. Do not respond to the transcript. Do not acknowledge it. No explanations.\n\
         Max {MAX_SESSION_NAME_CHARS} characters.\n\
         Use {MIN_SESSION_NAME_WORDS}-{MAX_SESSION_NAME_WORDS} words. A short noun phrase describing the main topic or task.\n\
         No quotes. No markdown. No punctuation at the end.\n\
         Do not include secrets, tokens, keys, passwords, emails, exact URLs, file paths, IDs, or other unique identifiers.\n\
         If the transcript contains sensitive data, use a generic topic name.\n\
         \n\
         Good titles:\n\
         - Debug login redirect\n\
         - Setup CI pipeline\n\
         - Review auth module PR\n\
         \n\
         Bad titles (never output anything like these):\n\
         - Good question. Let me dig into that\n\
         - Sure, I can help with this\n\
         - I'll look at the code\n\
         \n\
         {current_name}Transcript:\n{transcript}"
    )
}

/// Builds the bounded transcript used for session-title generation.
pub fn transcript_excerpt_with_partial_response(
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
            ContentItem::InputAudio { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Appends assistant message text to a generated title buffer.
pub fn append_message_text(output: &mut String, item: &ResponseItem) {
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

/// Sanitizes and bounds a model-generated session title.
pub fn normalize_generated_session_name(name: &str) -> Option<String> {
    let explicit_candidate = explicit_session_name_candidate(name);
    let candidate = explicit_candidate.unwrap_or_else(|| name.trim());
    let name = sanitize_generated_session_name(candidate.trim_matches(&['"', '\'', '`'][..]));
    if explicit_candidate.is_none() && generated_session_name_looks_like_agent_response(&name) {
        return None;
    }
    let words = name.split_whitespace().collect::<Vec<_>>();
    let repeat_start = (1..words.len()).find(|index| {
        let repeated_words = &words[*index..];
        repeated_words.len() >= 2 && words.starts_with(repeated_words)
    });
    let name_without_repeat = repeat_start.map(|index| words[..index].join(" "));
    let name = name_without_repeat.as_deref().unwrap_or(name.as_str());
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
    let name = name_within_limits.trim();
    if name.is_empty() {
        return None;
    }
    if generated_session_name_looks_sensitive(name) {
        Some(SENSITIVE_SESSION_NAME_FALLBACK.to_string())
    } else {
        Some(name.to_string())
    }
}

fn explicit_session_name_candidate(generated: &str) -> Option<&str> {
    const SESSION_NAME_LABEL: &str = "session name:";

    generated.lines().find_map(|line| {
        let line = line
            .trim()
            .trim_start_matches(&['*', '_', '`', '-', ' '][..]);
        if line.to_ascii_lowercase().starts_with(SESSION_NAME_LABEL) {
            let candidate = line[SESSION_NAME_LABEL.len()..]
                .trim()
                .trim_start_matches(&['*', '_', '`', '-', ' ', ':'][..])
                .trim();
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
        None
    })
}

fn generated_session_name_looks_like_agent_response(name: &str) -> bool {
    let lower = name.to_ascii_lowercase().replace('\u{2019}', "'");
    let first_line = lower
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim();
    first_line.starts_with("i'll ")
        || first_line.starts_with("i will ")
        || first_line.starts_with("i can ")
        || first_line.starts_with("i cannot ")
        || first_line.starts_with("i can't ")
        || first_line.starts_with("let me ")
        || lower.contains("name the session")
        || lower.contains("<function_calls")
        || lower.contains("<invoke ")
        || lower.contains("</invoke>")
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
