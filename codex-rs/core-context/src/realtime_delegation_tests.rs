use super::*;
use crate::ContextualUserFragment;
use codex_utils_string::approx_token_count;

#[test]
fn oversized_realtime_delegation_stays_within_context_budget() {
    let input = "<input>&".repeat(MAX_REALTIME_DELEGATION_TOKENS * 2);
    let transcript = "transcript ".repeat(MAX_REALTIME_DELEGATION_TOKENS * 2);
    let rendered =
        RealtimeDelegation::new(&input, Some(&transcript), RealtimeDelegationSource::Handoff)
            .render();

    assert!(approx_token_count(&rendered) <= MAX_REALTIME_DELEGATION_TOKENS);
}
