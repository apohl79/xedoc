use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::ModelRouterReportDay;
use xedoc_app_server_protocol::ModelRouterReportDecision;
use xedoc_app_server_protocol::ModelRouterReportReadParams;
use xedoc_app_server_protocol::ModelRouterReportReadResponse;
use xedoc_rollout::state_db::StateDbHandle;

const MAX_DAYS: i64 = 90;
const DEFAULT_RECENT_LIMIT: usize = 25;
const MAX_RECENT_LIMIT: usize = 100;
const MAX_DAILY_RECORDS: usize = 500;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Clone)]
pub(crate) struct ModelRouterReportRequestProcessor {
    state_db: Option<StateDbHandle>,
}

impl ModelRouterReportRequestProcessor {
    pub(crate) fn new(state_db: Option<StateDbHandle>) -> Self {
        Self { state_db }
    }

    pub(crate) async fn read(
        &self,
        params: ModelRouterReportReadParams,
    ) -> Result<ModelRouterReportReadResponse, JSONRPCErrorError> {
        let today = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|time| time.and_utc().timestamp())
            .unwrap_or_default();
        let through_day = params
            .through_day
            .unwrap_or(today)
            .div_euclid(SECONDS_PER_DAY)
            * SECONDS_PER_DAY;
        let earliest_day = through_day
            .checked_sub((MAX_DAYS - 1) * SECONDS_PER_DAY)
            .ok_or_else(|| invalid_params("throughDay is out of range"))?;
        let from_day = params
            .from_day
            .unwrap_or(earliest_day)
            .div_euclid(SECONDS_PER_DAY)
            * SECONDS_PER_DAY;
        let from_day = from_day.clamp(earliest_day, through_day);
        let recent_limit = params
            .recent_limit
            .map(|limit| usize::try_from(limit).unwrap_or(MAX_RECENT_LIMIT))
            .unwrap_or(DEFAULT_RECENT_LIMIT)
            .clamp(1, MAX_RECENT_LIMIT);
        let Some(state_db) = &self.state_db else {
            return Ok(ModelRouterReportReadResponse {
                from_day,
                through_day,
                days: Vec::new(),
                days_truncated: false,
                recent_decisions: Vec::new(),
            });
        };
        let mut days = state_db
            .model_router_daily_records(from_day, through_day)
            .await
            .map_err(|err| internal_error(format!("failed to read model-router report: {err}")))?;
        let days_truncated = days.len() > MAX_DAILY_RECORDS;
        days.truncate(MAX_DAILY_RECORDS);
        let recent_decisions = state_db
            .recent_model_router_decisions(recent_limit)
            .await
            .map_err(|err| {
                internal_error(format!(
                    "failed to read recent model-router decisions: {err}"
                ))
            })?;

        Ok(ModelRouterReportReadResponse {
            from_day,
            through_day,
            days: days
                .into_iter()
                .map(|day| ModelRouterReportDay {
                    day: day.day,
                    provider_id: day.provider_id,
                    model_slug: day.model_slug,
                    scope: day.scope,
                    reasoning_effort: day.reasoning_effort,
                    decisions: day.decisions,
                    invocations: day.invocations,
                    input_tokens: day.input_tokens,
                    cached_input_tokens: day.cached_input_tokens,
                    output_tokens: day.output_tokens,
                    total_cost_usd: day.total_cost_usd,
                    normalized_baseline_usd: day.normalized_baseline_usd,
                    estimated_savings_usd: day.estimated_savings_usd,
                    ab_experiment_overhead_usd: day.ab_experiment_overhead_usd,
                    attributed_invocations: day.attributed_invocations,
                    unattributed_invocations: day.unattributed_invocations,
                    missing_usage_invocations: day.missing_usage_invocations,
                    unknown_price_invocations: day.unknown_price_invocations,
                    classified_decisions: day.classified_decisions,
                    fallback_decisions: day.fallback_decisions,
                    average_score: day.average_score,
                    average_margin: day.average_margin,
                })
                .collect(),
            days_truncated,
            recent_decisions: recent_decisions
                .into_iter()
                .map(|decision| {
                    let fallback = decision.disposition == "fallback";
                    ModelRouterReportDecision {
                        decision_id: decision.decision_id,
                        scope: decision.scope,
                        class_id: decision.class_id,
                        score: decision.score,
                        margin: decision.margin,
                        disposition: decision.disposition,
                        fallback,
                        reason: decision.reason,
                        policy_revision: decision.policy_revision,
                        proposed_provider_id: decision.proposed_provider_id,
                        proposed_model_slug: decision.proposed_model_slug,
                        proposed_reasoning_effort: decision.proposed_reasoning_effort,
                        effective_provider_id: decision.effective_provider_id,
                        effective_model_slug: decision.effective_model_slug,
                        effective_reasoning_effort: decision.effective_reasoning_effort,
                        created_at: decision.created_at,
                    }
                })
                .collect(),
        })
    }
}
