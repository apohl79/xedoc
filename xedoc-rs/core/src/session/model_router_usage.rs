//! Persists exact provider completion usage for model-router analysis.

use xedoc_model_provider_info::ModelTokenPrices;
use xedoc_protocol::protocol::TokenUsage;
use xedoc_state::ModelRouterInvocationRecord;

use super::Session;
use super::turn_context::TurnContext;

impl Session {
    pub(crate) async fn record_model_router_invocation(
        &self,
        turn_context: &TurnContext,
        response_id: Option<&str>,
        token_usage: Option<&TokenUsage>,
        invocation_kind: &str,
    ) {
        let Some(state_db) = self.state_db() else {
            return;
        };
        let provider_id = &turn_context.config.model_provider_id;
        let model_slug = &turn_context.model_info.slug;
        let price = turn_context
            .config
            .model_providers
            .get(provider_id)
            .and_then(|provider| provider.model_prices.as_ref())
            .and_then(|prices| prices.get(model_slug));
        let (input_tokens, cached_input_tokens, output_tokens) =
            token_usage.map_or((None, None, None), |usage| {
                (
                    Some(usage.non_cached_input().max(0)),
                    Some(usage.cached_input().max(0)),
                    Some(usage.output_tokens.max(0)),
                )
            });
        let prices = token_usage
            .zip(price)
            .map(|(usage, price)| invocation_prices(usage, price));
        let ab_pair = self
            .services
            .agent_control
            .ab_pair_transport_for_thread(self.thread_id);
        let decision_id = ab_pair
            .as_ref()
            .and_then(|metadata| metadata.router_decision_id.clone())
            .or(self.model_router_decision_id(&turn_context.sub_id).await);
        let invocation = ModelRouterInvocationRecord {
            invocation_id: model_router_invocation_id(
                &self.thread_id.to_string(),
                &turn_context.sub_id,
                response_id,
                invocation_kind,
            ),
            decision_id,
            ab_pair_id: ab_pair.as_ref().map(|metadata| metadata.pair_id.clone()),
            ab_branch: ab_pair.map(|metadata| match metadata.branch {
                crate::agent_communication::AbPairBranch::Routed => "routed".to_string(),
                crate::agent_communication::AbPairBranch::Orchestrator => {
                    "orchestrator".to_string()
                }
            }),
            thread_id: self.thread_id.to_string(),
            turn_id: turn_context.sub_id.clone(),
            response_id: response_id.map(str::to_string),
            invocation_kind: invocation_kind.to_string(),
            provider_id: provider_id.clone(),
            model_slug: model_slug.clone(),
            reasoning_effort: turn_context
                .reasoning_effort
                .as_ref()
                .map(ToString::to_string),
            input_tokens,
            cached_input_tokens,
            output_tokens,
            actual_input_price_usd_per_token: prices.map(|prices| prices.input),
            actual_cached_input_price_usd_per_token: prices.map(|prices| prices.cached_input),
            actual_output_price_usd_per_token: prices.map(|prices| prices.output),
            actual_price_revision: None,
            input_cost_usd: prices
                .zip(input_tokens)
                .map(|(prices, tokens)| prices.input * tokens as f64),
            cached_input_cost_usd: prices
                .zip(cached_input_tokens)
                .map(|(prices, tokens)| prices.cached_input * tokens as f64),
            output_cost_usd: prices
                .zip(output_tokens)
                .map(|(prices, tokens)| prices.output * tokens as f64),
            total_cost_usd: prices.zip(token_usage).map(|(prices, usage)| {
                prices.input * usage.non_cached_input().max(0) as f64
                    + prices.cached_input * usage.cached_input().max(0) as f64
                    + prices.output * usage.output_tokens.max(0) as f64
            }),
            baseline_provider_id: None,
            baseline_model_slug: None,
            baseline_reasoning_effort: None,
            baseline_input_price_usd_per_token: None,
            baseline_cached_input_price_usd_per_token: None,
            baseline_output_price_usd_per_token: None,
            baseline_price_revision: None,
            normalized_baseline_usd: None,
            estimated_savings_usd: None,
            ab_experiment_overhead_usd: None,
            created_at: crate::turn_timing::now_unix_timestamp_ms() / 1_000,
        };
        if let Err(error) = state_db.insert_model_router_invocation(&invocation).await {
            tracing::warn!(%error, "failed to persist model-router invocation");
        }
    }
}

fn model_router_invocation_id(
    thread_id: &str,
    turn_id: &str,
    response_id: Option<&str>,
    invocation_kind: &str,
) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!(
            "{thread_id}\u{1f}{turn_id}\u{1f}{}\u{1f}{invocation_kind}",
            response_id.unwrap_or_default()
        )
        .as_bytes(),
    )
    .to_string()
}

#[derive(Clone, Copy)]
struct InvocationPrices {
    input: f64,
    cached_input: f64,
    output: f64,
}

fn invocation_prices(usage: &TokenUsage, prices: &ModelTokenPrices) -> InvocationPrices {
    let long_context = usage.total_tokens > 272_000;
    let input = if long_context {
        prices
            .long_context_input_price_per_1m_tokens
            .unwrap_or(prices.input_price_per_1m_tokens)
    } else {
        prices.input_price_per_1m_tokens
    };
    let cached_input = if long_context {
        prices
            .long_context_cached_input_price_per_1m_tokens
            .or(prices.cached_input_price_per_1m_tokens)
            .unwrap_or(input)
    } else {
        prices
            .cached_input_price_per_1m_tokens
            .unwrap_or(prices.input_price_per_1m_tokens)
    };
    let output = if long_context {
        prices
            .long_context_output_price_per_1m_tokens
            .unwrap_or(prices.output_price_per_1m_tokens)
    } else {
        prices.output_price_per_1m_tokens
    };
    InvocationPrices {
        input: input / 1_000_000.0,
        cached_input: cached_input / 1_000_000.0,
        output: output / 1_000_000.0,
    }
}
