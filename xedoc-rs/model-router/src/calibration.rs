//! Bounded local extraction and deterministic held-out calibration.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use sha2::Digest;
use sha2::Sha256;

use crate::CalibrationError;
use crate::FastEmbedder;
use crate::LocalArtifact;
use crate::SyncEmbedder;
use crate::error::DocumentKind;
use crate::error::ManifestViolation;
use crate::schema::ArtifactReport;
use crate::schema::CalibrationReport;
use crate::schema::ClassifierOutput;
use crate::schema::ClassifierProposal;
use crate::schema::FrozenInput;
use crate::schema::GateResults;
use crate::schema::LocalCorpus;
use crate::schema::Manifest;
use crate::schema::Metrics;
use crate::schema::Proposal;
use crate::schema::RawRecord;
use crate::schema::Record;
use crate::schema::Split;
use crate::schema::StructuralMetadata;

const MAX_RECORDS: usize = 100;
const MAX_SOURCE_ID_BYTES: usize = 128;
const MAX_PROMPT_BYTES: usize = 16_384;
const MAX_LINES: u16 = 8_192;
const CLASSIFIER_ALGORITHM_REVISION: &str = "normalized_dot_product_raw_margin_v1";

pub(super) fn calibrate(
    manifest_path: &Path,
    corpus_path: &Path,
    artifact_path: &Path,
    frozen_input_path: &Path,
    report_path: &Path,
    proposal_path: &Path,
) -> Result<(), CalibrationError> {
    let paths = [
        manifest_path,
        corpus_path,
        artifact_path,
        frozen_input_path,
        report_path,
        proposal_path,
    ];
    if paths
        .iter()
        .enumerate()
        .any(|(index, path)| paths.iter().skip(index + 1).any(|other| path == other))
    {
        return Err(CalibrationError::ConflictingPaths);
    }
    let manifest: Manifest = read_json(manifest_path, DocumentKind::Manifest)?;
    validate_manifest(&manifest)?;
    let corpus = read_corpus(corpus_path)?;
    validate_corpus(&manifest, &corpus)?;
    let artifact = LocalArtifact::load(artifact_path)
        .map_err(|source| CalibrationError::Artifact { source })?;
    if artifact.descriptor.revision != manifest.artifact.identity
        || artifact.descriptor.sha256 != manifest.artifact.sha256
        || artifact.descriptor.bundle_sha256 != manifest.artifact.bundle_sha256
        || artifact.descriptor.dimensions != manifest.artifact.embedding_dimensions
    {
        return Err(CalibrationError::InvalidManifest {
            violation: ManifestViolation::InvalidArtifact,
        });
    }
    let embedder = FastEmbedder::from_local_artifact(&artifact)
        .map_err(|source| CalibrationError::Embedding { source })?;
    let mut records = freeze_corpus(&corpus, &embedder)?;
    assign_split(&mut records, &manifest);
    let heads = train_heads(&records, &manifest);
    score_heldout(&mut records, &manifest, &heads);
    let frozen = FrozenInput { records };
    let frozen_bytes = serialize(&frozen)?;
    let frozen_input_sha256 = format!("{:x}", Sha256::digest(&frozen_bytes));
    write_bytes(frozen_input_path, frozen_bytes)?;
    let (metrics, gates) = evaluate(&manifest, &frozen);
    let passed = all_gates_pass(&gates);
    let report = CalibrationReport {
        manifest_id: &manifest.manifest_id,
        corpus_cutoff_unix_seconds: manifest.corpus_cutoff_unix_seconds,
        split_algorithm: &manifest.split_algorithm,
        random_seed: manifest.random_seed,
        taxonomy_revision: &manifest.taxonomy_revision,
        artifact: artifact_report(&manifest),
        record_count: frozen.records.len(),
        train_record_count: frozen
            .records
            .iter()
            .filter(|record| matches!(record.split, Split::Train))
            .count(),
        heldout_record_count: frozen
            .records
            .iter()
            .filter(|record| matches!(record.split, Split::Heldout))
            .count(),
        frozen_input_sha256: frozen_input_sha256.clone(),
        metrics,
        gates,
        passed,
    };
    write_json(report_path, &report)?;
    let classifier = ClassifierProposal {
        algorithm_revision: CLASSIFIER_ALGORITHM_REVISION,
        min_score: manifest.classifier.min_score,
        min_margin: manifest.classifier.min_margin,
        heads: heads
            .iter()
            .map(|(class, head)| (*class, head.as_slice()))
            .collect(),
    };
    let proposal = Proposal {
        proposal_schema_version: 1,
        manifest_id: &manifest.manifest_id,
        taxonomy_revision: &manifest.taxonomy_revision,
        artifact: artifact_report(&manifest),
        classifier,
        frozen_input_sha256: &frozen_input_sha256,
        quality_gates_passed: passed,
        activation: "review_required",
    };
    write_json(proposal_path, &proposal)
}

