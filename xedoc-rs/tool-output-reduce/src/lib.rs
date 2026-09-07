//! Content-aware tool-output reduction.
//!
//! Content-aware tool-output reduction and reversible spill storage.

mod dedup;
mod diff;
mod json;
mod log;
mod normalize;
mod spill;
pub use spill::prune_spill_root;

use sha2::Digest;
use sha2::Sha256;
use xedoc_protocol::protocol::TruncationPolicy;
use xedoc_utils_absolute_path::AbsolutePathBuf;

/// Controls which reduction stages are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReductionLevel {
    Off,
    Conservative,
    Balanced,
    Aggressive,
}

/// Configuration for reducing one tool output.
#[derive(Debug, Clone)]
pub struct ReductionConfig {
    pub level: ReductionLevel,
    pub budget: TruncationPolicy,
    pub spill_dir: Option<AbsolutePathBuf>,
}

/// Input metadata and text supplied by a tool-output producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReductionInput<'a> {
    pub tool_name: &'a str,
    pub call_id: &'a str,
    pub text: &'a str,
    /// Privacy-preserving identity of the producing command, when available.
    pub command_hash: Option<&'a str>,
}

/// Broad payload shape used to classify reduction records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    Json,
    Log,
    Diff,
    Table,
    Code,
    Prose,
    Binaryish,
}

/// Identifier for a reducer stage applied to an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReducerId {
    Normalize,
    Dedup,
    Json,
    Log,
    Diff,
    Budget,
}

/// Measurements and metadata produced alongside a reduced output.
#[derive(Debug, Clone, PartialEq)]
pub struct ReductionRecord {
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub call_id: String,
    pub command_hash: Option<String>,
    pub tool_name: String,
    pub kind: PayloadKind,
    pub level: ReductionLevel,
    pub reducers_applied: Vec<ReducerId>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub est_tokens_in: i64,
    pub est_tokens_out: i64,
    /// Model active when the reduction was recorded, when available.
    pub model_slug: Option<String>,
    /// Input price in USD per one million tokens captured at reduction time.
    pub input_price_per_1m: Option<f64>,
    pub duration_us: u64,
    pub spilled: bool,
    pub spill_path: Option<String>,
    pub recorded_at: i64,
}

/// Cumulative reduction savings for the current session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReductionSessionStats {
    pub reductions: i64,
    pub tokens_saved: i64,
    pub cost_saved_usd: f64,
}

/// Non-blocking destination for baseline reduction records.
///
/// Implementations should enqueue records into a bounded buffer and may drop
/// records when the buffer is full; callers run on synchronous ingestion paths.
pub trait ReductionSink: Send + Sync {
    fn try_record(&self, record: ReductionRecord);

    /// Returns additive savings accumulated by this sink for the active session.
    fn session_stats(&self) -> ReductionSessionStats {
        ReductionSessionStats {
            reductions: 0,
            tokens_saved: 0,
            cost_saved_usd: 0.0,
        }
    }

    /// Record a model-initiated read of a spilled output.
    fn try_record_retrieval(&self, call_id: &str, spill_path: &str) {
        let _ = (call_id, spill_path);
    }
}

/// Reduced text and the measurements describing the reduction.
#[derive(Debug, Clone, PartialEq)]
pub struct ReductionOutput {
    pub text: String,
    pub record: ReductionRecord,
}

