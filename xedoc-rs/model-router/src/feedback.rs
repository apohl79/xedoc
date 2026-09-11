//! Local, append-only classifier feedback and incremental recalibration.

use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use crate::FastEmbedder;
use crate::LocalArtifact;
use crate::MAX_PROMPT_BYTES;
use crate::SyncEmbedder;

const MAX_FEEDBACK_RECORDS: usize = 2_000;
const PRIOR_HEAD_WEIGHT: f32 = 64.0;
/// Persist one explicit classifier correction for the next calibration run.
///
/// # Errors
///
/// Returns an I/O error when the local feedback log cannot be appended.
pub fn append_classifier_feedback(
    path: &Path,
    source_id: &str,
    recorded_at_unix_seconds: i64,
    label: &str,
    prompt: &str,
) -> Result<(), std::io::Error> {
    if source_id.is_empty()
        || label.is_empty()
        || prompt.is_empty()
        || prompt.len() > MAX_PROMPT_BYTES
    {
        return Ok(());
    }
    let record = FeedbackRecord {
        source_id,
        recorded_at_unix_seconds,
        label,
        prompt,
    };
    let bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    let mut output = OpenOptions::new().create(true).append(true).open(path)?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")
}

/// Summary of an incremental feedback calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedbackCalibrationReport {
    /// Number of explicit corrections incorporated into the classifier heads.
    pub feedback_records: usize,
    /// Number of class heads updated by the corrections.
    pub updated_classes: usize,
}

/// Re-embed explicit corrections and atomically update affected policy heads.
///
/// Existing heads are retained as a bounded prior so a small number of
/// corrections improves the matching class without discarding the initial
/// calibration corpus. Records whose labels are not active policy classes are
/// ignored.
///
/// # Errors
///
/// Returns an error when local feedback, the artifact, or the policy cannot be
/// read or the updated policy cannot be written.
pub fn recalibrate_classifier_from_feedback(
    policy_path: &Path,
    feedback_path: &Path,
    artifact_path: &Path,
) -> Result<FeedbackCalibrationReport, FeedbackCalibrationError> {
    let mut policy = xedoc_config::load_model_router_policy(policy_path)
        .map_err(FeedbackCalibrationError::Policy)?;
    let records = read_feedback(feedback_path)?;
    let active_labels = policy
        .classes
        .iter()
        .map(|class| class.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let records = records
        .into_iter()
        .filter(|record| active_labels.contains(record.label.as_str()))
        .collect::<Vec<_>>();
    if records.is_empty() {
        return Ok(FeedbackCalibrationReport {
            feedback_records: 0,
            updated_classes: 0,
        });
    }

    let artifact =
        LocalArtifact::load(artifact_path).map_err(FeedbackCalibrationError::Artifact)?;
    if artifact.descriptor.sha256 != policy.embedding.artifact_sha256
        || artifact.descriptor.dimensions != policy.embedding.dimensions
    {
        return Err(FeedbackCalibrationError::ArtifactPolicyMismatch);
    }
    let embedder = FastEmbedder::from_local_artifact(&artifact)
        .map_err(FeedbackCalibrationError::Embedding)?;
    let mut sums = BTreeMap::<String, (usize, Vec<f32>)>::new();
    for record in &records {
        let embedding = embedder
            .embed_sync(&record.prompt)
            .map_err(FeedbackCalibrationError::Embedding)?;
        let entry = sums
            .entry(record.label.clone())
            .or_insert_with(|| (0, vec![0.0; embedding.len()]));
        entry
            .1
            .iter_mut()
            .zip(embedding)
            .for_each(|(sum, value)| *sum += value);
        entry.0 += 1;
    }

    let mut updated_classes = 0;
    for class in &mut policy.classes {
        let Some((count, sum)) = sums.get(&class.id) else {
            continue;
        };
        let prior = class
            .weights
            .as_ref()
            .map_or_else(|| vec![0.0; sum.len()], Clone::clone);
        class.weights = Some(
            prior
                .iter()
                .zip(sum)
                .map(|(weight, feedback)| {
                    (weight * PRIOR_HEAD_WEIGHT + feedback) / (PRIOR_HEAD_WEIGHT + *count as f32)
                })
                .collect(),
        );
        updated_classes += 1;
    }
    let parameters =
        serde_json::to_vec(&policy.classes).map_err(FeedbackCalibrationError::Serialize)?;
    let digest = format!("{:x}", Sha256::digest(parameters));
    policy.classifier.parameters_sha256 = digest.clone();
    policy.classifier.revision = format!("feedback-{}", &digest[..12]);
    policy.policy_revision = format!("feedback-{}", &digest[..12]);
    xedoc_config::write_model_router_policy(policy_path, &policy)
        .map_err(FeedbackCalibrationError::Policy)?;

    Ok(FeedbackCalibrationReport {
        feedback_records: records.len(),
        updated_classes,
    })
}

#[derive(Serialize)]
struct FeedbackRecord<'a> {
    source_id: &'a str,
    recorded_at_unix_seconds: i64,
    label: &'a str,
    prompt: &'a str,
}

#[derive(Deserialize)]
struct StoredFeedbackRecord {
    source_id: String,
    recorded_at_unix_seconds: i64,
    label: String,
    prompt: String,
}

fn read_feedback(path: &Path) -> Result<Vec<StoredFeedbackRecord>, FeedbackCalibrationError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(FeedbackCalibrationError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    contents
        .lines()
        .take(MAX_FEEDBACK_RECORDS)
        .map(|line| {
            let record: StoredFeedbackRecord =
                serde_json::from_str(line).map_err(FeedbackCalibrationError::Deserialize)?;
            (record.source_id.len() <= 128
                && record.recorded_at_unix_seconds > 0
                && !record.label.is_empty()
                && !record.prompt.is_empty()
                && record.prompt.len() <= MAX_PROMPT_BYTES)
                .then_some(record)
                .ok_or(FeedbackCalibrationError::InvalidRecord)
        })
        .collect()
}

/// Errors raised while applying explicit local classifier corrections.
#[derive(Debug, thiserror::Error)]
pub enum FeedbackCalibrationError {
    #[error("could not read model-router feedback at {path}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("model-router feedback contains an invalid record")]
    InvalidRecord,
    #[error("could not parse model-router feedback")]
    Deserialize(#[source] serde_json::Error),
    #[error("could not serialize model-router classifier heads")]
    Serialize(#[source] serde_json::Error),
    #[error("could not load or write the model-router policy")]
    Policy(#[source] xedoc_config::ModelRouterPolicyError),
    #[error("could not load the model-router embedding artifact")]
    Artifact(#[source] crate::ArtifactError),
    #[error("could not embed model-router feedback")]
    Embedding(#[source] crate::EmbedError),
    #[error("the model-router policy and embedding artifact do not match")]
    ArtifactPolicyMismatch,
}
