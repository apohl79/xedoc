#![allow(clippy::expect_used)]

pub use xedoc_protocol::error;

#[path = "suite/client_suite.rs"]
mod suite;
mod test_binary_dispatch;