fn reduction_header(original: &str, reduced: &str, reducers: &[ReducerId], path: &str) -> String {
    let names = reducers
        .iter()
        .map(|reducer| match reducer {
            ReducerId::Normalize => "normalize",
            ReducerId::Dedup => "dedup",
            ReducerId::Json => "json",
            ReducerId::Log => "log",
            ReducerId::Diff => "diff-display-only",
            ReducerId::Budget => "budget",
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Note: output reduced by xedoc ({names}; ~{} → ~{} tokens).\nRecoverable original: {path}",
        xedoc_utils_string::approx_token_count(original),
        xedoc_utils_string::approx_token_count(reduced)
    )
}

/// Reduces a tool output according to `config`.
///
/// Conservative reduction is limited to presentation normalization and exact
/// consecutive-line deduplication. Every changed payload is spilled before a
/// bounded retrieval marker is prepended.
pub fn reduce(input: ReductionInput<'_>, config: &ReductionConfig) -> ReductionOutput {
    let started = std::time::Instant::now();
    let bytes = input.text.len() as u64;
    let tokens = xedoc_utils_string::approx_token_count(input.text) as i64;

    let mut text = input.text.to_owned();
    let mut reducers_applied = Vec::new();
    let mut spill_path = None;

    let kind = detect_payload_kind(input.text);
    let json_candidate = looks_like_json(input.text);
    let structured_tool_output =
        input.tool_name == "tool_search" || input.tool_name.ends_with(".tool_search");
    if !matches!(config.level, ReductionLevel::Off)
        // Structured payloads are owned by their producer; changing their
        // bytes can invalidate their schema.
        && !structured_tool_output
        && !matches!(kind, PayloadKind::Json | PayloadKind::Diff | PayloadKind::Code)
        && !json_candidate
    {
        let normalized = normalize::normalize(input.text);
        if normalized.len() < text.len() {
            text = normalized;
            reducers_applied.push(ReducerId::Normalize);
        }
        let deduplicated = dedup::dedup_exact(&text);
        if deduplicated.len() < text.len() {
            text = deduplicated;
            reducers_applied.push(ReducerId::Dedup);
        }
    }

    if matches!(kind, PayloadKind::Json)
        && matches!(
            config.level,
            ReductionLevel::Balanced | ReductionLevel::Aggressive
        )
        && let Some(crushed) = json::crush(input.text)
    {
        text = crushed;
        reducers_applied.push(ReducerId::Json);
    }

    if matches!(kind, PayloadKind::Json)
        && text.len() > config.budget.byte_budget()
        && let Some(fitted) = json::fit_to_budget(&text, config.budget)
        && fitted.len() < text.len()
    {
        text = fitted;
        reducers_applied.push(ReducerId::Budget);
    }

    if matches!(
        (kind, config.level),
        (
            PayloadKind::Log,
            ReductionLevel::Balanced | ReductionLevel::Aggressive
        )
    ) && let Some(reduced) = log::reduce(&text)
        && reduced.len() < text.len()
    {
        text = reduced;
        reducers_applied.push(ReducerId::Log);
    }

    if matches!(kind, PayloadKind::Diff)
        && matches!(config.level, ReductionLevel::Aggressive)
        && let Some(reduced) = diff::reduce(&text)
        && reduced.len() < text.len()
    {
        text = reduced;
        reducers_applied.push(ReducerId::Diff);
    }

    if input.text.len() > config.budget.byte_budget()
        && !matches!(kind, PayloadKind::Json | PayloadKind::Code)
        && !json_candidate
    {
        let truncated = xedoc_utils_output_truncation::truncate_text(&text, config.budget);
        if truncated.len() < text.len() {
            reducers_applied.push(ReducerId::Budget);
            text = truncated;
        }
    }

    if !reducers_applied.is_empty() && text.len() < input.text.len() {
        let spilled_path = config
            .spill_dir
            .as_ref()
            .and_then(|dir| spill::spill_original(dir, input.call_id, input.text).ok());
        if let Some(path) = spilled_path {
            let marker = reduction_header(
                input.text,
                &text,
                &reducers_applied,
                &path.display().to_string(),
            );
            let mut with_header = format!("{marker}\n\n{text}");
            // Keep JSON parseable while accounting for the reduction header in
            // the configured byte budget.
            if matches!(kind, PayloadKind::Json) && with_header.len() > config.budget.byte_budget()
            {
                let body_budget = config.budget.byte_budget().saturating_sub(marker.len() + 2);
                if let Some(fitted) =
                    json::fit_to_budget(&text, TruncationPolicy::Bytes(body_budget))
                {
                    with_header = format!("{marker}\n\n{fitted}");
                }
            } else if with_header.len() > config.budget.byte_budget() {
                with_header = format!(
                    "{marker}\n\n{}",
                    xedoc_utils_output_truncation::truncate_text(&text, config.budget)
                );
            }
            if with_header.len() < input.text.len()
                && with_header.len() <= config.budget.byte_budget()
            {
                text = with_header;
                spill_path = Some(path.to_string_lossy().into_owned());
            } else {
                let _ = std::fs::remove_file(&path);
                text = input.text.to_owned();
                reducers_applied.clear();
            }
        } else {
            // A reduction without a durable original would violate
            // reversibility, so retain the input and let the existing cap
            // handle oversized output.
            text = input.text.to_owned();
            reducers_applied.clear();
        }
    }
    if reducers_applied.is_empty() && input.text.len() > config.budget.byte_budget() {
        let fitted = if matches!(kind, PayloadKind::Json) || json_candidate {
            json::fit_to_budget(input.text, config.budget)
        } else if !matches!(kind, PayloadKind::Code) {
            Some(xedoc_utils_output_truncation::truncate_text(
                input.text,
                config.budget,
            ))
        } else {
            None
        };
        if let Some(fitted) = fitted
            && fitted.len() < input.text.len()
        {
            text = fitted;
            reducers_applied.push(ReducerId::Budget);
        }
    }
    let bytes_out = text.len() as u64;
    let est_tokens_out = xedoc_utils_string::approx_token_count(&text) as i64;
    ReductionOutput {
        text,
        record: ReductionRecord {
            call_id: input.call_id.to_owned(),
            command_hash: input.command_hash.map(str::to_owned),
            thread_id: None,
            turn_id: None,
            tool_name: input.tool_name.to_owned(),
            kind,
            level: config.level,
            reducers_applied,
            bytes_in: bytes,
            bytes_out,
            est_tokens_in: tokens,
            est_tokens_out,
            model_slug: None,
            input_price_per_1m: None,
            duration_us: started.elapsed().as_micros() as u64,
            spilled: spill_path.is_some(),
            spill_path,
            recorded_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64),
        },
    }
}

