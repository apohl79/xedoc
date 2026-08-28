//! Picker and preview integration for terminal pets.

#![deny(clippy::print_stdout, clippy::print_stderr)]

mod picker;
mod preview;

pub use picker::PET_PICKER_VIEW_ID;
pub use picker::build_pet_picker_params;
pub use preview::PetPickerPreviewState;
