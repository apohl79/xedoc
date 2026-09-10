use super::stats_lines;
use insta::assert_snapshot;
use xedoc_app_server_protocol::TokenUsageOptimizerInsights;
use xedoc_app_server_protocol::TokenUsageOptimizerLevel;
use xedoc_app_server_protocol::TokenUsageOptimizerReadResponse;
use xedoc_app_server_protocol::TokenUsageOptimizerTopReduction;

#[test]
fn stats_lines_snapshot() {
    let response = TokenUsageOptimizerReadResponse {
        enabled: true,
        level: TokenUsageOptimizerLevel::Balanced,
        reduction_count: 667,
        tokens_saved: 18_711,
        cost_saved_usd: 0.12,
        insights: TokenUsageOptimizerInsights {
            by_kind: vec![
                breakdown("prose", 616),
                breakdown("diff", 32),
                breakdown("json", 19),
            ],
            by_reducer: Vec::new(),
            by_tool: Vec::new(),
            by_model: vec![breakdown_with_cost("test-model", 512, 0.09)],
            top_reductions: vec![
                top(
                    "shell",
                    "prose",
                    "call_vaPb123456789ni9",
                    34_536,
                    10_897,
                    5_900,
                ),
                top("shell", "diff", "call_fLq3c3a", 12_226, 6_333, 2_100),
                top("apply", "json", "call_skGdD9wW", 8_223, 3_405, 1_900),
            ],
            retrievals: 0,
            spilled: 0,
        },
    };

    let rendered = stats_lines(&response)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_snapshot!(rendered);
}

fn breakdown(
    dimension: &str,
    reductions: i64,
) -> xedoc_app_server_protocol::TokenUsageOptimizerBreakdown {
    xedoc_app_server_protocol::TokenUsageOptimizerBreakdown {
        dimension: dimension.to_owned(),
        reductions,
        bytes_in: 0,
        bytes_out: 0,
        retrievals: 0,
        reruns: 0,
        cost_saved_usd: None,
    }
}

fn top(
    tool_name: &str,
    kind: &str,
    call_id: &str,
    bytes_in: i64,
    bytes_out: i64,
    tokens_saved: i64,
) -> TokenUsageOptimizerTopReduction {
    TokenUsageOptimizerTopReduction {
        call_id: call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        kind: kind.to_owned(),
        bytes_in,
        bytes_out,
        tokens_saved,
        cost_saved_usd: None,
    }
}

fn breakdown_with_cost(
    dimension: &str,
    reductions: i64,
    cost_saved_usd: f64,
) -> xedoc_app_server_protocol::TokenUsageOptimizerBreakdown {
    xedoc_app_server_protocol::TokenUsageOptimizerBreakdown {
        dimension: dimension.to_owned(),
        reductions,
        bytes_in: 0,
        bytes_out: 0,
        retrievals: 0,
        reruns: 0,
        cost_saved_usd: Some(cost_saved_usd),
    }
}