/// Return a stable, bounded, non-reversible identity for a command string.
pub fn command_identity_hash(command: &str) -> String {
    let digest = Sha256::digest(command.as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Returns true when `path` is a spill file under `spill_dir`.
pub fn is_spill_path(path: &std::path::Path, spill_dir: &std::path::Path) -> bool {
    let Ok(root) = std::fs::canonicalize(spill_dir) else {
        return false;
    };
    let Ok(candidate) = std::fs::canonicalize(path) else {
        return false;
    };
    candidate.starts_with(root) && candidate.is_file()
}

/// Extract a spill path from a shell command, if it references a known spill root.
pub fn spill_path_in_command(
    command: &str,
    spill_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    command.split_whitespace().find_map(|token| {
        let candidate =
            std::path::Path::new(token.trim_matches(|c: char| c == '"' || c == '\'' || c == '`'));
        is_spill_path(candidate, spill_dir).then(|| candidate.to_path_buf())
    })
}

/// Recover the producing call id from the spill layout `<root>/<call-id>/<hash>.txt`.
pub fn spill_call_id(path: &std::path::Path, spill_dir: &std::path::Path) -> Option<String> {
    if !is_spill_path(path, spill_dir) {
        return None;
    }
    let encoded = path.parent()?.file_name()?.to_str()?;
    spill::decode_call_id(encoded)
}

fn detect_payload_kind(text: &str) -> PayloadKind {
    let trimmed = json_body(text).trim_start();
    if ((trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']')))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return PayloadKind::Json;
    }
    if text
        .lines()
        .any(|line| line.starts_with("diff --git ") || line.starts_with("@@ "))
    {
        return PayloadKind::Diff;
    }
    if trimmed.starts_with("```") || text.lines().any(|line| line.starts_with("#include ")) {
        return PayloadKind::Code;
    }
    let lines = text.lines().collect::<Vec<_>>();
    let log_lines = lines.iter().filter(|line| is_log_line(line)).count();
    if !lines.is_empty() && log_lines * 2 >= lines.len() {
        return PayloadKind::Log;
    }
    PayloadKind::Prose
}

fn is_log_line(line: &str) -> bool {
    let line = line.trim_start();
    let mut rest = line;
    // Reject line-numbered source/search output such as `src/main.rs:42: ...`
    // and `42: ...` before considering a level prefix.
    if rest.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        let Some(separator) = rest.find(' ') else {
            return false;
        };
        let prefix = &rest[..separator];
        if prefix.contains(':') && !looks_like_timestamp(prefix) {
            return false;
        }
        rest = &rest[separator + 1..];
    }
    ["ERROR", "WARN", "INFO", "DEBUG", "TRACE", "FATAL"]
        .iter()
        .any(|level| {
            rest.starts_with(level)
                && rest
                    .as_bytes()
                    .get(level.len())
                    .is_some_and(u8::is_ascii_whitespace)
        })
}

fn looks_like_timestamp(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    let clock = bytes.len() >= 8
        && bytes[2] == b':'
        && bytes[5] == b':'
        && bytes[..8]
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 2 || index == 5 || byte.is_ascii_digit());
    let iso = prefix.contains('T') && prefix.chars().filter(char::is_ascii_digit).count() >= 8;
    clock || iso
}

fn looks_like_json(text: &str) -> bool {
    let trimmed = json_body(text).trim_start();
    (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
}

fn json_body(text: &str) -> &str {
    text.find("\nOutput:\n")
        .map_or(text, |offset| &text[offset + "\nOutput:\n".len()..])
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "json_tests.rs"]
mod json_tests;

#[cfg(test)]
#[path = "log_tests.rs"]
mod log_tests;

#[cfg(test)]
#[path = "diff_tests.rs"]
mod diff_tests;
