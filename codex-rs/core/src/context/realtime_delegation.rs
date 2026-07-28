use super::ContextualUserFragment;
use codex_utils_string::approx_token_count;
use codex_utils_string::truncate_middle_with_token_budget;

pub(crate) const MAX_REALTIME_DELEGATION_TOKENS: usize = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealtimeDelegationSource {
    Handoff,
    TranscriptTailFlush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RealtimeDelegation<'a> {
    input: &'a str,
    transcript_delta: Option<&'a str>,
    source: RealtimeDelegationSource,
}

impl<'a> RealtimeDelegation<'a> {
    pub(crate) fn new(
        input: &'a str,
        transcript_delta: Option<&'a str>,
        source: RealtimeDelegationSource,
    ) -> Self {
        Self {
            input,
            transcript_delta,
            source,
        }
    }
}

impl ContextualUserFragment for RealtimeDelegation<'_> {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<realtime_delegation>", "</realtime_delegation>")
    }

    fn body(&self) -> String {
        let mut input_budget = MAX_REALTIME_DELEGATION_TOKENS / 2;
        let mut transcript_budget = MAX_REALTIME_DELEGATION_TOKENS / 2;
        loop {
            let input = truncate_to_token_budget(self.input, input_budget);
            let transcript_delta = self
                .transcript_delta
                .filter(|text| !text.is_empty())
                .map(|text| truncate_to_token_budget(text, transcript_budget));
            let rendered = render_body(
                &escape_xml_text(&input),
                transcript_delta.as_deref().map(escape_xml_text),
                self.source,
            );
            let rendered_tokens = approx_token_count(&rendered);
            if rendered_tokens <= MAX_REALTIME_DELEGATION_TOKENS {
                return rendered;
            }

            let excess = rendered_tokens - MAX_REALTIME_DELEGATION_TOKENS;
            if transcript_budget >= input_budget {
                transcript_budget = transcript_budget.saturating_sub(excess.max(1));
            } else {
                input_budget = input_budget.saturating_sub(excess.max(1));
            }
            if input_budget == 0 && transcript_budget == 0 {
                return render_body("", None, self.source);
            }
        }
    }
}

fn render_body(
    input: &str,
    transcript_delta: Option<String>,
    source_kind: RealtimeDelegationSource,
) -> String {
    let source = match source_kind {
        RealtimeDelegationSource::Handoff => "",
        RealtimeDelegationSource::TranscriptTailFlush => {
            "  <source>transcript_tail_flush</source>\n"
        }
    };
    if let Some(transcript_delta) = transcript_delta.filter(|text| !text.is_empty()) {
        return format!(
            "\n{source}  <input>{input}</input>\n  <transcript_delta>{transcript_delta}</transcript_delta>\n"
        );
    }

    format!("\n{source}  <input>{input}</input>\n")
}

fn truncate_to_token_budget(text: &str, max_tokens: usize) -> String {
    let mut budget = max_tokens;
    loop {
        let (candidate, _) = truncate_middle_with_token_budget(text, budget);
        let candidate_tokens = approx_token_count(&candidate);
        if candidate_tokens <= max_tokens {
            return candidate;
        }
        if budget == 0 {
            return String::new();
        }
        budget = budget.saturating_sub((candidate_tokens - max_tokens).max(1));
    }
}

fn escape_xml_text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
#[path = "realtime_delegation_tests.rs"]
mod tests;