fn artifact_report(manifest: &Manifest) -> ArtifactReport<'_> {
    ArtifactReport {
        identity: &manifest.artifact.identity,
        sha256: &manifest.artifact.sha256,
        bundle_sha256: &manifest.artifact.bundle_sha256,
        embedding_dimensions: manifest.artifact.embedding_dimensions,
    }
}

fn read_corpus(path: &Path) -> Result<LocalCorpus, CalibrationError> {
    let bytes = read_bytes(path)?;
    match serde_json::from_slice(&bytes) {
        Ok(corpus) => Ok(corpus),
        Err(_) => std::str::from_utf8(&bytes)
            .map_err(|_| CalibrationError::InvalidCorpus)
            .and_then(parse_markdown_corpus),
    }
}

fn parse_markdown_corpus(markdown: &str) -> Result<LocalCorpus, CalibrationError> {
    let chunks = markdown
        .strip_prefix("---\n")
        .ok_or(CalibrationError::InvalidCorpus)?
        .split("\n\n---\n");
    chunks
        .map(parse_markdown_record)
        .collect::<Result<Vec<_>, _>>()
        .map(|records| LocalCorpus { records })
}

fn parse_markdown_record(chunk: &str) -> Result<RawRecord, CalibrationError> {
    let (header, prompt) = chunk
        .split_once("\n---\n")
        .ok_or(CalibrationError::InvalidCorpus)?;
    let fields = header
        .lines()
        .map(|line| line.split_once(": ").ok_or(CalibrationError::InvalidCorpus))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if fields.len() != 3 {
        return Err(CalibrationError::InvalidCorpus);
    }
    Ok(RawRecord {
        source_id: fields
            .get("source_id")
            .ok_or(CalibrationError::InvalidCorpus)?
            .to_string(),
        recorded_at_unix_seconds: fields
            .get("recorded_at_unix_seconds")
            .ok_or(CalibrationError::InvalidCorpus)?
            .parse()
            .map_err(|_| CalibrationError::InvalidCorpus)?,
        label: fields
            .get("label")
            .ok_or(CalibrationError::InvalidCorpus)?
            .to_string(),
        prompt: prompt.to_string(),
    })
}

fn freeze_corpus(
    corpus: &LocalCorpus,
    embedder: &FastEmbedder,
) -> Result<Vec<Record>, CalibrationError> {
    corpus
        .records
        .iter()
        .map(|raw| {
            let normalized = normalize(&raw.prompt);
            let truncated = normalized.len() > MAX_PROMPT_BYTES;
            let bounded = bound_prompt(&normalized);
            let embedding = embedder
                .embed_sync(&bounded)
                .map_err(|source| CalibrationError::Embedding { source })?;
            Ok(Record {
                source_id: raw.source_id.clone(),
                recorded_at_unix_seconds: raw.recorded_at_unix_seconds,
                prompt_hash: format!("{:x}", Sha256::digest(bounded.as_bytes())),
                structural: StructuralMetadata {
                    original_bytes: raw.prompt.len().try_into().unwrap_or(u32::MAX),
                    normalized_bytes: bounded.len().try_into().unwrap_or(u32::MAX),
                    line_count: raw.prompt.lines().count().try_into().unwrap_or(u16::MAX),
                    attachment_count: 0,
                    truncated,
                },
                embedding,
                expected_class: raw.label.clone(),
                split: Split::Train,
                classifier_output: Some(ClassifierOutput {
                    predicted_class: None,
                    top_two_classes: Vec::new(),
                    confidence: 0.0,
                    margin: 0.0,
                }),
            })
        })
        .collect()
}

