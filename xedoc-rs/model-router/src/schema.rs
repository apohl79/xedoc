//! Private schemas for raw local corpus, frozen input, and aggregate output.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) schema_version: u32,
    pub(super) manifest_id: String,
    pub(super) corpus_cutoff_unix_seconds: i64,
    pub(super) split_algorithm: SplitAlgorithm,
    pub(super) random_seed: u64,
    pub(super) taxonomy_revision: String,
    pub(super) taxonomy: Vec<TaxonomyClass>,
    pub(super) artifact: Artifact,
    pub(super) heldout_fraction: f64,
    pub(super) classifier: ClassifierThresholds,
    pub(super) gates: QualityGates,
    #[serde(default)]
    pub(super) observed_p95_latency_ms: Option<u32>,
    #[serde(default)]
    pub(super) observed_peak_rss_kib: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SplitAlgorithm {
    ChronologicalHeldOutV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaxonomyClass {
    pub(super) id: String,
    pub(super) under_routing_penalty: f64,
    pub(super) strong_model_floor: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    pub(super) identity: String,
    pub(super) sha256: String,
    pub(super) bundle_sha256: String,
    pub(super) embedding_dimensions: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClassifierThresholds {
    pub(super) min_score: f64,
    pub(super) min_margin: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QualityGates {
    pub(super) min_macro_f1: f64,
    pub(super) min_top_two_recall: f64,
    pub(super) min_abstention_coverage: f64,
    pub(super) min_per_class_recall: BTreeMap<String, f64>,
    pub(super) max_cost_weighted_under_routing_penalty: f64,
    pub(super) max_p95_latency_ms: u32,
    pub(super) max_peak_rss_kib: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LocalCorpus {
    pub(super) records: Vec<RawRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRecord {
    pub(super) source_id: String,
    pub(super) recorded_at_unix_seconds: i64,
    pub(super) label: String,
    pub(super) prompt: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FrozenInput {
    pub(super) records: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub(super) source_id: String,
    pub(super) recorded_at_unix_seconds: i64,
    pub(super) prompt_hash: String,
    pub(super) structural: StructuralMetadata,
    pub(super) embedding: Vec<f32>,
    pub(super) expected_class: String,
    pub(super) split: Split,
    pub(super) classifier_output: Option<ClassifierOutput>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StructuralMetadata {
    pub(super) original_bytes: u32,
    pub(super) normalized_bytes: u32,
    pub(super) line_count: u16,
    pub(super) attachment_count: u16,
    pub(super) truncated: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClassifierOutput {
    pub(super) predicted_class: Option<String>,
    pub(super) top_two_classes: Vec<String>,
    pub(super) confidence: f64,
    pub(super) margin: f64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Split {
    Train,
    Heldout,
}

#[derive(Serialize)]
pub(super) struct CalibrationReport<'a> {
    pub(super) manifest_id: &'a str,
    pub(super) corpus_cutoff_unix_seconds: i64,
    pub(super) split_algorithm: &'a SplitAlgorithm,
    pub(super) random_seed: u64,
    pub(super) taxonomy_revision: &'a str,
    pub(super) artifact: ArtifactReport<'a>,
    pub(super) record_count: usize,
    pub(super) train_record_count: usize,
    pub(super) heldout_record_count: usize,
    pub(super) frozen_input_sha256: String,
    pub(super) metrics: Metrics,
    pub(super) gates: GateResults,
    pub(super) passed: bool,
}

#[derive(Serialize)]
pub(super) struct ArtifactReport<'a> {
    pub(super) identity: &'a str,
    pub(super) sha256: &'a str,
    pub(super) bundle_sha256: &'a str,
    pub(super) embedding_dimensions: usize,
}

#[derive(Serialize)]
pub(super) struct Metrics {
    pub(super) macro_f1: f64,
    pub(super) top_two_recall: f64,
    pub(super) abstention_coverage: f64,
    pub(super) cost_weighted_under_routing_penalty: f64,
    pub(super) p95_latency_ms: u32,
    pub(super) peak_rss_kib: u64,
    pub(super) rss_observed: bool,
    pub(super) per_class_recall: BTreeMap<String, f64>,
}

#[derive(Serialize)]
pub(super) struct GateResults {
    pub(super) macro_f1: bool,
    pub(super) top_two_recall: bool,
    pub(super) abstention_coverage: bool,
    pub(super) per_class_recall: bool,
    pub(super) cost_weighted_under_routing_penalty: bool,
    pub(super) p95_latency_ms: bool,
    pub(super) peak_rss_kib: bool,
}

#[derive(Serialize)]
pub(super) struct Proposal<'a> {
    pub(super) proposal_schema_version: u32,
    pub(super) manifest_id: &'a str,
    pub(super) taxonomy_revision: &'a str,
    pub(super) artifact: ArtifactReport<'a>,
    pub(super) classifier: ClassifierProposal<'a>,
    pub(super) frozen_input_sha256: &'a str,
    pub(super) quality_gates_passed: bool,
    pub(super) activation: &'static str,
}

#[derive(Serialize)]
pub(super) struct ClassifierProposal<'a> {
    pub(super) algorithm_revision: &'static str,
    pub(super) min_score: f64,
    pub(super) min_margin: f64,
    pub(super) heads: BTreeMap<&'a str, &'a [f32]>,
}
