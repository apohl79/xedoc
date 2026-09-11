//! Offline, review-gated model-router policy tuning.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use xedoc_config::ModelRouterPolicy;
use xedoc_config::ModelRouterPolicyReview;
use xedoc_config::ModelRouterPolicyRevisionStore;
use xedoc_utils_path::resolve_symlink_write_paths;
use xedoc_utils_path::write_atomically;

use crate::CalibrationError;
use crate::error::DocumentKind;
use crate::schema::FrozenInput;
use crate::schema::Manifest;
use crate::schema::Record;
use crate::schema::Split;

const MAX_OBSERVATIONS: usize = 10_000;
const MAX_DRIFT_SAMPLES: usize = 10_000;
const TUNING_SCHEMA_VERSION: u32 = 1;

/// Stage and assess a policy candidate without changing the active mapping.
///
/// # Errors
///
/// Returns [`CalibrationError`] when a local input is malformed, the candidate
/// policy is invalid, or an output cannot be written.
pub fn propose_tuning(
    manifest_path: &Path,
    frozen_input_path: &Path,
    frozen_report_path: &Path,
    evidence_path: &Path,
    drift_path: &Path,
    candidate_policy_path: &Path,
    active_policy_path: &Path,
    revisions_dir: Option<&Path>,
    proposal_path: &Path,
) -> Result<(), CalibrationError> {
    validate_proposal_paths(
        manifest_path,
        frozen_input_path,
        frozen_report_path,
        evidence_path,
        drift_path,
        candidate_policy_path,
        active_policy_path,
        proposal_path,
    )?;
    let manifest: Manifest = read_json(manifest_path, DocumentKind::Manifest)?;
    let frozen: FrozenInput = read_json(frozen_input_path, DocumentKind::Input)?;
    let report: FrozenBenchmarkReport = read_tuning_json(frozen_report_path)?;
    let evidence: TuningEvidence = read_tuning_json(evidence_path)?;
    let drift: DriftSamples = read_tuning_json(drift_path)?;
    let candidate =
        xedoc_config::load_model_router_policy(candidate_policy_path).map_err(policy_error)?;

    validate_tuning_inputs(&manifest, &frozen, &report, &evidence, &drift, &candidate)?;
    let frozen_bytes = read_bytes(frozen_input_path)?;
    let frozen_sha256 = sha256(&frozen_bytes);
    let frozen_evaluation = evaluate_frozen_policy(&manifest, &frozen, &candidate);
    let evidence_summary = summarize_evidence(&evidence);
    let drift_summary = summarize_drift(&drift, &frozen);
    let eligible_for_review = report.passed
        && report.manifest_id == manifest.manifest_id
        && report.frozen_input_sha256 == frozen_sha256
        && frozen_evaluation.passed
        && !drift.samples.is_empty();
    let proposal = TuningProposal {
        schema_version: TUNING_SCHEMA_VERSION,
        candidate_revision: candidate.policy_revision,
        candidate_policy_sha256: sha256(&read_bytes(candidate_policy_path)?),
        frozen_benchmark: FrozenBenchmarkComparison {
            source_report_passed: report.passed,
            heldout_gate_passed: frozen_evaluation.passed,
            metrics: frozen_evaluation.metrics,
            gates: frozen_evaluation.gates,
        },
        aggregate: evidence_summary,
        drift: drift_summary,
        eligible_for_review,
        activation: if eligible_for_review {
            ActivationState::ReviewRequired
        } else {
            ActivationState::BlockedByCalibrationGate
        },
    };
    write_json(proposal_path, &proposal)?;
    if !eligible_for_review {
        return Err(CalibrationError::ProposalNotEligible);
    }
    revision_store(active_policy_path, revisions_dir)
        .stage(candidate_policy_path)
        .map_err(policy_error)?;
    Ok(())
}

/// Inspect a local tuning proposal and its staged/active policy identities.
///
/// # Errors
///
/// Returns [`CalibrationError`] when the proposal is malformed or its staged
/// policy cannot be inspected.
pub fn inspect_tuning_proposal(
    proposal_path: &Path,
    active_policy_path: &Path,
    revisions_dir: Option<&Path>,
) -> Result<String, CalibrationError> {
    let proposal: TuningProposal = read_tuning_json(proposal_path)?;
    let store = revision_store(active_policy_path, revisions_dir);
    let staged = store
        .inspect(&proposal.candidate_revision)
        .map_err(policy_error)?;
    let active_revision = xedoc_config::load_model_router_policy(active_policy_path)
        .ok()
        .map(|policy| policy.policy_revision);
    serde_json::to_string_pretty(&serde_json::json!({
        "proposal": proposal,
        "staged_revision": staged.policy_revision,
        "active_revision": active_revision,
    }))
    .map_err(|source| CalibrationError::SerializeOutput { source })
}

