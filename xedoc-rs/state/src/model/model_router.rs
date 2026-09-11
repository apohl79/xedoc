use sqlx::FromRow;
use xedoc_protocol::protocol::ModelRouterDecisionEvent;
use xedoc_protocol::protocol::ModelRouterEffectiveRoute;

/// Immutable metadata recorded for one model-router decision.
///
/// Prompt and output bodies are intentionally excluded. `prompt_sha256` is the
/// only prompt-derived value retained by this record.
#[derive(Clone, Debug, FromRow)]
pub struct ModelRouterDecisionRecord {
    pub decision_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub scope: String,
    pub parent_decision_id: Option<String>,
    pub classifier_revision: Option<String>,
    pub policy_revision: String,
    pub artifact_revision: Option<String>,
    pub artifact_sha256: Option<String>,
    pub class_id: Option<String>,
    pub score: Option<f64>,
    pub margin: Option<f64>,
    pub proposed_provider_id: Option<String>,
    pub proposed_model_slug: Option<String>,
    pub proposed_reasoning_effort: Option<String>,
    pub effective_provider_id: Option<String>,
    pub effective_model_slug: Option<String>,
    pub effective_reasoning_effort: Option<String>,
    pub disposition: String,
    pub reason: String,
    pub prompt_sha256: String,
    pub prompt_original_bytes: i64,
    pub prompt_truncated: bool,
    pub created_at: i64,
}

impl From<&ModelRouterDecisionEvent> for ModelRouterDecisionRecord {
    fn from(event: &ModelRouterDecisionEvent) -> Self {
        let (effective_provider_id, effective_model_slug, effective_reasoning_effort) =
            match &event.effective_route {
                ModelRouterEffectiveRoute::Available {
                    provider_id,
                    model_slug,
                    reasoning_effort,
                } => (
                    Some(provider_id.clone()),
                    Some(model_slug.clone()),
                    Some(reasoning_effort.clone()),
                ),
                ModelRouterEffectiveRoute::Unavailable => (None, None, None),
            };
        Self {
            decision_id: event.decision_id.clone(),
            thread_id: event.thread_id.clone(),
            turn_id: event.turn_id.clone(),
            scope: serde_json::to_string(&event.scope)
                .expect("model-router scope should serialize")
                .trim_matches('"')
                .to_string(),
            parent_decision_id: None,
            classifier_revision: None,
            policy_revision: event.policy_revision.clone(),
            artifact_revision: None,
            artifact_sha256: None,
            class_id: None,
            score: None,
            margin: None,
            proposed_provider_id: Some(event.proposed_provider_id.clone()),
            proposed_model_slug: Some(event.proposed_model_slug.clone()),
            proposed_reasoning_effort: Some(event.proposed_reasoning_effort.clone()),
            effective_provider_id,
            effective_model_slug,
            effective_reasoning_effort,
            disposition: serde_json::to_string(&event.disposition)
                .expect("model-router disposition should serialize")
                .trim_matches('"')
                .to_string(),
            reason: serde_json::to_string(&event.reason)
                .expect("model-router reason should serialize")
                .trim_matches('"')
                .to_string(),
            prompt_sha256: event.prompt_sha256.clone(),
            prompt_original_bytes: i64::try_from(event.prompt_original_bytes).unwrap_or(i64::MAX),
            prompt_truncated: event.prompt_truncated,
            created_at: event.created_at,
        }
    }
}

/// Immutable provider-usage observation attributed to a router decision.
#[derive(Clone, Debug, FromRow)]
pub struct ModelRouterInvocationRecord {
    pub invocation_id: String,
    pub decision_id: Option<String>,
    pub ab_pair_id: Option<String>,
    pub ab_branch: Option<String>,
    pub thread_id: String,
    pub turn_id: String,
    pub response_id: Option<String>,
    pub invocation_kind: String,
    pub provider_id: String,
    pub model_slug: String,
    pub reasoning_effort: Option<String>,
    pub input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub actual_input_price_usd_per_token: Option<f64>,
    pub actual_cached_input_price_usd_per_token: Option<f64>,
    pub actual_output_price_usd_per_token: Option<f64>,
    pub actual_price_revision: Option<String>,
    pub input_cost_usd: Option<f64>,
    pub cached_input_cost_usd: Option<f64>,
    pub output_cost_usd: Option<f64>,
    pub total_cost_usd: Option<f64>,
    pub baseline_provider_id: Option<String>,
    pub baseline_model_slug: Option<String>,
    pub baseline_reasoning_effort: Option<String>,
    pub baseline_input_price_usd_per_token: Option<f64>,
    pub baseline_cached_input_price_usd_per_token: Option<f64>,
    pub baseline_output_price_usd_per_token: Option<f64>,
    pub baseline_price_revision: Option<String>,
    pub normalized_baseline_usd: Option<f64>,
    pub estimated_savings_usd: Option<f64>,
    pub ab_experiment_overhead_usd: Option<f64>,
    pub created_at: i64,
}

/// Mutable lifecycle record for one bounded model-router A/B comparison.
#[derive(Clone, Debug, FromRow)]
pub struct ModelRouterAbOutcomeRecord {
    pub pair_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub routed_decision_id: Option<String>,
    pub orchestrator_decision_id: Option<String>,
    pub outcome: String,
    pub created_at: i64,
}

/// One deterministic UTC-day aggregate of router decisions and usage.
#[derive(Clone, Debug, FromRow)]
pub struct ModelRouterDailyRecord {
    pub day: i64,
    pub provider_id: String,
    pub model_slug: String,
    pub scope: String,
    pub reasoning_effort: Option<String>,
    pub decisions: i64,
    pub invocations: i64,
    pub input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub normalized_baseline_usd: Option<f64>,
    pub estimated_savings_usd: Option<f64>,
    pub ab_experiment_overhead_usd: Option<f64>,
    pub attributed_invocations: i64,
    pub unattributed_invocations: i64,
    pub missing_usage_invocations: i64,
    pub unknown_price_invocations: i64,
    pub classified_decisions: i64,
    pub fallback_decisions: i64,
    pub average_score: Option<f64>,
    pub average_margin: Option<f64>,
}
