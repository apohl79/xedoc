//! Offline calibration support for the experimental model router.

mod artifact;
mod calibration;
mod error;
mod mode;
mod routing;
mod schema;
mod task;
mod tuning;
mod worker;

pub use artifact::ArtifactDescriptor;
pub use artifact::ArtifactError;
pub use artifact::LocalArtifact;
pub use error::CalibrationError;
pub use mode::RouterMode;
pub use routing::ClassRoute;
pub use routing::DecisionReason;
pub use routing::ModelCandidate;
pub use routing::ModelCatalog;
pub use routing::ModelRoute;
pub use routing::ReasoningEffort;
pub use routing::RouteDecision;
pub use routing::RouteDisposition;
pub use routing::RouteProfile;
pub use routing::RoutingPolicy;
pub use routing::decide;
pub use routing::finalize_decision;
pub use task::MAX_PROMPT_BYTES;
pub use task::PromptMetadata;
pub use task::RouteScope;
pub use task::TaskEnvelope;
pub use task::normalize_task;
pub use tuning::activate_tuning_proposal;
pub use tuning::inspect_tuning_proposal;
pub use tuning::propose_tuning;
pub use tuning::rollback_tuning_policy;
pub use worker::BoundedEmbedder;
pub use worker::EmbedError;
pub use worker::FastEmbedder;
pub use worker::SyncEmbedder;
pub use worker::TaskEmbedder;

use std::path::Path;

/// Calibrate a checked local artifact from a bounded local labelled corpus.
///
/// Raw prompts remain in process memory. The frozen input, report, and proposal
/// contain only hashes, structural metadata, embeddings, labels, and aggregates.
///
/// # Errors
///
/// Returns [`CalibrationError`] when paths cannot be read or written, JSON is
/// malformed, or the manifest and input fail reproducibility validation.
pub fn calibrate(
    manifest_path: &Path,
    corpus_path: &Path,
    artifact_path: &Path,
    frozen_input_path: &Path,
    report_path: &Path,
    proposal_path: &Path,
) -> Result<(), CalibrationError> {
    calibration::calibrate(
        manifest_path,
        corpus_path,
        artifact_path,
        frozen_input_path,
        report_path,
        proposal_path,
    )
}
