//! Typed failures for offline calibration.

use std::io;
use std::path::PathBuf;

/// An offline calibration failure that never includes prompt contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CalibrationError {
    /// A required file could not be read.
    #[error("could not read calibration file at {path}")]
    ReadFile {
        /// The requested file path.
        path: PathBuf,
        /// The operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A report or proposal could not be written.
    #[error("could not write calibration output at {path}")]
    WriteFile {
        /// The requested output path.
        path: PathBuf,
        /// The operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A JSON document did not match its privacy-safe schema.
    #[error("invalid {kind} JSON")]
    ParseJson {
        /// The kind of document that failed to parse.
        kind: DocumentKind,
        /// The parser failure.
        #[source]
        source: serde_json::Error,
    },
    /// A manifest field violated the reproducibility contract.
    #[error("invalid calibration manifest: {violation}")]
    InvalidManifest {
        /// The violated manifest constraint.
        violation: ManifestViolation,
    },
    /// A frozen input record violated the privacy-safe input contract.
    #[error("invalid calibration input record {record_index}: {violation}")]
    InvalidRecord {
        /// The zero-based record position.
        record_index: usize,
        /// The violated record constraint.
        violation: RecordViolation,
    },
    /// Two paths would overwrite a calibration source or another output.
    #[error("calibration paths must be distinct")]
    ConflictingPaths,
    /// Aggregate report serialization unexpectedly failed.
    #[error("could not serialize aggregate calibration output")]
    SerializeOutput {
        /// The serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// A raw local corpus could not be parsed without exposing its prompts.
    #[error("invalid local corpus")]
    InvalidCorpus,
    /// The checked local embedding artifact was unusable.
    #[error("invalid local embedding artifact")]
    Artifact {
        /// The verified artifact failure.
        #[source]
        source: crate::ArtifactError,
    },
    /// The local embedding worker could not embed a prompt.
    #[error("could not create a local embedding")]
    Embedding {
        /// The embedding failure.
        #[source]
        source: crate::EmbedError,
    },
    /// A bounded tuning input did not match the local aggregate schema.
    #[error("invalid local tuning input")]
    InvalidTuningInput,
    /// A policy candidate could not be loaded or validated.
    #[error("invalid model-router policy candidate")]
    Policy {
        /// The policy validation failure.
        #[source]
        source: xedoc_config::ModelRouterPolicyError,
    },
    /// A requested activation has not passed the frozen calibration gate.
    #[error("tuning proposal is not eligible for activation")]
    ProposalNotEligible,
}

/// A document kind accepted by the calibration command.
#[derive(Debug)]
pub enum DocumentKind {
    /// The reproducibility manifest.
    Manifest,
    /// The frozen held-out input.
    Input,
}

impl std::fmt::Display for DocumentKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest => formatter.write_str("manifest"),
            Self::Input => formatter.write_str("input"),
        }
    }
}

/// A reproducibility constraint that a manifest must satisfy.
#[derive(Debug)]
pub enum ManifestViolation {
    /// The manifest schema version is unsupported.
    UnsupportedSchemaVersion,
    /// A required identity or revision is empty.
    MissingIdentity,
    /// The corpus cutoff is not a positive Unix-second timestamp.
    InvalidCorpusCutoff,
    /// The split algorithm is not the frozen chronological held-out algorithm.
    InvalidSplitAlgorithm,
    /// The taxonomy is empty or contains duplicate classes.
    InvalidTaxonomy,
    /// The artifact identity, hash, or dimensions are invalid.
    InvalidArtifact,
    /// A required numeric gate is missing, non-finite, or out of range.
    InvalidQualityGate,
}

impl std::fmt::Display for ManifestViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedSchemaVersion => "unsupported schema version",
            Self::MissingIdentity => "missing identity or revision",
            Self::InvalidCorpusCutoff => "invalid corpus cutoff",
            Self::InvalidSplitAlgorithm => "invalid split algorithm",
            Self::InvalidTaxonomy => "invalid taxonomy",
            Self::InvalidArtifact => "invalid artifact identity",
            Self::InvalidQualityGate => "invalid numeric quality gate",
        })
    }
}

/// A privacy-safe input invariant that an individual record must satisfy.
#[derive(Debug)]
pub enum RecordViolation {
    /// The source ID is not an opaque bounded token.
    InvalidSourceId,
    /// The prompt hash is not a SHA-256 hex digest.
    InvalidPromptHash,
    /// Structural metadata exceeds its declared bounds.
    InvalidStructuralMetadata,
    /// The embedding dimension or values are invalid.
    InvalidEmbedding,
    /// A label is outside the frozen taxonomy.
    UnknownClass,
    /// Classifier output violates deterministic-output constraints.
    InvalidClassifierOutput,
}

impl std::fmt::Display for RecordViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidSourceId => "invalid opaque source ID",
            Self::InvalidPromptHash => "invalid prompt hash",
            Self::InvalidStructuralMetadata => "invalid bounded structural metadata",
            Self::InvalidEmbedding => "invalid embedding",
            Self::UnknownClass => "unknown taxonomy class",
            Self::InvalidClassifierOutput => "invalid deterministic classifier output",
        })
    }
}
