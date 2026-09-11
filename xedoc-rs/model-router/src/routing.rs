//! Deterministic route policy and decision types.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::RouteScope;
use crate::RouterMode;
use crate::task::TaskEnvelope;
use crate::task::normalize_task;
use crate::worker::TaskEmbedder;

/// Closed reasoning effort values accepted by the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    ExtraHigh,
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
    /// Estimated USD per 1M tokens across one input and one output token.
    ///
    /// Missing prices are never treated as free and therefore are not eligible
    /// for automatic selection.
    pub price_per_1m_tokens: Option<f64>,
    /// User-managed capability tags used only to distinguish equally eligible
    /// candidates, such as research from review.
    pub capabilities: BTreeSet<String>,
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

    fn select(&self, profile: &RouteProfile) -> Option<ModelRoute> {
        self.candidates
            .iter()
            .filter(|candidate| {
                candidate.price_per_1m_tokens.is_some_and(f64::is_finite)
                    && candidate
                        .price_per_1m_tokens
                        .is_some_and(|price| price >= 0.0)
                    && effort_rank(candidate.route.reasoning_effort)
                        >= effort_rank(profile.minimum_reasoning_effort)
                    && profile
                        .required_capabilities
                        .iter()
                        .all(|capability| candidate.capabilities.contains(capability))
            })
            .min_by(|left, right| {
                let left_price = left.price_per_1m_tokens.unwrap_or(f64::INFINITY);
                let right_price = right.price_per_1m_tokens.unwrap_or(f64::INFINITY);
                left_price
                    .total_cmp(&right_price)
                    .then_with(|| {
                        effort_rank(left.route.reasoning_effort)
                            .cmp(&effort_rank(right.route.reasoning_effort))
                    })
                    .then_with(|| left.route.provider_id.cmp(&right.route.provider_id))
                    .then_with(|| left.route.model_slug.cmp(&right.route.model_slug))
            })
            .map(|candidate| candidate.route.clone())
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
    pub weights: Vec<f32>,
    pub minimum_score: f32,
    pub minimum_margin: f32,
}

/// Versioned deterministic routing policy.
#[derive(Debug, Clone)]
pub struct RoutingPolicy {
    pub revision: String,
    pub fallback: ModelRoute,
    pub classes: BTreeMap<String, ClassRoute>,
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
    let Some((class_id, class_route, score, margin)) = best_class(&embedding, &policy.classes)
    else {
        return fallback_decision(DecisionReason::NoClass, fallback);
    };
    let proposed_route = catalog.select(&class_route.profile);
    if score < class_route.minimum_score || margin < class_route.minimum_margin {
        return RouteDecision {
            class_id: Some(class_id),
            score,
            margin,
            ..fallback_decision(DecisionReason::LowConfidence, fallback)
        };
    }
    let Some(proposed_route) = proposed_route else {
        return RouteDecision {
            class_id: Some(class_id),
            score,
            margin,
            ..fallback_decision(DecisionReason::RouteUnavailable, fallback)
        };
    };
    RouteDecision {
        scope,
        class_id: Some(class_id),
        score,
        margin,
        original_route,
        proposed_route,
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
        && !policy.classes.is_empty()
        && policy.classes.iter().all(|(class_id, class_route)| {
            !class_id.is_empty()
                && class_route.weights.len() == embedding.len()
                && class_route.weights.iter().all(|weight| weight.is_finite())
                && class_route.minimum_score.is_finite()
                && class_route.minimum_margin.is_finite()
        })
}

const fn effort_rank(effort: ReasoningEffort) -> u8 {
    match effort {
        ReasoningEffort::Low => 0,
        ReasoningEffort::Medium => 1,
        ReasoningEffort::High => 2,
        ReasoningEffort::ExtraHigh => 3,
    }
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
