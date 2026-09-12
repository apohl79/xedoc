//! Deterministic route policy and decision types.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::RouteScope;
use crate::RouterMode;
use crate::task::TaskEnvelope;
use crate::task::normalize_task;
use crate::worker::TaskEmbedder;

/// Closed reasoning effort values accepted by the router.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    ExtraHigh,
    Max,
}

/// Ordered quality bands used by the policy ranking ladder.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ModelClass {
    Simple,
    Smart,
    Intelligent,
}

/// A provider/model/effort tuple selected for a task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelRoute {
    pub provider_id: String,
    pub model_slug: String,
    pub reasoning_effort: ReasoningEffort,
}

/// A user-enabled route known to the live provider catalog.
#[derive(Debug, Clone)]
pub struct ModelCandidate {
    pub route: ModelRoute,
    /// Reasoning efforts accepted by this provider/model.
    pub supported_reasoning_efforts: BTreeSet<ReasoningEffort>,
    /// Estimated USD per 1M tokens across one input and one output token.
    ///
    /// Missing prices are never treated as free and therefore are not eligible
    /// for automatic selection.
    pub price_per_1m_tokens: Option<f64>,
    /// User-managed capability tags used only to distinguish equally eligible
    /// candidates, such as research from review.
    pub capabilities: BTreeSet<String>,
}

/// One policy-owned position in the ordered automatic-routing ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankingLadderEntry {
    pub rank: u16,
    pub class: ModelClass,
    pub route: ModelRoute,
}

/// A classified value with its point contribution and allowed model range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisValue {
    pub id: String,
    pub points: u16,
    pub minimum_class: ModelClass,
    pub maximum_class: ModelClass,
}

/// Independently trained classifier head for one routing axis.
#[derive(Debug, Clone)]
pub struct AxisRoute {
    pub values: BTreeMap<String, ClassRoute>,
}

/// Score domain and ordered candidate routes for automatic ranking.
#[derive(Debug, Clone)]
pub struct RankingPolicy {
    pub minimum_score: u16,
    pub maximum_score: u16,
    pub ladder: Vec<RankingLadderEntry>,
}

/// User-enabled candidates available for automatic route selection.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    candidates: Vec<ModelCandidate>,
}

impl ModelCatalog {
    pub fn new(candidates: impl IntoIterator<Item = ModelCandidate>) -> Self {
        Self {
            candidates: candidates.into_iter().collect(),
        }
    }

    fn contains(&self, route: &ModelRoute) -> bool {
        self.candidates.iter().any(|candidate| {
            candidate.route.provider_id == route.provider_id
                && candidate.route.model_slug == route.model_slug
                && candidate
                    .supported_reasoning_efforts
                    .contains(&route.reasoning_effort)
        })
    }
}

/// A minimum effort and user-managed capability preference for a classified task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteProfile {
    pub minimum_reasoning_effort: ReasoningEffort,
    pub required_capabilities: BTreeSet<String>,
}

/// Class-specific profile and score thresholds.
#[derive(Debug, Clone)]
pub struct ClassRoute {
    pub profile: RouteProfile,
    pub ranking: Option<(u16, ModelClass, ModelClass)>,
    pub weights: Vec<f32>,
    pub minimum_score: f32,
    pub minimum_margin: f32,
}

/// Versioned deterministic routing policy.
#[derive(Debug, Clone)]
pub struct RoutingPolicy {
    pub revision: String,
    pub fallback: ModelRoute,
    pub axes: BTreeMap<String, AxisRoute>,
    pub ranking: RankingPolicy,
}

/// Whether a decision is applied or diagnostic only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDisposition {
    Applied,
    Shadow,
    Fallback,
}

/// Machine-readable reason for a route outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionReason {
    Classified,
    SteeringBypass,
    LowConfidence,
    NoClass,
    InvalidPolicy,
    EmbeddingFailed,
    RouteUnavailable,
    UnsupportedScope,
    ExplicitOverride,
}

/// Complete bounded route decision.
#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub scope: RouteScope,
    pub class_id: Option<String>,
    pub classifications: BTreeMap<String, String>,
    pub ranking_score: Option<u16>,
    pub ranking_minimum_class: Option<ModelClass>,
    pub ranking_maximum_class: Option<ModelClass>,
    pub ranking_minimum_rank: Option<u16>,
    pub ranking_maximum_rank: Option<u16>,
    pub score: f32,
    pub margin: f32,
    pub original_route: Option<ModelRoute>,
    pub proposed_route: ModelRoute,
    /// Assigned only after the host has applied the configured mode and all
    /// explicit/safety override checks.
    pub effective_route: Option<ModelRoute>,
    pub disposition: RouteDisposition,
    pub reason: DecisionReason,
    pub policy_revision: String,
    pub prompt: crate::PromptMetadata,
}

