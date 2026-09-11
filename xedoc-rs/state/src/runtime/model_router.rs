use super::*;
use crate::ModelRouterAbOutcomeRecord;
use crate::ModelRouterDailyRecord;
use crate::ModelRouterDecisionRecord;
use crate::ModelRouterInvocationRecord;

const MODEL_ROUTER_RECENT_LIMIT: i64 = 100;
const MODEL_ROUTER_DAILY_LIMIT: i64 = 501;
const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_DAY_USIZE: usize = 86_400;

impl StateRuntime {
    /// Insert a decision exactly once so replay cannot rewrite recorded routing facts.
    pub async fn insert_model_router_decision(
        &self,
        decision: &ModelRouterDecisionRecord,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO model_router_decisions (
    decision_id, thread_id, turn_id, scope, parent_decision_id,
    classifier_revision, policy_revision, artifact_revision, artifact_sha256,
    class_id, score, margin,
    proposed_provider_id, proposed_model_slug, proposed_reasoning_effort,
    effective_provider_id, effective_model_slug, effective_reasoning_effort,
    disposition, reason, prompt_sha256, prompt_original_bytes, prompt_truncated, created_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(decision_id) DO NOTHING
            "#,
        )
        .bind(&decision.decision_id)
        .bind(&decision.thread_id)
        .bind(&decision.turn_id)
        .bind(&decision.scope)
        .bind(&decision.parent_decision_id)
        .bind(&decision.classifier_revision)
        .bind(&decision.policy_revision)
        .bind(&decision.artifact_revision)
        .bind(&decision.artifact_sha256)
        .bind(&decision.class_id)
        .bind(decision.score)
        .bind(decision.margin)
        .bind(&decision.proposed_provider_id)
        .bind(&decision.proposed_model_slug)
        .bind(&decision.proposed_reasoning_effort)
        .bind(&decision.effective_provider_id)
        .bind(&decision.effective_model_slug)
        .bind(&decision.effective_reasoning_effort)
        .bind(&decision.disposition)
        .bind(&decision.reason)
        .bind(&decision.prompt_sha256)
        .bind(decision.prompt_original_bytes)
        .bind(decision.prompt_truncated)
        .bind(decision.created_at)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Insert one provider-usage observation exactly once.
    pub async fn insert_model_router_invocation(
        &self,
        invocation: &ModelRouterInvocationRecord,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO model_router_invocations (
    invocation_id, decision_id, ab_pair_id, ab_branch, thread_id, turn_id, response_id, invocation_kind,
    provider_id, model_slug, reasoning_effort,
    input_tokens, cached_input_tokens, output_tokens,
    actual_input_price_usd_per_token, actual_cached_input_price_usd_per_token,
    actual_output_price_usd_per_token, actual_price_revision,
    input_cost_usd, cached_input_cost_usd, output_cost_usd, total_cost_usd,
    baseline_provider_id, baseline_model_slug, baseline_reasoning_effort,
    baseline_input_price_usd_per_token, baseline_cached_input_price_usd_per_token,
    baseline_output_price_usd_per_token, baseline_price_revision,
    normalized_baseline_usd, estimated_savings_usd, ab_experiment_overhead_usd, created_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(invocation_id) DO NOTHING
            "#,
        )
        .bind(&invocation.invocation_id)
        .bind(&invocation.decision_id)
        .bind(&invocation.ab_pair_id)
        .bind(&invocation.ab_branch)
        .bind(&invocation.thread_id)
        .bind(&invocation.turn_id)
        .bind(&invocation.response_id)
        .bind(&invocation.invocation_kind)
        .bind(&invocation.provider_id)
        .bind(&invocation.model_slug)
        .bind(&invocation.reasoning_effort)
        .bind(invocation.input_tokens)
        .bind(invocation.cached_input_tokens)
        .bind(invocation.output_tokens)
        .bind(invocation.actual_input_price_usd_per_token)
        .bind(invocation.actual_cached_input_price_usd_per_token)
        .bind(invocation.actual_output_price_usd_per_token)
        .bind(&invocation.actual_price_revision)
        .bind(invocation.input_cost_usd)
        .bind(invocation.cached_input_cost_usd)
        .bind(invocation.output_cost_usd)
        .bind(invocation.total_cost_usd)
        .bind(&invocation.baseline_provider_id)
        .bind(&invocation.baseline_model_slug)
        .bind(&invocation.baseline_reasoning_effort)
        .bind(invocation.baseline_input_price_usd_per_token)
        .bind(invocation.baseline_cached_input_price_usd_per_token)
        .bind(invocation.baseline_output_price_usd_per_token)
        .bind(&invocation.baseline_price_revision)
        .bind(invocation.normalized_baseline_usd)
        .bind(invocation.estimated_savings_usd)
        .bind(invocation.ab_experiment_overhead_usd)
        .bind(invocation.created_at)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Creates an A/B outcome or records its final human preference.
    pub async fn upsert_model_router_ab_outcome(
        &self,
        outcome: &ModelRouterAbOutcomeRecord,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO model_router_ab_outcomes (
    pair_id, thread_id, turn_id, routed_decision_id, orchestrator_decision_id, outcome, created_at
) VALUES (?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(pair_id) DO UPDATE SET
    routed_decision_id = COALESCE(excluded.routed_decision_id, model_router_ab_outcomes.routed_decision_id),
    orchestrator_decision_id = COALESCE(excluded.orchestrator_decision_id, model_router_ab_outcomes.orchestrator_decision_id),
    outcome = excluded.outcome
            "#,
        )
        .bind(&outcome.pair_id)
        .bind(&outcome.thread_id)
        .bind(&outcome.turn_id)
        .bind(&outcome.routed_decision_id)
        .bind(&outcome.orchestrator_decision_id)
        .bind(&outcome.outcome)
        .bind(outcome.created_at)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Read the newest decision metadata, capped to prevent unbounded UI or RPC responses.
    pub async fn recent_model_router_decisions(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<ModelRouterDecisionRecord>> {
        let limit = i64::try_from(limit)
            .unwrap_or(MODEL_ROUTER_RECENT_LIMIT)
            .clamp(1, MODEL_ROUTER_RECENT_LIMIT);
        let rows = sqlx::query_as::<_, ModelRouterDecisionRecord>(
            r#"
SELECT
    decision_id, thread_id, turn_id, scope, parent_decision_id,
    classifier_revision, policy_revision, artifact_revision, artifact_sha256,
    class_id, score, margin,
    proposed_provider_id, proposed_model_slug, proposed_reasoning_effort,
    effective_provider_id, effective_model_slug, effective_reasoning_effort,
    disposition, reason, prompt_sha256, prompt_original_bytes, prompt_truncated, created_at
FROM model_router_decisions
ORDER BY created_at DESC, decision_id DESC
LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(self.pool.as_ref())
        .await?;
        Ok(rows)
    }

    /// Rebuild one UTC day from immutable decision and invocation records.
    pub async fn roll_up_model_router_day(&self, day: i64) -> anyhow::Result<()> {
        let day = day.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
        let next_day = day + SECONDS_PER_DAY;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM model_router_daily WHERE day = ?")
            .bind(day)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            r#"
INSERT INTO model_router_daily (
    day, provider_id, model_slug, scope, reasoning_effort, decisions, invocations,
    input_tokens, cached_input_tokens, output_tokens, total_cost_usd,
    normalized_baseline_usd, estimated_savings_usd, ab_experiment_overhead_usd,
    attributed_invocations, unattributed_invocations, missing_usage_invocations,
    unknown_price_invocations, classified_decisions, fallback_decisions,
    average_score, average_margin
)
SELECT
    day, provider_id, model_slug, scope, reasoning_effort,
    SUM(decisions), SUM(invocations),
    SUM(input_tokens), SUM(cached_input_tokens), SUM(output_tokens), SUM(total_cost_usd),
    SUM(normalized_baseline_usd), SUM(estimated_savings_usd), SUM(ab_experiment_overhead_usd),
    SUM(attributed_invocations), SUM(unattributed_invocations), SUM(missing_usage_invocations),
    SUM(unknown_price_invocations), SUM(classified_decisions), SUM(fallback_decisions),
    AVG(average_score), AVG(average_margin)
FROM (
    SELECT
        ? AS day,
        COALESCE(effective_provider_id, 'unknown') AS provider_id,
        COALESCE(effective_model_slug, 'unknown') AS model_slug,
        scope,
        effective_reasoning_effort AS reasoning_effort,
        COUNT(*) AS decisions,
        0 AS invocations,
        NULL AS input_tokens,
        NULL AS cached_input_tokens,
        NULL AS output_tokens,
        NULL AS total_cost_usd,
        NULL AS normalized_baseline_usd,
        NULL AS estimated_savings_usd,
        NULL AS ab_experiment_overhead_usd,
        0 AS attributed_invocations,
        0 AS unattributed_invocations,
        0 AS missing_usage_invocations,
        0 AS unknown_price_invocations,
        SUM(CASE WHEN reason = 'classified' THEN 1 ELSE 0 END) AS classified_decisions,
        SUM(CASE WHEN disposition = 'fallback' THEN 1 ELSE 0 END) AS fallback_decisions,
        AVG(score) AS average_score,
        AVG(margin) AS average_margin
    FROM model_router_decisions
    WHERE created_at >= ? AND created_at < ?
    GROUP BY 2, 3, 4, 5

    UNION ALL

    SELECT
        ? AS day,
        i.provider_id,
        i.model_slug,
        COALESCE(d.scope, 'unattributed') AS scope,
        i.reasoning_effort,
        0 AS decisions,
        COUNT(*) AS invocations,
        CASE WHEN COUNT(*) = COUNT(i.input_tokens) THEN SUM(i.input_tokens) END,
        CASE WHEN COUNT(*) = COUNT(i.cached_input_tokens) THEN SUM(i.cached_input_tokens) END,
        CASE WHEN COUNT(*) = COUNT(i.output_tokens) THEN SUM(i.output_tokens) END,
        CASE WHEN COUNT(*) = COUNT(i.total_cost_usd) THEN SUM(i.total_cost_usd) END,
        CASE WHEN COUNT(*) = COUNT(i.normalized_baseline_usd) THEN SUM(i.normalized_baseline_usd) END,
        CASE WHEN COUNT(*) = COUNT(i.estimated_savings_usd) THEN SUM(i.estimated_savings_usd) END,
        CASE WHEN COUNT(*) = COUNT(i.ab_experiment_overhead_usd) THEN SUM(i.ab_experiment_overhead_usd) END,
        SUM(CASE WHEN i.decision_id IS NOT NULL THEN 1 ELSE 0 END) AS attributed_invocations,
        SUM(CASE WHEN i.decision_id IS NULL THEN 1 ELSE 0 END) AS unattributed_invocations,
        SUM(CASE
            WHEN i.input_tokens IS NULL
              OR i.cached_input_tokens IS NULL
              OR i.output_tokens IS NULL
            THEN 1
            ELSE 0
        END) AS missing_usage_invocations,
        SUM(CASE WHEN i.total_cost_usd IS NULL THEN 1 ELSE 0 END) AS unknown_price_invocations,
        0 AS classified_decisions,
        0 AS fallback_decisions,
        NULL AS average_score,
        NULL AS average_margin
    FROM model_router_invocations i
    LEFT JOIN model_router_decisions d ON d.decision_id = i.decision_id
    WHERE i.created_at >= ? AND i.created_at < ?
    GROUP BY 2, 3, 4, 5
)
GROUP BY 1, 2, 3, 4, 5
            "#,
        )
        .bind(day)
        .bind(day)
        .bind(next_day)
        .bind(day)
        .bind(day)
        .bind(next_day)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Return the persisted aggregates for an inclusive UTC-day range.
    pub async fn model_router_daily_records(
        &self,
        from_day: i64,
        through_day: i64,
    ) -> anyhow::Result<Vec<ModelRouterDailyRecord>> {
        let from_day = from_day.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
        let through_day = through_day
            .div_euclid(SECONDS_PER_DAY)
            .min(from_day.div_euclid(SECONDS_PER_DAY) + MODEL_ROUTER_DAILY_LIMIT - 1)
            * SECONDS_PER_DAY;
        for day in (from_day..=through_day).step_by(SECONDS_PER_DAY_USIZE) {
            self.roll_up_model_router_day(day).await?;
        }
        sqlx::query_as::<_, ModelRouterDailyRecord>(
            r#"
SELECT
    day, provider_id, model_slug, scope, reasoning_effort, decisions, invocations,
    input_tokens, cached_input_tokens, output_tokens, total_cost_usd,
    normalized_baseline_usd, estimated_savings_usd, ab_experiment_overhead_usd,
    attributed_invocations, unattributed_invocations, missing_usage_invocations,
    unknown_price_invocations, classified_decisions, fallback_decisions,
    average_score, average_margin
FROM model_router_daily
WHERE day >= ? AND day <= ?
ORDER BY day ASC, provider_id ASC, model_slug ASC, scope ASC, reasoning_effort ASC
LIMIT ?
            "#,
        )
        .bind(from_day)
        .bind(through_day)
        .bind(MODEL_ROUTER_DAILY_LIMIT)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(Into::into)
    }
}
