//! Frame scheduling primitives shared by Codex TUI crates.

mod frame_rate_limiter;
mod frame_requester;

pub use frame_rate_limiter::MIN_FRAME_INTERVAL;
pub use frame_requester::FrameRequester;