/// Classify one task using a deterministic normalized dot-product head.
pub fn decide(
    task: TaskEnvelope<'_>,
    policy: &RoutingPolicy,
    catalog: &ModelCatalog,
    embedder: &dyn TaskEmbedder,
) -> RouteDecision {
    let (prompt, metadata) = normalize_task(task.prompt);
    let scope = task.scope;
    let original_route = task.current_route.cloned();
    let fallback = policy.fallback.clone();
    let fallback_decision = |reason, proposed_route| RouteDecision {
        scope,
        class_id: None,
        classifications: BTreeMap::new(),
        ranking_score: None,
        ranking_minimum_class: None,
        ranking_maximum_class: None,
        ranking_minimum_rank: None,
        ranking_maximum_rank: None,
        score: 0.0,
        margin: 0.0,
        original_route: original_route.clone(),
        proposed_route,
        effective_route: None,
        disposition: RouteDisposition::Fallback,
        reason,
        policy_revision: policy.revision.clone(),
        prompt: metadata.clone(),
    };
    let embedding = match embedder.embed(&prompt) {
        Ok(vector) => vector,
        Err(_) => {
            return fallback_decision(DecisionReason::EmbeddingFailed, fallback);
        }
    };
    if !policy_matches_embedding(policy, &embedding) {
        return fallback_decision(DecisionReason::InvalidPolicy, fallback);
    }
    let classified = policy
        .axes
        .iter()
        .map(|(axis, route)| best_class(&embedding, &route.values).map(|value| (axis, value)))
        .collect::<Option<Vec<_>>>();
    let Some(classified) = classified else {
        return fallback_decision(DecisionReason::NoClass, fallback);
    };
    let low_confidence = classified.iter().any(|(_, (_, route, score, margin))| {
        *score < route.minimum_score || *margin < route.minimum_margin
    });
    let primary = classified
        .iter()
        .find(|(axis, _)| *axis == "work_type")
        .map(|(_, (_, _, score, margin))| (*score, *margin));
    let classifications = classified.into_iter().fold(
        BTreeMap::new(),
        |mut classifications, (axis, (id, _, _, _))| {
            classifications.insert(axis.to_string(), id.clone());
            classifications
        },
    );
    let Some((score, margin)) = primary else {
        return fallback_decision(DecisionReason::NoClass, fallback);
    };
    let class_id = classifications.get("work_type").cloned();
    if class_id.as_deref() == Some("steering") {
        return RouteDecision {
            class_id,
            classifications,
            score,
            margin,
            ..fallback_decision(DecisionReason::SteeringBypass, fallback)
        };
    }
    if low_confidence {
        return RouteDecision {
            class_id,
            classifications,
            score,
            margin,
            ..fallback_decision(DecisionReason::LowConfidence, fallback)
        };
    }
    let Some(selection) = select_ranked_route(policy, catalog, &classifications) else {
        return RouteDecision {
            class_id,
            classifications,
            score,
            margin,
            ..fallback_decision(DecisionReason::RouteUnavailable, fallback)
        };
    };
    RouteDecision {
        scope,
        class_id,
        classifications,
        ranking_score: Some(selection.score),
        ranking_minimum_class: Some(selection.minimum_class),
        ranking_maximum_class: Some(selection.maximum_class),
        ranking_minimum_rank: Some(selection.minimum_rank),
        ranking_maximum_rank: Some(selection.maximum_rank),
        score,
        margin,
        original_route,
        proposed_route: selection.route,
        effective_route: None,
        // The host applies the configured mode only after it has also checked
        // explicit user and safety overrides.
        disposition: RouteDisposition::Shadow,
        reason: DecisionReason::Classified,
        policy_revision: policy.revision.clone(),
        prompt: metadata,
    }
}

fn policy_matches_embedding(policy: &RoutingPolicy, embedding: &[f32]) -> bool {
    !policy.revision.is_empty()
        && policy.axes.len() == 4
        && policy.axes.values().all(|axis| {
            !axis.values.is_empty()
                && axis.values.iter().all(|(class_id, class_route)| {
                    !class_id.is_empty()
                        && class_route.weights.len() == embedding.len()
                        && class_route.weights.iter().all(|weight| weight.is_finite())
                        && class_route.minimum_score.is_finite()
                        && class_route.minimum_margin.is_finite()
                })
        })
        && policy.ranking.minimum_score <= policy.ranking.maximum_score
        && !policy.ranking.ladder.is_empty()
}

