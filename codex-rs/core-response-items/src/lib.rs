//! Translation from wire-level response items to persisted turn items.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod event_mapping;
mod stream_items;
mod web_search;

pub use event_mapping::parse_turn_item;
#[doc(hidden)]
pub use stream_items::completed_item_defers_mailbox_delivery_to_next_turn;
#[doc(hidden)]
pub use stream_items::last_assistant_message_from_item;
#[doc(hidden)]
pub use stream_items::raw_assistant_output_text_from_item;
#[doc(hidden)]
pub use stream_items::response_input_to_response_item;
#[doc(hidden)]
pub use stream_items::sanitize_agent_message;
pub use web_search::web_search_action_detail;

#[cfg(test)]
#[path = "event_mapping_tests.rs"]
mod tests;