/// Record explicit review and atomically activate an eligible staged revision.
///
/// # Errors
///
/// Returns [`CalibrationError::ProposalNotEligible`] when the frozen gate did
/// not pass, leaving the active policy unchanged.
pub fn activate_tuning_proposal(
    proposal_path: &Path,
    manifest_path: &Path,
    frozen_input_path: &Path,
    frozen_report_path: &Path,
    evidence_path: &Path,
    drift_path: &Path,
    active_policy_path: &Path,
    revisions_dir: Option<&Path>,
    reviewed_by: String,
    review_note: String,
) -> Result<String, CalibrationError> {
    validate_activation_paths(
        proposal_path,
        manifest_path,
        frozen_input_path,
        frozen_report_path,
        evidence_path,
        drift_path,
        active_policy_path,
    )?;
    let proposal: TuningProposal = read_tuning_json(proposal_path)?;
    let store = revision_store(active_policy_path, revisions_dir);
    let policy = store
        .inspect(&proposal.candidate_revision)
        .map_err(policy_error)?;
    if sha256(&read_bytes(&store_path(
        active_policy_path,
        revisions_dir,
        &proposal.candidate_revision,
    ))?) != proposal.candidate_policy_sha256
    {
        return Err(CalibrationError::ProposalNotEligible);
    }
    let manifest: Manifest = read_json(manifest_path, DocumentKind::Manifest)?;
    let frozen: FrozenInput = read_json(frozen_input_path, DocumentKind::Input)?;
    let report: FrozenBenchmarkReport = read_tuning_json(frozen_report_path)?;
    let evidence: TuningEvidence = read_tuning_json(evidence_path)?;
    let drift: DriftSamples = read_tuning_json(drift_path)?;
    validate_tuning_inputs(&manifest, &frozen, &report, &evidence, &drift, &policy)?;
    let frozen_sha256 = sha256(&read_bytes(frozen_input_path)?);
    let recomputed_eligible = report.passed
        && report.manifest_id == manifest.manifest_id
        && report.frozen_input_sha256 == frozen_sha256
        && evaluate_frozen_policy(&manifest, &frozen, &policy).passed
        && !drift.samples.is_empty();
    if !recomputed_eligible {
        return Err(CalibrationError::ProposalNotEligible);
    }
    store
        .record_review(
            &proposal.candidate_revision,
            &ModelRouterPolicyReview {
                reviewed_by,
                review_note,
                calibration_gate_passed: true,
            },
        )
        .map_err(policy_error)?;
    store
        .activate_reviewed(&proposal.candidate_revision)
        .map_err(policy_error)?;
    serde_json::to_string(&serde_json::json!({
        "activated_revision": policy.policy_revision,
        "activation": "reviewed",
    }))
    .map_err(|source| CalibrationError::SerializeOutput { source })
}

