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
        token_usage: None,
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(
        rendered,
        vec!["• Agents", "  └ □ reviewer Working... (5m 26s)"]
    );
}

#[test]
fn display_lines_header_uses_lighter_gray_background_and_black_foreground() {
    let now = Instant::now();
    let mut list = ActiveAgentList::new(FrameRequester::test_dummy());
    list.set_agents(vec![ActiveAgentEntry {
        name: "reviewer".to_string(),
        started_at: now,
        provider_model: None,
        total_tokens: None,
        token_usage: None,
        current_activity: None,
    }]);

    let header = &list.display_lines_at(/*width*/ 80, now)[0];
    let header_style = crate::city_lights::active_list_header_style();

    assert_eq!(header.spans[0].style.bg, None);
    assert_eq!(header.spans[0].style.fg, None);
    for span in &header.spans[1..] {
        assert_eq!(span.style.bg, header_style.bg);
        assert_eq!(span.style.fg, header_style.fg);
    }
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
        token_usage: None,
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(
        rendered,
        vec![
            "• Agents",
            "  └ □ reviewer Working... (5m 26s, openai/gpt-5.5)"
        ]
    );
}

#[test]
fn display_lines_with_reasoning_effort_snapshot() {
    let now = Instant::now();
    let mut list = ActiveAgentList::new(FrameRequester::test_dummy());
    list.set_agents(vec![ActiveAgentEntry {
        name: "review_stack3_61_70".to_string(),
        started_at: now - Duration::from_secs(9),
        provider_model: Some("openai/gpt-5.6-terra/medium".to_string()),
        total_tokens: None,
        token_usage: Some(TokenUsage {
            input_tokens: 175_000,
            output_tokens: 2_540,
            total_tokens: 177_540,
            ..Default::default()
        }),
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 120, now)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    insta::assert_snapshot!(rendered);
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
        token_usage: None,
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(
        rendered,
        vec![
            "• Agents",
            "  └ □ reviewer Working... (32m 11s, openai/gpt-5.5, 42k)"
        ]
    );
}

#[test]
fn display_lines_renders_input_and_output_tokens() {
    let now = Instant::now();
    let mut list = ActiveAgentList::new(FrameRequester::test_dummy());
    list.set_agents(vec![ActiveAgentEntry {
        name: "reviewer".to_string(),
        started_at: now,
        provider_model: None,
        total_tokens: Some(42_000),
        token_usage: Some(TokenUsage {
            input_tokens: 40_000,
            output_tokens: 2_000,
            total_tokens: 42_000,
            ..Default::default()
        }),
        current_activity: None,
    }]);

    let rendered = list
        .display_lines_at(/*width*/ 80, now)
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect::<Vec<String>>();

    assert_eq!(
        rendered,
        vec!["• Agents", "  └ □ reviewer Working... (0s, ↓40k, ↑2k)"]
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
                token_usage: None,
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
            "  └ □ agent-1 Working... (0s)",
            "    □ agent-2 Working... (0s)",
            "    □ agent-3 Working... (0s)",
            "    □ agent-4 Working... (0s)",
            "    □ agent-5 Working... (0s)",
            "    □ agent-6 Working... (0s)",
            "    □ agent-7 Working... (0s)",
            "    ... 1 more",
        ]
    );
}
