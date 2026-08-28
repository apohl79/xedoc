//! Translation from wire-level response items to persisted turn items.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod event_mapping;
mod web_search;

pub use event_mapping::parse_turn_item;
pub use web_search::web_search_action_detail;

#[cfg(test)]
#[path = "event_mapping_tests.rs"]
mod tests;