/// Explicitly and atomically restore a reviewed policy revision.
///
/// # Errors
///
/// Returns [`CalibrationError`] when the revision was not staged and reviewed.
pub fn rollback_tuning_policy(
    active_policy_path: &Path,
    revisions_dir: Option<&Path>,
    revision: &str,
) -> Result<String, CalibrationError> {
    let store = revision_store(active_policy_path, revisions_dir);
    let policy = store.rollback_reviewed(revision).map_err(policy_error)?;
    serde_json::to_string(&serde_json::json!({
        "rolled_back_to_revision": policy.policy_revision,
        "activation": "reviewed_rollback",
    }))
    .map_err(|source| CalibrationError::SerializeOutput { source })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenBenchmarkReport {
    manifest_id: String,
    frozen_input_sha256: String,
    passed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TuningEvidence {
    schema_version: u32,
    observations: Vec<Observation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    class_id: String,
    ab_preference: AbPreference,
    observed_cost_microusd: Option<u64>,
    latency_ms: Option<u32>,
    confidence_decile: u8,
    fallback: bool,
    override_applied: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AbPreference {
    Routed,
    Orchestrator,
    Tie,
    Unusable,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DriftSamples {
    schema_version: u32,
    samples: Vec<DriftSample>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DriftSample {
    class_id: String,
    confidence_decile: u8,
    fallback: bool,
    override_applied: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActivationState {
    ReviewRequired,
    BlockedByCalibrationGate,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TuningProposal {
    schema_version: u32,
    candidate_revision: String,
    candidate_policy_sha256: String,
    frozen_benchmark: FrozenBenchmarkComparison,
    aggregate: AggregateSummary,
    drift: DriftSummary,
    eligible_for_review: bool,
    activation: ActivationState,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenBenchmarkComparison {
    source_report_passed: bool,
    heldout_gate_passed: bool,
    metrics: FrozenMetrics,
    gates: FrozenGates,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenMetrics {
    macro_f1: f64,
    top_two_recall: f64,
    abstention_coverage: f64,
    cost_weighted_under_routing_penalty: f64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenGates {
    macro_f1: bool,
    top_two_recall: bool,
    abstention_coverage: bool,
    per_class_recall: bool,
    cost_weighted_under_routing_penalty: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateSummary {
    observation_count: usize,
    ab_preferences: BTreeMap<String, u64>,
    known_cost_microusd: u64,
    unknown_cost_count: u64,
    known_latency_count: u64,
    p95_latency_ms: Option<u32>,
    confidence_deciles: [u64; 10],
    fallback_count: u64,
    override_count: u64,
    class_counts: BTreeMap<String, u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriftSummary {
    sample_count: usize,
    confidence_deciles: [u64; 10],
    fallback_count: u64,
    override_count: u64,
    class_counts: BTreeMap<String, u64>,
    heldout_total_variation_distance: f64,
}

struct FrozenEvaluation {
    metrics: FrozenMetrics,
    gates: FrozenGates,
    passed: bool,
}

fn revision_store(
    active_policy_path: &Path,
    revisions_dir: Option<&Path>,
) -> ModelRouterPolicyRevisionStore {
    revisions_dir.map_or_else(
        || ModelRouterPolicyRevisionStore::new(active_policy_path),
        |revisions_dir| {
            ModelRouterPolicyRevisionStore::with_revisions_dir(active_policy_path, revisions_dir)
        },
    )
}

fn validate_proposal_paths(
    manifest_path: &Path,
    frozen_input_path: &Path,
    frozen_report_path: &Path,
    evidence_path: &Path,
    drift_path: &Path,
    candidate_policy_path: &Path,
    active_policy_path: &Path,
    proposal_path: &Path,
) -> Result<(), CalibrationError> {
    let sources = [
        canonical_existing(manifest_path)?,
        canonical_existing(frozen_input_path)?,
        canonical_existing(frozen_report_path)?,
        canonical_existing(evidence_path)?,
        canonical_existing(drift_path)?,
        canonical_existing(candidate_policy_path)?,
        canonical_output(active_policy_path)?,
    ];
    let proposal = canonical_output(proposal_path)?;
    let distinct_sources = sources.iter().collect::<std::collections::BTreeSet<_>>();
    (distinct_sources.len() == sources.len() && !distinct_sources.contains(&proposal))
        .then_some(())
        .ok_or(CalibrationError::ConflictingPaths)
}

fn validate_activation_paths(
    proposal_path: &Path,
    manifest_path: &Path,
    frozen_input_path: &Path,
    frozen_report_path: &Path,
    evidence_path: &Path,
    drift_path: &Path,
    active_policy_path: &Path,
) -> Result<(), CalibrationError> {
    let paths = [
        canonical_existing(proposal_path)?,
        canonical_existing(manifest_path)?,
        canonical_existing(frozen_input_path)?,
        canonical_existing(frozen_report_path)?,
        canonical_existing(evidence_path)?,
        canonical_existing(drift_path)?,
        canonical_output(active_policy_path)?,
    ];
    (paths
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == paths.len())
    .then_some(())
    .ok_or(CalibrationError::ConflictingPaths)
}

fn canonical_existing(path: &Path) -> Result<std::path::PathBuf, CalibrationError> {
    fs::canonicalize(path).map_err(|source| CalibrationError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn canonical_output(path: &Path) -> Result<std::path::PathBuf, CalibrationError> {
    let parent = path.parent().ok_or(CalibrationError::ConflictingPaths)?;
    let filename = path.file_name().ok_or(CalibrationError::ConflictingPaths)?;
    fs::canonicalize(parent)
        .map(|parent| parent.join(filename))
        .map_err(|source| CalibrationError::ReadFile {
            path: parent.to_path_buf(),
            source,
        })
}

fn store_path(
    active_policy_path: &Path,
    revisions_dir: Option<&Path>,
    revision: &str,
) -> std::path::PathBuf {
    let revisions_dir = revisions_dir.map_or_else(
        || {
            active_policy_path.parent().map_or_else(
                || std::path::PathBuf::from("model-router-revisions"),
                |parent| parent.join("model-router-revisions"),
            )
        },
        Path::to_path_buf,
    );
    revisions_dir.join(format!("{revision}.toml"))
}

fn validate_tuning_inputs(
    manifest: &Manifest,
    frozen: &FrozenInput,
    report: &FrozenBenchmarkReport,
    evidence: &TuningEvidence,
    drift: &DriftSamples,
    candidate: &ModelRouterPolicy,
) -> Result<(), CalibrationError> {
    let class_ids = candidate
        .classes
        .iter()
        .map(|class| class.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let manifest_class_ids = manifest
        .taxonomy
        .iter()
        .map(|class| class.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    (report.manifest_id == manifest.manifest_id
        && evidence.schema_version == TUNING_SCHEMA_VERSION
        && drift.schema_version == TUNING_SCHEMA_VERSION
        && evidence.observations.len() <= MAX_OBSERVATIONS
        && drift.samples.len() <= MAX_DRIFT_SAMPLES
        && class_ids == manifest_class_ids
        && candidate.embedding.dimensions == manifest.artifact.embedding_dimensions
        && candidate.embedding.revision == manifest.artifact.identity
        && candidate.embedding.artifact_sha256 == manifest.artifact.sha256
        && candidate.classes.iter().all(|class| {
            class
                .weights
                .as_ref()
                .is_some_and(|weights| weights.len() == manifest.artifact.embedding_dimensions)
        })
        && frozen.records.iter().all(|record| {
            manifest_class_ids.contains(record.expected_class.as_str())
                && record.embedding.len() == manifest.artifact.embedding_dimensions
        })
        && evidence.observations.iter().all(|observation| {
            class_ids.contains(observation.class_id.as_str()) && observation.confidence_decile < 10
        })
        && drift.samples.iter().all(|sample| {
            class_ids.contains(sample.class_id.as_str()) && sample.confidence_decile < 10
        }))
    .then_some(())
    .ok_or(CalibrationError::InvalidTuningInput)
}

fn evaluate_frozen_policy(
    manifest: &Manifest,
    frozen: &FrozenInput,
    policy: &ModelRouterPolicy,
) -> FrozenEvaluation {
    let heldout = frozen
        .records
        .iter()
        .filter(|record| matches!(record.split, Split::Heldout))
        .collect::<Vec<_>>();
    let totals = heldout.iter().fold(
        manifest
            .taxonomy
            .iter()
            .map(|class| (class.id.as_str(), (0_u64, 0_u64, 0_u64)))
            .collect::<BTreeMap<_, _>>(),
        |mut totals, record| {
            let output = classify(record, policy);
            let actual = totals
                .get_mut(record.expected_class.as_str())
                .unwrap_or_else(|| unreachable!());
            actual.0 += 1;
            if let Some(predicted) = output.predicted {
                if predicted == record.expected_class {
                    actual.1 += 1;
                    totals
                        .get_mut(predicted)
                        .unwrap_or_else(|| unreachable!())
                        .2 += 1;
                } else {
                    totals
                        .get_mut(predicted)
                        .unwrap_or_else(|| unreachable!())
                        .2 += 1;
                }
            }
            totals
        },
    );
    let outputs = heldout
        .iter()
        .map(|record| (record, classify(record, policy)))
        .collect::<Vec<_>>();
    let count = heldout.len() as u64;
    let top_two_recall = outputs
        .iter()
        .filter(|(record, output)| {
            output
                .top_two
                .iter()
                .any(|class| *class == record.expected_class)
        })
        .count() as u64;
    let accepted = outputs
        .iter()
        .filter(|(_, output)| output.predicted.is_some())
        .count() as u64;
    let under_routing_penalty = outputs
        .iter()
        .filter_map(|(record, output)| {
            output.predicted.and_then(|predicted| {
                (predicted != record.expected_class
                    && strong_floor(manifest, &record.expected_class)
                    && !strong_floor(manifest, predicted))
                .then(|| penalty(manifest, &record.expected_class))
            })
        })
        .sum::<f64>();
    let per_class_recall = totals
        .iter()
        .map(|(class, (actual, correct, _))| (*class, ratio(*correct, *actual)))
        .collect::<BTreeMap<_, _>>();
    let metrics = FrozenMetrics {
        macro_f1: totals
            .values()
            .map(|(actual, correct, predicted)| ratio(2 * *correct, *actual + *predicted))
            .sum::<f64>()
            / manifest.taxonomy.len() as f64,
        top_two_recall: ratio(top_two_recall, count),
        abstention_coverage: ratio(accepted, count),
        cost_weighted_under_routing_penalty: under_routing_penalty / count.max(1) as f64,
    };
    let gates = FrozenGates {
        macro_f1: metrics.macro_f1 >= manifest.gates.min_macro_f1,
        top_two_recall: metrics.top_two_recall >= manifest.gates.min_top_two_recall,
        abstention_coverage: metrics.abstention_coverage >= manifest.gates.min_abstention_coverage,
        per_class_recall: manifest
            .gates
            .min_per_class_recall
            .iter()
            .all(|(class, gate)| {
                per_class_recall
                    .get(class.as_str())
                    .is_some_and(|value| value >= gate)
            }),
        cost_weighted_under_routing_penalty: metrics.cost_weighted_under_routing_penalty
            <= manifest.gates.max_cost_weighted_under_routing_penalty,
    };
    let passed = gates.macro_f1
        && gates.top_two_recall
        && gates.abstention_coverage
        && gates.per_class_recall
        && gates.cost_weighted_under_routing_penalty;
    FrozenEvaluation {
        metrics,
        gates,
        passed,
    }
}

struct ClassifierOutput<'a> {
    predicted: Option<&'a str>,
    top_two: Vec<&'a str>,
}

fn classify<'a>(record: &Record, policy: &'a ModelRouterPolicy) -> ClassifierOutput<'a> {
    let norm = record
        .embedding
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    let mut scores = if norm.is_finite() && norm > 0.0 {
        {
            policy
                .classes
                .iter()
                .filter_map(|class| {
                    class.weights.as_ref().map(|weights| {
                        let score = norm.recip()
                            * weights
                                .iter()
                                .zip(&record.embedding)
                                .map(|(weight, value)| weight * value)
                                .sum::<f32>();
                        (class.id.as_str(), score, class)
                    })
                })
                .collect::<Vec<_>>()
        }
    } else {
        Default::default()
    };
    scores.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let Some((class_id, score, class)) = scores.first().copied() else {
        return ClassifierOutput {
            predicted: None,
            top_two: Vec::new(),
        };
    };
    let margin = score - scores.get(1).map_or(0.0, |(_, next, _)| *next);
    let accepted = score
        >= class
            .minimum_score
            .unwrap_or(policy.classifier.minimum_score)
        && margin
            >= class
                .minimum_margin
                .unwrap_or(policy.classifier.minimum_margin);
    ClassifierOutput {
        predicted: accepted.then_some(class_id),
        top_two: if accepted {
            scores.iter().take(2).map(|(id, _, _)| *id).collect()
        } else {
            Default::default()
        },
    }
}

fn summarize_evidence(evidence: &TuningEvidence) -> AggregateSummary {
    let mut latencies = evidence
        .observations
        .iter()
        .filter_map(|observation| observation.latency_ms)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    let mut summary = evidence.observations.iter().fold(
        AggregateSummary {
            observation_count: evidence.observations.len(),
            ab_preferences: BTreeMap::new(),
            known_cost_microusd: 0,
            unknown_cost_count: 0,
            known_latency_count: latencies.len() as u64,
            p95_latency_ms: None,
            confidence_deciles: [0; 10],
            fallback_count: 0,
            override_count: 0,
            class_counts: BTreeMap::new(),
        },
        |mut summary, observation| {
            *summary
                .ab_preferences
                .entry(format!("{:?}", observation.ab_preference).to_lowercase())
                .or_default() += 1;
            *summary
                .class_counts
                .entry(observation.class_id.clone())
                .or_default() += 1;
            summary.confidence_deciles[usize::from(observation.confidence_decile)] += 1;
            summary.fallback_count += u64::from(observation.fallback);
            summary.override_count += u64::from(observation.override_applied);
            match observation.observed_cost_microusd {
                Some(cost) => {
                    summary.known_cost_microusd = summary.known_cost_microusd.saturating_add(cost)
                }
                None => summary.unknown_cost_count += 1,
            }
            summary
        },
    );
    summary.p95_latency_ms = latencies
        .get((latencies.len() * 95).saturating_sub(1) / 100)
        .copied();
    summary
}

fn summarize_drift(drift: &DriftSamples, frozen: &FrozenInput) -> DriftSummary {
    let heldout_class_counts = frozen
        .records
        .iter()
        .filter(|record| matches!(record.split, Split::Heldout))
        .fold(BTreeMap::new(), |mut counts, record| {
            *counts
                .entry(record.expected_class.as_str())
                .or_insert(0_u64) += 1;
            counts
        });
    let mut summary = drift.samples.iter().fold(
        DriftSummary {
            sample_count: drift.samples.len(),
            confidence_deciles: [0; 10],
            fallback_count: 0,
            override_count: 0,
            class_counts: BTreeMap::new(),
            heldout_total_variation_distance: 0.0,
        },
        |mut summary, sample| {
            summary.confidence_deciles[usize::from(sample.confidence_decile)] += 1;
            summary.fallback_count += u64::from(sample.fallback);
            summary.override_count += u64::from(sample.override_applied);
            *summary
                .class_counts
                .entry(sample.class_id.clone())
                .or_default() += 1;
            summary
        },
    );
    let heldout_count = heldout_class_counts.values().sum::<u64>();
    let drift_count = drift.samples.len() as u64;
    summary.heldout_total_variation_distance = summary
        .class_counts
        .keys()
        .cloned()
        .chain(
            heldout_class_counts
                .keys()
                .map(|class| (*class).to_string()),
        )
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|class| {
            let drift_share = ratio(*summary.class_counts.get(&class).unwrap_or(&0), drift_count);
            let heldout_share = ratio(
                *heldout_class_counts.get(class.as_str()).unwrap_or(&0),
                heldout_count,
            );
            (drift_share - heldout_share).abs()
        })
        .sum::<f64>()
        / 2.0;
    summary
}

fn strong_floor(manifest: &Manifest, id: &str) -> bool {
    manifest
        .taxonomy
        .iter()
        .find(|class| class.id == id)
        .is_some_and(|class| class.strong_model_floor)
}

fn penalty(manifest: &Manifest, id: &str) -> f64 {
    manifest
        .taxonomy
        .iter()
        .find(|class| class.id == id)
        .map_or(0.0, |class| class.under_routing_penalty)
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    (denominator > 0)
        .then_some(numerator as f64 / denominator as f64)
        .unwrap_or(0.0)
}

fn read_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    kind: DocumentKind,
) -> Result<T, CalibrationError> {
    serde_json::from_slice(&read_bytes(path)?)
        .map_err(|source| CalibrationError::ParseJson { kind, source })
}

fn read_tuning_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CalibrationError> {
    serde_json::from_slice(&read_bytes(path)?).map_err(|_| CalibrationError::InvalidTuningInput)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, CalibrationError> {
    fs::read(path).map_err(|source| CalibrationError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), CalibrationError> {
    let contents = serde_json::to_vec_pretty(value)
        .map_err(|source| CalibrationError::SerializeOutput { source })?;
    let contents =
        std::str::from_utf8(&contents).map_err(|_| CalibrationError::InvalidTuningInput)?;
    let write_path =
        resolve_symlink_write_paths(path).map_err(|source| CalibrationError::WriteFile {
            path: path.to_path_buf(),
            source,
        })?;
    write_atomically(&write_path.write_path, contents).map_err(|source| {
        CalibrationError::WriteFile {
            path: write_path.write_path,
            source,
        }
    })
}

fn sha256(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

fn policy_error(source: xedoc_config::ModelRouterPolicyError) -> CalibrationError {
    CalibrationError::Policy { source }
}
