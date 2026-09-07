use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use ratatui::prelude::*;
use std::time::Instant;

/// A transient history cell shown while token-optimizer statistics are loading.
#[derive(Debug)]
pub struct TokenUsageOptimizerStatsLoadingCell {
    start_time: Instant,
    animations_enabled: bool,
}

impl TokenUsageOptimizerStatsLoadingCell {
    pub fn new(animations_enabled: bool) -> Self {
        Self {
            start_time: Instant::now(),
            animations_enabled,
        }
    }
}

impl super::HistoryCell for TokenUsageOptimizerStatsLoadingCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![
            vec![
                activity_indicator(
                    Some(self.start_time),
                    MotionMode::from_animations_enabled(self.animations_enabled),
                    ReducedMotionIndicator::StaticBullet,
                )
                .unwrap_or_else(|| "•".dim()),
                " ".into(),
                "Reading token optimizer stats".bold(),
                "…".dim(),
            ]
            .into(),
        ]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from("Reading token optimizer stats...")]
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled {
            return None;
        }
        Some((self.start_time.elapsed().as_millis() / 50) as u64)
    }
}

/// Convenience constructor for [`TokenUsageOptimizerStatsLoadingCell`].
pub fn new_token_usage_optimizer_stats_loading(
    animations_enabled: bool,
) -> TokenUsageOptimizerStatsLoadingCell {
    TokenUsageOptimizerStatsLoadingCell::new(animations_enabled)
}
