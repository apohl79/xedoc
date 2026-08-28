//! Turn timing state and profile accounting.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod turn_timing;

pub use turn_timing::TurnProfileTimingGuard;
pub use turn_timing::TurnTimingState;
pub use turn_timing::now_unix_timestamp_ms;
