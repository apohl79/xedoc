//! Converts an explicitly reviewed local corpus into a four-axis router policy.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use sha2::Digest;
use sha2::Sha256;
use xedoc_config::ModelRouterAxis;
use xedoc_config::ModelRouterClass;
use xedoc_config::ModelRouterClassifier;
use xedoc_config::ModelRouterEmbedding;
use xedoc_config::ModelRouterModelClass;
use xedoc_config::ModelRouterPolicy;
use xedoc_config::ModelRouterRankedRoute;
use xedoc_config::ModelRouterRanking;
use xedoc_protocol::openai_models::ReasoningEffort;

use crate::CalibrationError;
use crate::FastEmbedder;
use crate::LocalArtifact;
use crate::SyncEmbedder;

const AXES: [&str; 4] = ["work_type", "complexity", "orchestration", "risk"];

/// Derive and write a schema-v2 policy from a reviewed Markdown corpus.
///
/// The resulting policy contains classifier heads and routing parameters, but
/// never copies prompt text into the output.
pub fn calibrate_review_policy(
    review_path: &Path,
    artifact_path: &Path,
    output_path: &Path,
) -> Result<(), CalibrationError> {
    let review = fs::read_to_string(review_path).map_err(|source| CalibrationError::ReadFile {
        path: review_path.to_path_buf(),
        source,
    })?;
    let artifact = LocalArtifact::load(artifact_path)
        .map_err(|source| CalibrationError::Artifact { source })?;
    let embedder = FastEmbedder::from_local_artifact(&artifact)
        .map_err(|source| CalibrationError::Embedding { source })?;
    let mut sums = BTreeMap::<(String, String), (u32, Vec<f32>)>::new();
    for record in parse_review(&review)? {
        let embedding = embedder
            .embed_sync(&record.prompt)
            .map_err(|source| CalibrationError::Embedding { source })?;
        for (axis, value) in record.labels {
            let entry = sums
                .entry((axis, value))
                .or_insert_with(|| (0, vec![0.0; embedding.len()]));
            entry
                .1
                .iter_mut()
                .zip(&embedding)
                .for_each(|(sum, value)| *sum += value);
            entry.0 += 1;
        }
    }
    // The reviewed corpus is authoritative where it has coverage. The three
    // newly introduced work types use explicit, versioned bootstrap examples
    // until a reviewer replaces them with labelled session prompts.
    for axis in AXES {
        for (id, _, _, _) in axis_definitions(axis) {
            let key = (axis.to_string(), id.to_string());
            if sums.contains_key(&key) {
                continue;
            }
            let prototype =
                classification_prototype(axis, id).ok_or(CalibrationError::InvalidCorpus)?;
            let embedding = embedder
                .embed_sync(prototype)
                .map_err(|source| CalibrationError::Embedding { source })?;
            sums.insert(key, (1, embedding));
        }
    }
    let axes = AXES
        .into_iter()
        .map(|axis| ModelRouterAxis {
            id: axis.to_string(),
            classes: axis_definitions(axis)
                .into_iter()
                .map(|(id, points, minimum_model_class, maximum_model_class)| {
                    let weights =
                        sums.get(&(axis.to_string(), id.to_string()))
                            .map(|(count, sum)| {
                                sum.iter()
                                    .map(|value| value / *count as f32)
                                    .collect::<Vec<_>>()
                            });
                    ModelRouterClass {
                        id: id.to_string(),
                        minimum_reasoning_effort: ReasoningEffort::Low,
                        required_capabilities: Vec::new(),
                        weights,
                        minimum_score: None,
                        minimum_margin: None,
                        points,
                        minimum_model_class,
                        maximum_model_class,
                    }
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    let parameters =
        serde_json::to_vec(&axes).map_err(|source| CalibrationError::SerializeOutput { source })?;
    let digest = format!("{:x}", Sha256::digest(parameters));
    let policy = ModelRouterPolicy {
        schema_version: 2,
        policy_revision: format!("review-calibration-{}", &digest[..12]),
        embedding: ModelRouterEmbedding {
            runtime: "fastembed".to_string(),
            model: "arctic-embed-xs".to_string(),
            revision: artifact.descriptor.revision,
            artifact_sha256: artifact.descriptor.sha256,
            dimensions: artifact.descriptor.dimensions,
        },
        classifier: ModelRouterClassifier {
            revision: format!("four-axis-{}", &digest[..12]),
            parameters_sha256: digest,
            minimum_score: 0.20,
            minimum_margin: 0.01,
        },
        capabilities: Vec::new(),
        classes: Vec::new(),
        axes,
        ranking: default_ranking(),
    };
    xedoc_config::write_model_router_policy(output_path, &policy)
        .map_err(|source| CalibrationError::Policy { source })?;
    Ok(())
}

fn axis_definitions(
    axis: &str,
) -> Vec<(
    &'static str,
    u16,
    ModelRouterModelClass,
    ModelRouterModelClass,
)> {
    use ModelRouterModelClass::Intelligent;
    use ModelRouterModelClass::Simple;
    use ModelRouterModelClass::Smart;

    match axis {
        "work_type" => vec![
            ("steering", 0, Simple, Simple),
            ("question", 1, Simple, Smart),
            ("docs_analysis", 1, Simple, Smart),
            ("packaging", 1, Simple, Smart),
            ("operational", 1, Simple, Smart),
            ("testing", 1, Simple, Smart),
            ("implementation", 3, Simple, Intelligent),
            ("bug_fix", 3, Simple, Intelligent),
            ("refactor", 3, Simple, Intelligent),
            ("docs_authoring", 3, Simple, Intelligent),
            ("orchestration", 3, Simple, Intelligent),
            ("calibration", 3, Simple, Intelligent),
            ("research", 6, Smart, Intelligent),
            ("review", 6, Smart, Intelligent),
            ("diagnosis", 6, Smart, Intelligent),
            ("design", 6, Smart, Intelligent),
        ],
        "complexity" => vec![
            ("low", 1, Simple, Smart),
            ("medium", 3, Simple, Intelligent),
            ("high", 6, Simple, Intelligent),
            ("very_high", 9, Smart, Intelligent),
        ],
        "orchestration" => vec![
            ("none", 0, Simple, Smart),
            ("delegate", 1, Simple, Smart),
            ("coordination", 1, Smart, Smart),
            ("workflow", 3, Smart, Smart),
        ],
        "risk" => vec![
            ("low", 1, Simple, Smart),
            ("medium", 3, Simple, Intelligent),
            ("high", 10, Smart, Intelligent),
        ],
        _ => Vec::new(),
    }
}

fn classification_prototype(axis: &str, id: &str) -> Option<&'static str> {
    match (axis, id) {
        ("work_type", "docs_analysis") => {
            Some("Analyze existing documentation and explain its implications.")
        }
        ("work_type", "docs_authoring") => {
            Some("Write and revise user-facing technical documentation.")
        }
        ("work_type", "calibration") => {
            Some("Calibrate and evaluate a classifier using labelled examples.")
        }
        _ => None,
    }
}

fn default_ranking() -> ModelRouterRanking {
    let models = [
        (ModelRouterModelClass::Simple, "gpt-5.6-luna"),
        (ModelRouterModelClass::Smart, "gpt-5.6-terra"),
        (ModelRouterModelClass::Intelligent, "gpt-5.6-sol"),
    ];
    let efforts = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Max,
    ];
    ModelRouterRanking {
        minimum_score: 3,
        maximum_score: 35,
        ladder: models
            .into_iter()
            .flat_map(|(class, model)| {
                efforts
                    .iter()
                    .cloned()
                    .map(move |reasoning_effort| (class, model, reasoning_effort))
            })
            .enumerate()
            .map(
                |(index, (class, model, reasoning_effort))| ModelRouterRankedRoute {
                    rank: u16::try_from(index + 1).unwrap_or(u16::MAX),
                    class,
                    provider: "openai".to_string(),
                    model: model.to_string(),
                    reasoning_effort,
                },
            )
            .collect(),
    }
}

struct ReviewRecord {
    prompt: String,
    labels: BTreeMap<String, String>,
}

fn parse_review(markdown: &str) -> Result<Vec<ReviewRecord>, CalibrationError> {
    markdown
        .split("\n### ")
        .skip(1)
        .map(parse_review_record)
        .collect()
}

fn parse_review_record(block: &str) -> Result<ReviewRecord, CalibrationError> {
    let mut labels = BTreeMap::new();
    for (axis, marker) in [
        ("work_type", "- Proposed work type: `"),
        ("complexity", "- Proposed complexity: `"),
        ("orchestration", "- Proposed orchestration: `"),
        ("risk", "- Proposed risk: `"),
    ] {
        let value = block
            .lines()
            .find_map(|line| line.strip_prefix(marker)?.strip_suffix('`'))
            .ok_or(CalibrationError::InvalidCorpus)?;
        let value = match (axis, value) {
            ("orchestration", "delegated") => "delegate",
            ("orchestration", "workflow_control") => "workflow",
            _ => value,
        };
        labels.insert(axis.to_string(), value.to_string());
    }
    let prompt = block
        .split("````")
        .nth(1)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or(CalibrationError::InvalidCorpus)?
        .to_string();
    Ok(ReviewRecord { prompt, labels })
}
