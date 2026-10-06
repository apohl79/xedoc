//! Bounded configured provider-budget consumption for scripted model routing.

use chrono::Datelike;
use chrono::TimeZone;
use chrono::Utc;
use serde_json::Map;
use serde_json::Value;
use xedoc_config::types::ProviderBudgetKind;

use crate::config::Config;
use crate::state_db_bridge::StateDbHandle;

const MAX_ROUTER_BUDGET_PROVIDERS: usize = 16;

pub(crate) async fn snapshot(
    config: &Config,
    state_db: Option<&StateDbHandle>,
) -> Map<String, Value> {
    let mut percentages = Map::new();
    let Some(state_db) = state_db else {
        return percentages;
    };
    let now = Utc::now();
    let mut budgets = config.provider_budgets.iter().collect::<Vec<_>>();
    budgets.sort_by_key(|(provider_id, _)| provider_id.as_str());
    for (provider_id, budget) in budgets.into_iter().take(MAX_ROUTER_BUDGET_PROVIDERS) {
        let Some(limit) = budget.usd.filter(|limit| limit.is_finite() && *limit > 0.0) else {
            continue;
        };
        if provider_id.len() > 64 {
            continue;
        }
        let started_at = match budget.kind {
            ProviderBudgetKind::None => continue,
            ProviderBudgetKind::Prepaid => budget.started_at.unwrap_or_else(|| now.timestamp()),
            ProviderBudgetKind::Monthly => {
                provider_budget_month_start(now, budget.reset_day.unwrap_or(1))
            }
        };
        match state_db.provider_cost_since(provider_id, started_at).await {
            Ok(used) if used.is_finite() => {
                percentages.insert(
                    provider_id.clone(),
                    Value::from(((used / limit) * 100.0).round().clamp(0.0, 10_000.0)),
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, provider_id, "failed to read model-router provider budget");
            }
        }
    }
    percentages
}

fn provider_budget_month_start(now: chrono::DateTime<Utc>, reset_day: u8) -> i64 {
    let current = provider_budget_reset_at(now.year(), now.month(), reset_day).unwrap_or(now);
    if current <= now {
        return current.timestamp();
    }
    let (year, month) = match now.month() {
        1 => (now.year() - 1, 12),
        month => (now.year(), month - 1),
    };
    provider_budget_reset_at(year, month, reset_day)
        .unwrap_or(now)
        .timestamp()
}

fn provider_budget_reset_at(year: i32, month: u32, reset_day: u8) -> Option<chrono::DateTime<Utc>> {
    let (next_year, next_month) = match month {
        12 => (year + 1, 1),
        month => (year, month + 1),
    };
    let last_day = Utc
        .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
        .single()?
        .checked_sub_days(chrono::Days::new(1))?
        .day();
    Utc.with_ymd_and_hms(
        year,
        month,
        u32::from(reset_day).clamp(1, last_day),
        0,
        0,
        0,
    )
    .single()
}