fn assign_split(records: &mut [Record], manifest: &Manifest) {
    records.sort_by(|left, right| {
        left.recorded_at_unix_seconds
            .cmp(&right.recorded_at_unix_seconds)
            .then_with(|| {
                seeded_key(&left.source_id, manifest.random_seed)
                    .cmp(&seeded_key(&right.source_id, manifest.random_seed))
            })
    });
    let heldout = ((records.len() as f64 * manifest.heldout_fraction).ceil() as usize)
        .clamp(1, records.len().saturating_sub(1));
    let split_at = records.len() - heldout;
    records.iter_mut().enumerate().for_each(|(index, record)| {
        record.split = if index < split_at {
            Split::Train
        } else {
            Split::Heldout
        };
    });
}

fn train_heads<'a>(records: &[Record], manifest: &'a Manifest) -> BTreeMap<&'a str, Vec<f32>> {
    manifest
        .taxonomy
        .iter()
        .filter_map(|class| {
            let training = records.iter().filter(|record| {
                matches!(record.split, Split::Train) && record.expected_class == class.id
            });
            let (count, sum) = training.fold(
                (0_u32, vec![0.0; manifest.artifact.embedding_dimensions]),
                |(count, mut sum), record| {
                    sum.iter_mut()
                        .zip(&record.embedding)
                        .for_each(|(value, embedding)| *value += embedding);
                    (count + 1, sum)
                },
            );
            (count > 0)
                .then(|| {
                    sum.into_iter()
                        .map(|value| value / count as f32)
                        .collect::<Vec<_>>()
                })
                .map(|head| (class.id.as_str(), head))
        })
        .collect()
}

fn score_heldout(records: &mut [Record], manifest: &Manifest, heads: &BTreeMap<&str, Vec<f32>>) {
    records
        .iter_mut()
        .filter(|record| matches!(record.split, Split::Heldout))
        .for_each(|record| {
            let norm = record
                .embedding
                .iter()
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt();
            let mut scores: Vec<(&str, f32)> = if norm == 0.0 {
                Vec::new()
            } else {
                heads
                    .iter()
                    .map(|(class, head)| {
                        (
                            *class,
                            norm.recip()
                                * head
                                    .iter()
                                    .zip(&record.embedding)
                                    .map(|(weight, value)| weight * value)
                                    .sum::<f32>(),
                        )
                    })
                    .collect()
            };
            scores.sort_by(|left, right| {
                right
                    .1
                    .partial_cmp(&left.1)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| left.0.cmp(right.0))
            });
            let best = scores.first().copied();
            let second = scores.get(1).copied();
            let confidence = best.map_or(0.0, |(_, score)| f64::from(score));
            let margin = best.map_or(0.0, |(_, score)| {
                f64::from(score - second.map_or(0.0, |(_, next)| next))
            });
            let accepted = confidence >= manifest.classifier.min_score
                && margin >= manifest.classifier.min_margin;
            record.classifier_output = Some(ClassifierOutput {
                predicted_class: accepted
                    .then(|| best.map(|(class, _)| class.to_string()))
                    .flatten(),
                top_two_classes: if accepted {
                    scores
                        .iter()
                        .take(2)
                        .map(|(class, _)| (*class).to_string())
                        .collect()
                } else {
                    Vec::new()
                },
                confidence,
                margin,
            });
        });
}

