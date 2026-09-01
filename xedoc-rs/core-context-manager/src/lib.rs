//! Model-visible turn history and contextual-message preparation.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod audio;
mod contextual_content;
mod history;
mod image;
mod normalize;
pub mod updates;

pub use audio::estimate_audio_token_count;
pub use audio::prepare_response_items;
pub use contextual_content::has_non_contextual_dev_message_content;
pub use contextual_content::is_contextual_dev_message_content;
pub use contextual_content::is_contextual_user_message_content;
pub use history::ContextManager;
pub use history::content_items_to_text;
pub use history::estimate_item_token_count;
pub use history::is_user_turn_boundary;
pub use history::truncate_function_output_payload;
pub use image::prepare_image_response_items;
