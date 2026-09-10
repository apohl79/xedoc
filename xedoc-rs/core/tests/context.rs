#![allow(clippy::expect_used)]

pub use xedoc_protocol::error;

#[path = "suite/context.rs"]
mod suite;
mod test_binary_dispatch;
