#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod client;
pub mod client_common;
pub mod responses_metadata;

mod util;

pub use client::ModelClient;
pub use client::ModelClientSession;
pub use client::X_CODEX_INSTALLATION_ID_HEADER;
pub use client::X_CODEX_TURN_METADATA_HEADER;
pub use client::X_RESPONSESAPI_INCLUDE_TIMING_METRICS_HEADER;
pub use client_common::Prompt;
pub use client_common::ResponseEvent;
pub use client_common::ResponseStream;
pub use responses_metadata::CodexResponsesMetadata;