fn evaluate(manifest: &Manifest, frozen: &FrozenInput) -> (Metrics, GateResults) {
    let heldout = frozen
        .records
        .iter()
        .filter(|record| matches!(record.split, Split::Heldout));
    let mut totals: BTreeMap<&str, (u64, u64, u64)> = manifest
        .taxonomy
        .iter()
        .map(|class| (class.id.as_str(), (0, 0, 0)))
        .collect();
    let mut top_two_hits = 0_u64;
    let mut accepted = 0_u64;
    let mut under_routing_penalty = 0.0;
    let mut count = 0_u64;
    for record in heldout {
        count += 1;
        let output = record.classifier_output.as_ref();
        let expected = record.expected_class.as_str();
        let total = totals.get_mut(expected).unwrap_or_else(|| unreachable!());
        total.0 += 1;
        if output.is_some_and(|output| output.top_two_classes.iter().any(|class| class == expected))
        {
            top_two_hits += 1;
        }
        if let Some(Some(predicted)) = output.map(|output| output.predicted_class.as_ref()) {
            accepted += 1;
            if predicted == expected {
                total.1 += 1;
                totals
                    .get_mut(predicted.as_str())
                    .unwrap_or_else(|| unreachable!())
                    .2 += 1;
            } else {
                totals
                    .get_mut(predicted.as_str())
                    .unwrap_or_else(|| unreachable!())
                    .2 += 1;
                if strong_floor(manifest, expected) && !strong_floor(manifest, predicted) {
                    under_routing_penalty += penalty(manifest, expected);
                }
            }
        }
    }
    let per_class_recall = totals
        .iter()
        .map(|(class, (actual, true_positive, _))| {
            ((*class).to_string(), ratio(*true_positive, *actual))
        })
        .collect();
    let metrics = Metrics {
        macro_f1: totals
            .values()
            .map(|(actual, true_positive, predicted)| {
                ratio(2 * *true_positive, *actual + *predicted)
            })
            .sum::<f64>()
            / manifest.taxonomy.len() as f64,
        top_two_recall: ratio(top_two_hits, count),
        abstention_coverage: ratio(accepted, count),
        cost_weighted_under_routing_penalty: under_routing_penalty / count as f64,
        p95_latency_ms: manifest.observed_p95_latency_ms.unwrap_or_default(),
        peak_rss_kib: manifest.observed_peak_rss_kib.unwrap_or_default(),
        rss_observed: manifest.observed_peak_rss_kib.is_some(),
        per_class_recall,
    };
    let gates = GateResults {
        macro_f1: metrics.macro_f1 >= manifest.gates.min_macro_f1,
        top_two_recall: metrics.top_two_recall >= manifest.gates.min_top_two_recall,
        abstention_coverage: metrics.abstention_coverage >= manifest.gates.min_abstention_coverage,
        per_class_recall: manifest
            .gates
            .min_per_class_recall
            .iter()
            .all(|(class, gate)| {
                metrics
                    .per_class_recall
                    .get(class)
                    .is_some_and(|actual| actual >= gate)
            }),
        cost_weighted_under_routing_penalty: metrics.cost_weighted_under_routing_penalty
            <= manifest.gates.max_cost_weighted_under_routing_penalty,
        p95_latency_ms: manifest.observed_p95_latency_ms.is_some()
            && metrics.p95_latency_ms <= manifest.gates.max_p95_latency_ms,
        peak_rss_kib: metrics.rss_observed
            && metrics.peak_rss_kib <= manifest.gates.max_peak_rss_kib,
    };
    (metrics, gates)
}

