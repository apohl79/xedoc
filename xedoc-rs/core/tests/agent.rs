#![allow(clippy::expect_used)]

pub use xedoc_protocol::error;

#[path = "suite/agent.rs"]
mod suite;
mod test_binary_dispatch;
