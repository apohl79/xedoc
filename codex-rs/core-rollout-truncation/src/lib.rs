//! Rollout truncation around persisted and effective turn boundaries.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod truncation;

pub use truncation::fork_turn_positions_in_rollout;
pub use truncation::initial_history_has_prior_user_turns;
pub use truncation::truncate_rollout_after_turn_id;
pub use truncation::truncate_rollout_before_nth_user_message_from_start;
pub use truncation::truncate_rollout_before_turn_id;
pub use truncation::truncate_rollout_to_last_n_fork_turns;
pub use truncation::user_message_positions_in_rollout;

#[cfg(test)]
#[path = "truncation_tests.rs"]
mod tests;
