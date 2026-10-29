//! Helpers for mapping thread-goal state into the compact status-line indicator.

use std::time::Instant;
use xedoc_app_server_protocol::ThreadGoal as AppThreadGoal;
use xedoc_app_server_protocol::ThreadGoalStatus as AppThreadGoalStatus;

use crate::bottom_pane::GoalStatusIndicator;
use crate::goal_display::format_goal_elapsed_seconds;
use crate::status::format_tokens_compact;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GoalStatusState {
    goal: AppThreadGoal,
    observed_at: Instant,
}

impl GoalStatusState {
    pub(super) fn new(goal: AppThreadGoal, observed_at: Instant) -> Self {
        Self { goal, observed_at }
    }

    pub(super) fn is_active(&self) -> bool {
        self.goal.status == AppThreadGoalStatus::Active
    }

    pub(super) fn indicator(
        &self,
        now: Instant,
        active_turn_started_at: Option<Instant>,
    ) -> Option<GoalStatusIndicator> {
        let mut goal = self.goal.clone();
        if goal.status == AppThreadGoalStatus::Active
            && let Some(active_turn_started_at) = active_turn_started_at
        {
            let baseline = self.observed_at.max(active_turn_started_at);
            let active_seconds = now.saturating_duration_since(baseline).as_secs();
            goal.time_used_seconds = goal
                .time_used_seconds
                .saturating_add(i64::try_from(active_seconds).unwrap_or(i64::MAX));
        }
        goal_status_indicator_from_app_goal(&goal)
    }
}

pub(super) fn goal_status_indicator_from_app_goal(
    goal: &AppThreadGoal,
) -> Option<GoalStatusIndicator> {
    match goal.status {
        AppThreadGoalStatus::Active => Some(GoalStatusIndicator::Active {
            usage: active_goal_usage(goal.token_budget, goal.tokens_used, goal.time_used_seconds),
        }),
        AppThreadGoalStatus::Paused => Some(GoalStatusIndicator::Paused),
        AppThreadGoalStatus::Blocked => Some(GoalStatusIndicator::Blocked),
        AppThreadGoalStatus::UsageLimited => Some(GoalStatusIndicator::UsageLimited),
        AppThreadGoalStatus::BudgetLimited => Some(GoalStatusIndicator::BudgetLimited {
            usage: stopped_goal_budget_usage(goal.token_budget, goal.tokens_used),
        }),
        AppThreadGoalStatus::Complete => Some(GoalStatusIndicator::Complete {
            usage: Some(completed_goal_usage(
                goal.token_budget,
                goal.tokens_used,
                goal.time_used_seconds,
            )),
        }),
    }
}

fn active_goal_usage(
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
) -> Option<String> {
    if let Some(token_budget) = token_budget {
        return Some(format!(
            "{} / {}",
            format_tokens_compact(tokens_used),
            format_tokens_compact(token_budget)
        ));
    }

    Some(format_goal_elapsed_seconds(time_used_seconds))
}

fn stopped_goal_budget_usage(token_budget: Option<i64>, tokens_used: i64) -> Option<String> {
    token_budget.map(|token_budget| {
        format!(
            "{} / {} tokens",
            format_tokens_compact(tokens_used),
            format_tokens_compact(token_budget)
        )
    })
}

fn completed_goal_usage(
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
) -> String {
    if token_budget.is_some() {
        return format!("{} tokens", format_tokens_compact(tokens_used));
    }

    format_goal_elapsed_seconds(time_used_seconds)
}