struct RankedSelection {
    route: ModelRoute,
    score: u16,
    minimum_class: ModelClass,
    maximum_class: ModelClass,
    minimum_rank: u16,
    maximum_rank: u16,
}

fn select_ranked_route(
    policy: &RoutingPolicy,
    catalog: &ModelCatalog,
    classifications: &BTreeMap<String, String>,
) -> Option<RankedSelection> {
    let selected = policy
        .axes
        .iter()
        .map(|(axis, classifier)| {
            let id = classifications.get(axis)?;
            let route = classifier.values.get(id)?;
            axis_value(id, route)
        })
        .collect::<Option<Vec<_>>>()?;
    let score = selected.iter().map(|value| value.points).sum::<u16>();
    let minimum_class = selected.iter().map(|value| value.minimum_class).max()?;
    let maximum_class = selected.iter().map(|value| value.maximum_class).max()?;
    (minimum_class <= maximum_class).then_some(())?;
    let mut eligible = policy
        .ranking
        .ladder
        .iter()
        .filter(|entry| {
            entry.class >= minimum_class
                && entry.class <= maximum_class
                && catalog.contains(&entry.route)
        })
        .collect::<Vec<_>>();
    eligible.sort_by_key(|entry| entry.rank);
    let minimum_rank = eligible.first()?.rank;
    let maximum_rank = eligible.last()?.rank;
    let domain = policy.ranking.maximum_score - policy.ranking.minimum_score;
    let bounded = score.clamp(policy.ranking.minimum_score, policy.ranking.maximum_score);
    let offset = if domain == 0 {
        0
    } else {
        let numerator = u32::from(bounded - policy.ranking.minimum_score)
            * u32::from(maximum_rank - minimum_rank);
        ((numerator + u32::from(domain) / 2) / u32::from(domain)) as u16
    };
    let selected_rank = minimum_rank + offset;
    let entry = eligible
        .iter()
        .min_by_key(|entry| entry.rank.abs_diff(selected_rank))?;
    Some(RankedSelection {
        route: entry.route.clone(),
        score,
        minimum_class,
        maximum_class,
        minimum_rank,
        maximum_rank,
    })
}

fn axis_value(id: &str, route: &ClassRoute) -> Option<AxisValue> {
    let (points, minimum_class, maximum_class) = route.ranking.as_ref()?.clone();
    Some(AxisValue {
        id: id.to_string(),
        points,
        minimum_class,
        maximum_class,
    })
}

/// Apply the configured mode after a single shared classification decision.
///
/// `explicit_override` covers user choices and higher-priority safety policy.
/// The returned effective route is always either the original route, the
/// validated classified proposal, or absent when neither is executable.
pub fn finalize_decision(
    mut decision: RouteDecision,
    mode: RouterMode,
    explicit_override: bool,
) -> RouteDecision {
    if explicit_override {
        decision.effective_route = decision.original_route.clone();
        decision.disposition = RouteDisposition::Fallback;
        decision.reason = DecisionReason::ExplicitOverride;
        return decision;
    }

    if mode.applies(decision.scope) && decision.reason == DecisionReason::Classified {
        decision.effective_route = Some(decision.proposed_route.clone());
        decision.disposition = RouteDisposition::Applied;
        return decision;
    }

    decision.effective_route = decision.original_route.clone();
    decision.disposition =
        if decision.reason == DecisionReason::Classified && mode.classifies(decision.scope) {
            RouteDisposition::Shadow
        } else {
            RouteDisposition::Fallback
        };
    decision
}

fn best_class<'a>(
    embedding: &[f32],
    classes: &'a BTreeMap<String, ClassRoute>,
) -> Option<(String, &'a ClassRoute, f32, f32)> {
    if embedding.is_empty() || embedding.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let norm = embedding
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return None;
    }
    let mut scored = classes
        .iter()
        .map(|(class_id, route)| (class_id, route, norm.recip()))
        .map(|(class_id, route, inverse_norm)| {
            let score = inverse_norm
                * embedding
                    .iter()
                    .zip(route.weights.iter())
                    .map(|(value, weight)| value * weight)
                    .sum::<f32>();
            (class_id, route, score)
        })
        .collect::<Vec<_>>();
    if scored.iter().any(|(_, _, score)| !score.is_finite()) {
        return None;
    }
    scored.sort_by(|left, right| right.2.total_cmp(&left.2));
    let (class_id, route, score) = scored.first().copied()?;
    let runner_up = scored.get(1).map_or(0.0, |entry| entry.2);
    let margin = score - runner_up;
    margin
        .is_finite()
        .then(|| (class_id.clone(), route, score, margin))
}
