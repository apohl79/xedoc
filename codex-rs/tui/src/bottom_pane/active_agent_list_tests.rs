use std::time::Duration;
use std::time::Instant;

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn display_lines_returns_empty_for_empty_agents() {
    let list = ActiveAgentList::new(FrameRequester::test_dummy());

    let lines = list.display_lines_at(/*width*/ 80, Instant::now());

    assert_eq!(lines, Vec::<Line<'static>>::new());
}

#[test]
fn display_lines_renders_elapsed_agent() {
    let now = Instant::now();
    let mut list = ActiveAgentList::new(FrameRequester::test_dummy());
    list.set_agents(vec![ActiveAgentEntry {
        name: " reviewer ".to_string(),
        started_at: now - Duration::from_secs(326),
        provider_model: None,
        total_tokens: None,
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(rendered, vec!["• Agents", "  └ □ reviewer (5m 26s)"]);
}

#[test]
fn display_lines_renders_provider_model() {
    let now = Instant::now();
    let mut list = ActiveAgentList::new(FrameRequester::test_dummy());
    list.set_agents(vec![ActiveAgentEntry {
        name: " reviewer ".to_string(),
        started_at: now - Duration::from_secs(326),
        provider_model: Some(" openai/gpt-5.5 ".to_string()),
        total_tokens: None,
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(
        rendered,
        vec!["• Agents", "  └ □ reviewer (5m 26s, openai/gpt-5.5)"]
    );
}

#[test]
fn display_lines_renders_provider_model_and_tokens() {
    let now = Instant::now();
    let mut list = ActiveAgentList::new(FrameRequester::test_dummy());
    list.set_agents(vec![ActiveAgentEntry {
        name: " reviewer ".to_string(),
        started_at: now - Duration::from_secs(1931),
        provider_model: Some(" openai/gpt-5.5 ".to_string()),
        total_tokens: Some(42_000),
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(
        rendered,
        vec!["• Agents", "  └ □ reviewer (32m 11s, openai/gpt-5.5, 42k)"]
    );
}

#[test]
fn display_lines_caps_visible_agents() {
    let now = Instant::now();
    let mut list = ActiveAgentList::new(FrameRequester::test_dummy());
    list.set_agents(
        (1..=8)
            .map(|index| ActiveAgentEntry {
                name: format!("agent-{index}"),
                started_at: now,
                provider_model: None,
                total_tokens: None,
                current_activity: None,
            })
            .collect(),
    );

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(
        rendered,
        vec![
            "• Agents",
            "  └ □ agent-1 (0s)",
            "    □ agent-2 (0s)",
            "    □ agent-3 (0s)",
            "    □ agent-4 (0s)",
            "    □ agent-5 (0s)",
            "    □ agent-6 (0s)",
            "    □ agent-7 (0s)",
            "    ... 1 more",
        ]
    );
}
