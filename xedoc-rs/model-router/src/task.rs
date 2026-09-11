//! Bounded, privacy-preserving task input preparation.

use sha2::Digest;
use sha2::Sha256;

/// Maximum prompt bytes retained by the classifier.
pub const MAX_PROMPT_BYTES: usize = 16_384;

/// A task's scope in the orchestration graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteScope {
    Root,
    Subagent,
}

/// Bounded metadata recorded for a prompt without retaining its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMetadata {
    pub sha256: String,
    pub original_bytes: usize,
    pub normalized_bytes: usize,
    pub truncated: bool,
}

/// Input passed to the router. The prompt is borrowed and never stored.
#[derive(Debug, Clone, Copy)]
pub struct TaskEnvelope<'a> {
    pub scope: RouteScope,
    pub prompt: &'a str,
    /// Route in effect before automatic routing. This is retained for shadow
    /// telemetry and as the fallback when a proposal cannot be applied.
    pub current_route: Option<&'a crate::ModelRoute>,
}

/// Normalize insignificant whitespace and retain only a bounded prompt view.
pub fn normalize_task(prompt: &str) -> (String, PromptMetadata) {
    let original_bytes = prompt.len();
    let normalized_bytes = normalized_len(prompt);
    let (bounded, truncated) = if normalized_bytes <= MAX_PROMPT_BYTES {
        (normalize_all(prompt, normalized_bytes), false)
    } else {
        (normalize_bounded(prompt), true)
    };
    let sha256 = format!("{:x}", Sha256::digest(bounded.as_bytes()));
    let metadata = PromptMetadata {
        sha256,
        original_bytes,
        normalized_bytes: bounded.len(),
        truncated,
    };
    (bounded, metadata)
}

fn normalized_len(prompt: &str) -> usize {
    prompt.split_whitespace().fold(0, |normalized_bytes, word| {
        normalized_bytes + word.len() + usize::from(normalized_bytes != 0)
    })
}

fn normalize_all(prompt: &str, normalized_bytes: usize) -> String {
    let mut normalized = String::with_capacity(normalized_bytes);
    for word in prompt.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

fn normalize_bounded(prompt: &str) -> String {
    const SEPARATOR: &str = " … ";
    let content_bytes = MAX_PROMPT_BYTES - SEPARATOR.len();
    let head_bytes = content_bytes / 2;
    let tail_bytes = content_bytes - head_bytes;
    let head = normalized_prefix(prompt, head_bytes);
    let tail = normalized_suffix(prompt, tail_bytes);
    format!("{head}{SEPARATOR}{tail}")
}

fn normalized_prefix(prompt: &str, max_bytes: usize) -> String {
    let mut prefix = String::with_capacity(max_bytes);
    for word in prompt.split_whitespace() {
        if !prefix.is_empty() && !append_prefix(&mut prefix, " ", max_bytes) {
            break;
        }
        if !append_prefix(&mut prefix, word, max_bytes) {
            break;
        }
    }
    prefix
}

fn append_prefix(output: &mut String, value: &str, max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(output.len());
    let prefix = value
        .char_indices()
        .take_while(|(index, character)| *index + character.len_utf8() <= remaining)
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .map_or("", |end| &value[..end]);
    output.push_str(prefix);
    prefix.len() == value.len()
}

fn normalized_suffix(prompt: &str, max_bytes: usize) -> String {
    let mut remaining = max_bytes;
    let mut reversed_parts = Vec::new();
    for (index, word) in prompt.split_whitespace().rev().enumerate() {
        if index != 0 && !append_suffix(&mut reversed_parts, " ", &mut remaining) {
            break;
        }
        if !append_suffix(&mut reversed_parts, word, &mut remaining) {
            break;
        }
    }
    reversed_parts.into_iter().rev().collect()
}

fn append_suffix<'a>(parts: &mut Vec<&'a str>, value: &'a str, remaining: &mut usize) -> bool {
    let suffix = value
        .char_indices()
        .find(|(index, _)| value.len() - index <= *remaining)
        .map_or("", |(start, _)| &value[start..]);
    *remaining -= suffix.len();
    if suffix.is_empty() {
        return false;
    }
    parts.push(suffix);
    suffix.len() == value.len() && *remaining != 0
}
