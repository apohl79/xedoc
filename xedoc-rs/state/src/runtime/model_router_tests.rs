use super::super::StateRuntime;
use super::super::test_support::unique_temp_dir;
use crate::ModelRouterDailyRecord;
use crate::ModelRouterInvocationRecord;
use pretty_assertions::assert_eq;

const DAY: i64 = 1_725_667_200;

#[derive(Debug, PartialEq)]
struct DailyCostProjection {
    day: i64,
    provider_id: String,
    model_slug: String,
    scope: String,
    reasoning_effort: Option<String>,
    invocations: i64,
    total_cost_usd: Option<f64>,
    normalized_baseline_usd: Option<f64>,
    estimated_savings_usd: Option<f64>,
    unknown_price_invocations: i64,
    unknown_baseline_price_invocations: i64,
}

impl From<ModelRouterDailyRecord> for DailyCostProjection {
    fn from(record: ModelRouterDailyRecord) -> Self {
        Self {
            day: record.day,
            provider_id: record.provider_id,
            model_slug: record.model_slug,
            scope: record.scope,
            reasoning_effort: record.reasoning_effort,
            invocations: record.invocations,
            total_cost_usd: record.total_cost_usd,
            normalized_baseline_usd: record.normalized_baseline_usd,
            estimated_savings_usd: record.estimated_savings_usd,
            unknown_price_invocations: record.unknown_price_invocations,
            unknown_baseline_price_invocations: record.unknown_baseline_price_invocations,
        }
    }
}

fn invocation(
    invocation_id: &str,
    total_cost_usd: Option<f64>,
    normalized_baseline_usd: Option<f64>,
    estimated_savings_usd: Option<f64>,
) -> ModelRouterInvocationRecord {
    ModelRouterInvocationRecord {
        invocation_id: invocation_id.to_string(),
        decision_id: None,
        ab_pair_id: None,
        ab_branch: None,
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        response_id: Some(format!("response-{invocation_id}")),
        invocation_kind: "router".to_string(),
        provider_id: "openai".to_string(),
        model_slug: "gpt-5".to_string(),
        reasoning_effort: Some("medium".to_string()),
        input_tokens: Some(1),
        cached_input_tokens: Some(0),
        output_tokens: Some(1),
        actual_input_price_usd_per_token: None,
        actual_cached_input_price_usd_per_token: None,
        actual_output_price_usd_per_token: None,
        actual_price_revision: None,
        input_cost_usd: None,
        cached_input_cost_usd: None,
        output_cost_usd: None,
        total_cost_usd,
        baseline_provider_id: None,
        baseline_model_slug: None,
        baseline_reasoning_effort: None,
        baseline_input_price_usd_per_token: None,
        baseline_cached_input_price_usd_per_token: None,
        baseline_output_price_usd_per_token: None,
        baseline_price_revision: None,
        normalized_baseline_usd,
        estimated_savings_usd,
        ab_experiment_overhead_usd: None,
        created_at: DAY + 1,
    }
}

#[tokio::test]
async fn daily_records_retain_priced_cost_subtotals_with_unpriced_invocations() {
    let xedoc_home = unique_temp_dir();
    let runtime = StateRuntime::init(xedoc_home.clone(), "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    runtime
        .insert_model_router_invocation(&invocation("priced", Some(3.0), Some(5.0), Some(2.0)))
        .await
        .expect("priced invocation should insert");
    runtime
        .insert_model_router_invocation(&invocation("unpriced", None, None, None))
        .await
        .expect("unpriced invocation should insert");

    let actual = runtime
        .model_router_daily_records(DAY, DAY)
        .await
        .expect("daily records should load")
        .into_iter()
        .map(DailyCostProjection::from)
        .collect::<Vec<_>>();
    runtime.close().await;
    let _ = tokio::fs::remove_dir_all(xedoc_home).await;

    assert_eq!(
        actual,
        vec![DailyCostProjection {
            day: DAY,
            provider_id: "openai".to_string(),
            model_slug: "gpt-5".to_string(),
            scope: "unattributed".to_string(),
            reasoning_effort: Some("medium".to_string()),
            invocations: 2,
            total_cost_usd: Some(3.0),
            normalized_baseline_usd: Some(5.0),
            estimated_savings_usd: Some(2.0),
            unknown_price_invocations: 1,
            unknown_baseline_price_invocations: 1,
        }]
    );
}