fn validate_manifest(manifest: &Manifest) -> Result<(), CalibrationError> {
    let labels: BTreeSet<&str> = manifest
        .taxonomy
        .iter()
        .map(|class| class.id.as_str())
        .collect();
    if manifest.schema_version != 1 || manifest.corpus_cutoff_unix_seconds <= 0 {
        return Err(CalibrationError::InvalidManifest {
            violation: ManifestViolation::UnsupportedSchemaVersion,
        });
    }
    if manifest.manifest_id.is_empty() || manifest.taxonomy_revision.is_empty() {
        return Err(CalibrationError::InvalidManifest {
            violation: ManifestViolation::MissingIdentity,
        });
    }
    if labels.is_empty()
        || labels.len() != manifest.taxonomy.len()
        || labels.iter().any(|label| label.is_empty())
    {
        return Err(CalibrationError::InvalidManifest {
            violation: ManifestViolation::InvalidTaxonomy,
        });
    }
    if manifest.artifact.identity.is_empty()
        || !is_sha256(&manifest.artifact.sha256)
        || !is_sha256(&manifest.artifact.bundle_sha256)
        || manifest.artifact.embedding_dimensions == 0
    {
        return Err(CalibrationError::InvalidManifest {
            violation: ManifestViolation::InvalidArtifact,
        });
    }
    let gates = &manifest.gates;
    if !unit_interval(manifest.heldout_fraction)
        || manifest.heldout_fraction == 0.0
        || manifest.heldout_fraction == 1.0
        || !manifest.classifier.min_score.is_finite()
        || !manifest.classifier.min_margin.is_finite()
        || manifest.classifier.min_margin < 0.0
        || !unit_interval(gates.min_macro_f1)
        || !unit_interval(gates.min_top_two_recall)
        || !unit_interval(gates.min_abstention_coverage)
        || !gates.max_cost_weighted_under_routing_penalty.is_finite()
        || gates.max_cost_weighted_under_routing_penalty < 0.0
        || gates
            .min_per_class_recall
            .iter()
            .any(|(class, gate)| !labels.contains(class.as_str()) || !unit_interval(*gate))
        || manifest.taxonomy.iter().any(|class| {
            !class.under_routing_penalty.is_finite() || class.under_routing_penalty < 0.0
        })
        || (manifest.gates.min_per_class_recall.len() != labels.len())
        || labels
            .iter()
            .any(|label| !manifest.gates.min_per_class_recall.contains_key(*label))
        || manifest
            .observed_p95_latency_ms
            .is_some_and(|value| value == 0)
        || manifest
            .observed_peak_rss_kib
            .is_some_and(|value| value == 0)
    {
        return Err(CalibrationError::InvalidManifest {
            violation: ManifestViolation::InvalidQualityGate,
        });
    }
    Ok(())
}

fn validate_corpus(manifest: &Manifest, corpus: &LocalCorpus) -> Result<(), CalibrationError> {
    if corpus.records.len() < 2 || corpus.records.len() > MAX_RECORDS {
        return Err(CalibrationError::InvalidCorpus);
    }
    corpus.records.iter().try_for_each(|record| {
        (valid_source_id(&record.source_id)
            && record.recorded_at_unix_seconds > 0
            && record.recorded_at_unix_seconds <= manifest.corpus_cutoff_unix_seconds
            && is_class(manifest, &record.label)
            && !record.prompt.is_empty()
            && record.prompt.len() <= MAX_PROMPT_BYTES
            && record.prompt.lines().count() <= usize::from(MAX_LINES))
        .then_some(())
        .ok_or(CalibrationError::InvalidCorpus)
    })
}

fn serialize<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, CalibrationError> {
    serde_json::to_vec_pretty(value).map_err(|source| CalibrationError::SerializeOutput { source })
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), CalibrationError> {
    write_bytes(path, serialize(value)?)
}

fn write_bytes(path: &Path, output: Vec<u8>) -> Result<(), CalibrationError> {
    fs::write(path, output).map_err(|source| CalibrationError::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

fn read_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    kind: DocumentKind,
) -> Result<T, CalibrationError> {
    serde_json::from_slice(&read_bytes(path)?)
        .map_err(|source| CalibrationError::ParseJson { kind, source })
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, CalibrationError> {
    fs::read(path).map_err(|source| CalibrationError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn normalize(prompt: &str) -> String {
    prompt.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bound_prompt(prompt: &str) -> String {
    let end = prompt
        .char_indices()
        .take_while(|(index, character)| *index + character.len_utf8() <= MAX_PROMPT_BYTES)
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or_default();
    prompt[..end].to_string()
}

fn seeded_key(source_id: &str, seed: u64) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{seed}:{source_id}").as_bytes())
    )
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    (denominator > 0)
        .then_some(numerator as f64 / denominator as f64)
        .unwrap_or(0.0)
}

fn all_gates_pass(gates: &GateResults) -> bool {
    gates.macro_f1
        && gates.top_two_recall
        && gates.abstention_coverage
        && gates.per_class_recall
        && gates.cost_weighted_under_routing_penalty
        && gates.p95_latency_ms
        && gates.peak_rss_kib
}

fn is_class(manifest: &Manifest, id: &str) -> bool {
    manifest.taxonomy.iter().any(|class| class.id == id)
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

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SOURCE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn unit_interval(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
