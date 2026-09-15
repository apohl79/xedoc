//! Local model-router host adapter for bounded shadow decisions.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use xedoc_config::ModelRouterApproval;
use xedoc_config::ModelRouterClass;
use xedoc_config::ModelRouterMode;
use xedoc_config::ModelRouterModelClass;
use xedoc_config::ModelRouterPolicy;
use xedoc_config::ModelRouterPolicyStore;
use xedoc_core_config::config::Config;
use xedoc_install_context::InstallContext;
use xedoc_model_router::AxisRoute;
use xedoc_model_router::BoundedEmbedder;
use xedoc_model_router::ClassRoute;
use xedoc_model_router::FastEmbedder;
use xedoc_model_router::LocalArtifact;
use xedoc_model_router::ModelCandidate;
use xedoc_model_router::ModelCatalog;
use xedoc_model_router::ModelClass;
use xedoc_model_router::ModelRoute;
use xedoc_model_router::RankingLadderEntry;
use xedoc_model_router::RankingPolicy;
use xedoc_model_router::ReasoningEffort as RouterReasoningEffort;
use xedoc_model_router::RouteDecision;
use xedoc_model_router::RouteProfile;
use xedoc_model_router::RouteScope;
use xedoc_model_router::RouterMode;
use xedoc_model_router::RoutingPolicy;
use xedoc_model_router::TaskEmbedder;
use xedoc_model_router::TaskEnvelope;
use xedoc_model_router::decide;
use xedoc_models_manager::manager::RefreshStrategy;
use xedoc_models_manager::manager::SharedModelsManager;
use xedoc_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::protocol::ModelRouterApprovalClassRating;
use xedoc_protocol::protocol::ModelRouterApprovalRankedRoute;
use xedoc_protocol::protocol::ModelRouterApprovalRoute;
use xedoc_protocol::protocol::ModelRouterDecisionEvent;
use xedoc_protocol::protocol::ModelRouterDecisionReason;
use xedoc_protocol::protocol::ModelRouterDisposition;
use xedoc_protocol::protocol::ModelRouterEffectiveRoute;
use xedoc_protocol::protocol::ModelRouterScope;

const MODEL_ROUTER_ARTIFACT_PATH: &str = "model-router/arctic-embed-xs";
const ROUTER_WORKER_CAPACITY: usize = 8;
const MAX_EVENT_DIAGNOSTIC_CHARS: usize = 1_024;
const EVENT_DIAGNOSTIC_TRUNCATION_SUFFIX: &str = "… [truncated]";

pub(crate) struct ModelRouterService {
    policy_store: ModelRouterPolicyStore,
    artifact_path: Option<PathBuf>,
    runtime: OnceLock<Result<RouterRuntime, String>>,
}

impl ModelRouterService {
    fn new(config: &Config) -> Self {
        let policy_path = config
            .model_router
            .resolved_policy_path(config.xedoc_home.as_path());
        Self {
            policy_store: ModelRouterPolicyStore::new(policy_path),
            artifact_path: bundled_artifact_path(),
            runtime: OnceLock::new(),
        }
    }

    fn global(config: &Config) -> &'static Self {
        static SERVICE: OnceLock<ModelRouterService> = OnceLock::new();
        SERVICE.get_or_init(|| Self::new(config))
    }

    pub(crate) async fn decide_subagent(
        config: &Config,
        models_manager: &SharedModelsManager,
        prompt: &str,
        current_route: ModelRoute,
        explicit_override: bool,
    ) -> Option<RouteDecision> {
        let mode = router_mode(config.model_router.mode);
        if !mode.classifies(RouteScope::Subagent) {
            return None;
        }

        let service = Self::global(config);
        let _ = service.policy_store.reload_if_changed();
        let Some(policy) = service.policy_store.snapshot() else {
            return Some(fallback_decision(
                RouteScope::Subagent,
                current_route,
                prompt,
            ));
        };
        let catalog = model_catalog(config, models_manager, &policy).await;
        let decision = match service.runtime() {
            Ok(runtime) if runtime.matches(&policy) => decide(
                TaskEnvelope {
                    scope: RouteScope::Subagent,
                    prompt,
                    current_route: Some(&current_route),
                },
                &routing_policy(&policy, current_route.clone()),
                &catalog,
                &runtime.embedder,
            ),
            Err(error) => {
                fallback_runtime_decision(RouteScope::Subagent, current_route, prompt, error)
            }
            Ok(_) => fallback_decision(RouteScope::Subagent, current_route, prompt),
        };
        Some(xedoc_model_router::finalize_decision(
            decision,
            mode,
            explicit_override,
        ))
    }

    /// Produces a root-turn decision without mutating the session route.
    pub(crate) async fn decide_root(
        config: &Config,
        models_manager: &SharedModelsManager,
        prompt: &str,
        current_route: ModelRoute,
        explicit_override: bool,
    ) -> Option<RouteDecision> {
        let mode = router_mode(config.model_router.mode);
        if !mode.classifies(RouteScope::Root) {
            return None;
        }

        let service = Self::global(config);
        let _ = service.policy_store.reload_if_changed();
        let Some(policy) = service.policy_store.snapshot() else {
            return Some(fallback_decision(RouteScope::Root, current_route, prompt));
        };
        let catalog = model_catalog(config, models_manager, &policy).await;
        let decision = match service.runtime() {
            Ok(runtime) if runtime.matches(&policy) => decide(
                TaskEnvelope {
                    scope: RouteScope::Root,
                    prompt,
                    current_route: Some(&current_route),
                },
                &routing_policy(&policy, current_route.clone()),
                &catalog,
                &runtime.embedder,
            ),
            Err(error) => fallback_runtime_decision(RouteScope::Root, current_route, prompt, error),
            Ok(_) => fallback_decision(RouteScope::Root, current_route, prompt),
        };
        Some(xedoc_model_router::finalize_decision(
            decision,
            mode,
            explicit_override,
        ))
    }

    /// Records accepted active-turn input without embedding or rerouting it.
    pub(crate) fn steering_bypass_root(
        config: &Config,
        prompt: &str,
        current_route: ModelRoute,
    ) -> Option<RouteDecision> {
        let mode = router_mode(config.model_router.mode);
        if !mode.classifies(RouteScope::Root) {
            return None;
        }

        let service = Self::global(config);
        let _ = service.policy_store.reload_if_changed();
        let policy = service
            .policy_store
            .snapshot()
            .map(|policy| routing_policy(&policy, current_route.clone()))
            .unwrap_or_else(|| RoutingPolicy {
                revision: "unavailable".to_string(),
                fallback: current_route.clone(),
                axes: BTreeMap::new(),
                ranking: RankingPolicy {
                    minimum_score: 0,
                    maximum_score: 0,
                    ladder: Vec::new(),
                },
            });
        Some(xedoc_model_router::steering_bypass(
            TaskEnvelope {
                scope: RouteScope::Root,
                prompt,
                current_route: Some(&current_route),
            },
            &policy,
            &ModelCatalog::default(),
        ))
    }

    /// Returns the current user-selected reporting baseline, if any.
    pub(crate) fn reporting_baseline(
        config: &Config,
    ) -> Option<xedoc_config::ModelRouterRankedRoute> {
        let service = Self::global(config);
        let _ = service.policy_store.reload_if_changed();
        service
            .policy_store
            .snapshot()
            .and_then(|policy| policy.ranking.reporting_baseline.clone())
    }

    /// Incorporate explicit user classifier corrections into the active policy.
    ///
    /// The policy store reload keeps subsequent decisions in this process on
    /// the newly calibrated class heads.
    pub(crate) async fn recalibrate_from_feedback(
        config: std::sync::Arc<Config>,
        feedback_path: xedoc_utils_absolute_path::AbsolutePathBuf,
    ) -> Result<xedoc_model_router::FeedbackCalibrationReport, String> {
        tokio::task::spawn_blocking(move || {
            Self::recalibrate_from_feedback_sync(&config, feedback_path.as_path())
        })
        .await
        .map_err(|error| format!("model-router recalibration worker failed: {error}"))?
    }

    fn recalibrate_from_feedback_sync(
        config: &Config,
        feedback_path: &Path,
    ) -> Result<xedoc_model_router::FeedbackCalibrationReport, String> {
        let service = Self::global(config);
        let artifact_path = service
            .artifact_path
            .as_deref()
            .ok_or_else(|| "bundled model-router artifact is unavailable".to_string())?;
        let policy_path = config
            .model_router
            .resolved_policy_path(config.xedoc_home.as_path());
        let report = xedoc_model_router::recalibrate_classifier_from_feedback(
            &policy_path,
            feedback_path,
            artifact_path,
        )
        .map_err(|error| error.to_string())?;
        service
            .policy_store
            .reload_if_changed()
            .map_err(|error| error.to_string())?;
        Ok(report)
    }

    fn runtime(&self) -> Result<&RouterRuntime, &str> {
        self.runtime
            .get_or_init(|| {
                let artifact_path = self
                    .artifact_path
                    .as_ref()
                    .ok_or_else(|| "bundled artifact is unavailable".to_string())?;
                let artifact =
                    LocalArtifact::load(artifact_path).map_err(|error| error_diagnostic(&error))?;
                let embedder = FastEmbedder::from_local_artifact(&artifact)
                    .map_err(|error| error_diagnostic(&error))?;
                Ok(RouterRuntime {
                    artifact_sha256: artifact.descriptor.sha256,
                    artifact_revision: artifact.descriptor.revision,
                    dimensions: artifact.descriptor.dimensions,
                    embedder: BoundedEmbedder::new(embedder, ROUTER_WORKER_CAPACITY),
                })
            })
            .as_ref()
            .map_err(String::as_str)
    }
}

/// Applies one already-classified proposal to a temporary configuration.
///
/// The caller retains the original configuration until this complete validation
/// succeeds, so an unavailable provider, model, effort, or service tier can
/// always fall back to the original route.
pub(crate) async fn apply_route_to_config(
    config: &mut Config,
    models_manager: &SharedModelsManager,
    route: &ModelRoute,
) -> bool {
    let Some(provider) = config.model_providers.get(&route.provider_id).cloned() else {
        return false;
    };
    let Some(reasoning_effort) = protocol_reasoning_effort(route.reasoning_effort) else {
        return false;
    };
    let model_info = models_manager
        .get_model_info_for_provider(
            &route.model_slug,
            &route.provider_id,
            &config.to_models_manager_config(),
        )
        .await;
    if model_info.used_fallback_model_metadata
        || !model_info
            .supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == reasoning_effort)
        || config.service_tier.as_deref().is_some_and(|tier| {
            tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE && !model_info.supports_service_tier(tier)
        })
    {
        return false;
    }

    config.model = Some(route.model_slug.clone());
    config.model_provider_id = route.provider_id.clone();
    config.model_provider = provider;
    config.model_reasoning_effort = Some(reasoning_effort);
    true
}

pub(crate) fn fallback_to_original_route(decision: &mut RouteDecision) {
    decision.effective_route = decision.original_route.clone();
    decision.disposition = xedoc_model_router::RouteDisposition::Fallback;
    decision.reason = xedoc_model_router::DecisionReason::RouteUnavailable;
}

fn bundled_artifact_path() -> Option<PathBuf> {
    InstallContext::current()
        .bundled_resource(format!("{MODEL_ROUTER_ARTIFACT_PATH}/manifest.json"))
        .and_then(|manifest_path| manifest_path.parent().map(PathBuf::from))
}

struct RouterRuntime {
    artifact_sha256: String,
    artifact_revision: String,
    dimensions: usize,
    embedder: BoundedEmbedder,
}

impl RouterRuntime {
    fn matches(&self, policy: &ModelRouterPolicy) -> bool {
        self.artifact_sha256 == policy.embedding.artifact_sha256
            && self.artifact_revision == policy.embedding.revision
            && self.dimensions == policy.embedding.dimensions
    }
}

fn router_mode(mode: ModelRouterMode) -> RouterMode {
    match mode {
        ModelRouterMode::Off => RouterMode::Off,
        ModelRouterMode::ShadowSubagents => RouterMode::ShadowSubagents,
        ModelRouterMode::ShadowFull => RouterMode::ShadowFull,
        ModelRouterMode::Subagents => RouterMode::Subagents,
        ModelRouterMode::Full => RouterMode::Full,
    }
}

fn routing_policy(policy: &ModelRouterPolicy, fallback: ModelRoute) -> RoutingPolicy {
    RoutingPolicy {
        revision: policy.policy_revision.clone(),
        fallback,
        axes: policy
            .axes
            .iter()
            .map(|axis| {
                let values = if axis.id == "work_type" {
                    work_type_group_routes(&axis.classes, policy)
                } else {
                    axis.classes
                        .iter()
                        .filter_map(|class| class_route(class, policy))
                        .collect()
                };
                (axis.id.clone(), AxisRoute { values })
            })
            .collect(),
        ranking: RankingPolicy {
            minimum_score: policy.ranking.minimum_score,
            maximum_score: policy.ranking.maximum_score,
            ladder: policy
                .ranking
                .ladder
                .iter()
                .map(|entry| RankingLadderEntry {
                    rank: entry.rank,
                    class: model_class(entry.class),
                    route: ModelRoute {
                        provider_id: entry.provider.clone(),
                        model_slug: entry.model.clone(),
                        reasoning_effort: router_reasoning_effort(entry.reasoning_effort.clone()),
                    },
                })
                .collect(),
        },
    }
}

fn work_type_group_routes(
    classes: &[ModelRouterClass],
    policy: &ModelRouterPolicy,
) -> BTreeMap<String, ClassRoute> {
    let mut groups = BTreeMap::<
        (
            u16,
            xedoc_config::ModelRouterModelClass,
            xedoc_config::ModelRouterModelClass,
        ),
        Vec<&ModelRouterClass>,
    >::new();
    for class in classes.iter().filter(|class| class.id != "steering") {
        groups
            .entry((
                class.points,
                class.minimum_model_class,
                class.maximum_model_class,
            ))
            .or_default()
            .push(class);
    }
    groups
        .into_values()
        .enumerate()
        .filter_map(|(group_index, group)| {
            let group_id = format!(
                "group{group_index}: {}",
                group
                    .iter()
                    .map(|class| class.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let routes = group
                .iter()
                .filter_map(|class| class_route(class, policy).map(|(_, route)| route))
                .collect::<Vec<_>>();
            let first = routes.first()?;
            let mut weights = vec![0.0; first.weights.len()];
            for route in &routes {
                weights
                    .iter_mut()
                    .zip(&route.weights)
                    .for_each(|(sum, weight)| *sum += weight);
            }
            let count = routes.len() as f32;
            weights.iter_mut().for_each(|weight| *weight /= count);
            Some((
                group_id,
                ClassRoute {
                    profile: RouteProfile {
                        minimum_reasoning_effort: routes
                            .iter()
                            .map(|route| route.profile.minimum_reasoning_effort)
                            .max()?,
                        required_capabilities: routes
                            .iter()
                            .flat_map(|route| route.profile.required_capabilities.iter().cloned())
                            .collect(),
                    },
                    ranking: first.ranking,
                    weights,
                    minimum_score: routes
                        .iter()
                        .map(|route| route.minimum_score)
                        .fold(0.0, f32::max),
                    minimum_margin: routes
                        .iter()
                        .map(|route| route.minimum_margin)
                        .fold(0.0, f32::max),
                },
            ))
        })
        .collect()
}

fn class_route(
    class: &ModelRouterClass,
    policy: &ModelRouterPolicy,
) -> Option<(String, ClassRoute)> {
    Some((
        class.id.clone(),
        ClassRoute {
            profile: RouteProfile {
                minimum_reasoning_effort: router_reasoning_effort(
                    class.minimum_reasoning_effort.clone(),
                ),
                required_capabilities: class.required_capabilities.iter().cloned().collect(),
            },
            ranking: Some((
                class.points,
                model_class(class.minimum_model_class),
                model_class(class.maximum_model_class),
            )),
            weights: class.weights.clone()?,
            minimum_score: class
                .minimum_score
                .unwrap_or(policy.classifier.minimum_score),
            minimum_margin: class
                .minimum_margin
                .unwrap_or(policy.classifier.minimum_margin),
        },
    ))
}

async fn model_catalog(
    config: &Config,
    models_manager: &SharedModelsManager,
    policy: &ModelRouterPolicy,
) -> ModelCatalog {
    let models = models_manager
        .list_models(RefreshStrategy::Offline, config.http_client_factory())
        .await;
    let mut candidates = Vec::new();
    for model in models {
        let provider_id = if model.provider_id.is_empty() {
            config.model_provider_id.clone()
        } else {
            model.provider_id
        };
        let model_info = models_manager
            .get_model_info_for_provider(
                &model.model,
                &provider_id,
                &config.to_models_manager_config(),
            )
            .await;
        if model_info.used_fallback_model_metadata
            || config.service_tier.as_deref().is_some_and(|tier| {
                tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE
                    && !model_info.supports_service_tier(tier)
            })
        {
            continue;
        }
        let supported_reasoning_efforts = model_info
            .supported_reasoning_levels
            .iter()
            .map(|preset| router_reasoning_effort(preset.effort.clone()))
            .collect::<BTreeSet<_>>();
        if supported_reasoning_efforts.is_empty() {
            continue;
        }
        let price_per_1m_tokens = config
            .model_providers
            .get(&provider_id)
            .and_then(|provider| provider.model_prices.as_ref())
            .and_then(|prices| prices.get(&model.model))
            .map(|prices| prices.input_price_per_1m_tokens + prices.output_price_per_1m_tokens);
        candidates.push(ModelCandidate {
            route: ModelRoute {
                provider_id: provider_id.clone(),
                model_slug: model.model.clone(),
                reasoning_effort: router_reasoning_effort(model.default_reasoning_effort),
            },
            supported_reasoning_efforts,
            price_per_1m_tokens,
            capabilities: policy
                .capabilities
                .iter()
                .find(|capability| {
                    capability.provider == provider_id && capability.model == model.model
                })
                .map(|capability| capability.tags.iter().cloned().collect())
                .unwrap_or_else(BTreeSet::new),
        });
    }
    ModelCatalog::new(candidates)
}

fn fallback_decision(scope: RouteScope, current_route: ModelRoute, prompt: &str) -> RouteDecision {
    xedoc_model_router::finalize_decision(
        decide(
            TaskEnvelope {
                scope,
                prompt,
                current_route: Some(&current_route),
            },
            &RoutingPolicy {
                revision: "unavailable".to_string(),
                fallback: current_route.clone(),
                axes: BTreeMap::new(),
                ranking: RankingPolicy {
                    minimum_score: 0,
                    maximum_score: 0,
                    ladder: Vec::new(),
                },
            },
            &ModelCatalog::default(),
            &UnavailableEmbedder,
        ),
        RouterMode::ShadowSubagents,
        false,
    )
}

fn fallback_runtime_decision(
    scope: RouteScope,
    current_route: ModelRoute,
    prompt: &str,
    error: &str,
) -> RouteDecision {
    let mut decision = fallback_decision(scope, current_route, prompt);
    decision.diagnostic = Some(error.to_string());
    decision
}

fn error_diagnostic(error: &(dyn std::error::Error + 'static)) -> String {
    let mut diagnostic = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        diagnostic.push_str(": ");
        diagnostic.push_str(&error.to_string());
        source = error.source();
    }
    diagnostic
}

fn model_class(class: ModelRouterModelClass) -> ModelClass {
    match class {
        ModelRouterModelClass::Simple => ModelClass::Simple,
        ModelRouterModelClass::Smart => ModelClass::Smart,
        ModelRouterModelClass::Intelligent => ModelClass::Intelligent,
    }
}

struct UnavailableEmbedder;

impl TaskEmbedder for UnavailableEmbedder {
    fn embed(&self, _: &str) -> Result<Vec<f32>, xedoc_model_router::EmbedError> {
        Err(xedoc_model_router::EmbedError::WorkerStopped)
    }
}

fn router_reasoning_effort(effort: ReasoningEffort) -> RouterReasoningEffort {
    match effort {
        ReasoningEffort::None | ReasoningEffort::Minimal | ReasoningEffort::Low => {
            RouterReasoningEffort::Low
        }
        ReasoningEffort::Medium => RouterReasoningEffort::Medium,
        ReasoningEffort::High => RouterReasoningEffort::High,
        ReasoningEffort::XHigh | ReasoningEffort::Ultra | ReasoningEffort::Custom(_) => {
            RouterReasoningEffort::ExtraHigh
        }
        ReasoningEffort::Max => RouterReasoningEffort::Max,
    }
}

fn protocol_reasoning_effort(effort: RouterReasoningEffort) -> Option<ReasoningEffort> {
    match effort {
        RouterReasoningEffort::Low => Some(ReasoningEffort::Low),
        RouterReasoningEffort::Medium => Some(ReasoningEffort::Medium),
        RouterReasoningEffort::High => Some(ReasoningEffort::High),
        RouterReasoningEffort::ExtraHigh => Some(ReasoningEffort::XHigh),
        RouterReasoningEffort::Max => Some(ReasoningEffort::Max),
    }
}

pub(crate) fn current_route(config: &Config) -> Option<ModelRoute> {
    Some(route_for_model(
        config,
        config.model.as_deref()?,
        config.model_reasoning_effort.clone()?,
    ))
}

pub(crate) fn route_for_model(
    config: &Config,
    model_slug: &str,
    reasoning_effort: ReasoningEffort,
) -> ModelRoute {
    ModelRoute {
        provider_id: config.model_provider_id.clone(),
        model_slug: model_slug.to_string(),
        reasoning_effort: router_reasoning_effort(reasoning_effort),
    }
}

pub(crate) fn decision_event(
    decision: RouteDecision,
    thread_id: String,
    turn_id: String,
    scope: ModelRouterScope,
    created_at: i64,
) -> ModelRouterDecisionEvent {
    if let Some(error) = decision.diagnostic.as_deref() {
        tracing::warn!(%error, ?scope, "model-router embedding failed");
    }
    ModelRouterDecisionEvent {
        decision_id: uuid::Uuid::now_v7().to_string(),
        thread_id,
        turn_id,
        scope,
        disposition: match decision.disposition {
            xedoc_model_router::RouteDisposition::Applied => ModelRouterDisposition::Applied,
            xedoc_model_router::RouteDisposition::Shadow => ModelRouterDisposition::Shadow,
            xedoc_model_router::RouteDisposition::Fallback => ModelRouterDisposition::Fallback,
        },
        reason: match decision.reason {
            xedoc_model_router::DecisionReason::Classified => ModelRouterDecisionReason::Classified,
            xedoc_model_router::DecisionReason::SteeringBypass => {
                ModelRouterDecisionReason::SteeringBypass
            }
            xedoc_model_router::DecisionReason::LowConfidence => {
                ModelRouterDecisionReason::LowConfidence
            }
            xedoc_model_router::DecisionReason::NoClass => ModelRouterDecisionReason::NoClass,
            xedoc_model_router::DecisionReason::EmbeddingFailed
            | xedoc_model_router::DecisionReason::InvalidPolicy
            | xedoc_model_router::DecisionReason::UnsupportedScope => {
                ModelRouterDecisionReason::EmbeddingFailed
            }
            xedoc_model_router::DecisionReason::RouteUnavailable => {
                ModelRouterDecisionReason::RouteUnavailable
            }
            xedoc_model_router::DecisionReason::ExplicitOverride => {
                ModelRouterDecisionReason::ExplicitOverride
            }
        },
        diagnostic: decision.diagnostic.map(|diagnostic| {
            let mut chars = diagnostic.chars();
            let full_diagnostic = chars
                .by_ref()
                .take(MAX_EVENT_DIAGNOSTIC_CHARS + 1)
                .collect::<String>();
            if full_diagnostic.chars().count() <= MAX_EVENT_DIAGNOSTIC_CHARS {
                full_diagnostic
            } else {
                let summary = full_diagnostic
                    .chars()
                    .take(
                        MAX_EVENT_DIAGNOSTIC_CHARS
                            - EVENT_DIAGNOSTIC_TRUNCATION_SUFFIX.chars().count(),
                    )
                    .collect::<String>();
                format!("{summary}{EVENT_DIAGNOSTIC_TRUNCATION_SUFFIX}")
            }
        }),
        policy_revision: decision.policy_revision,
        classifications: decision.classifications,
        confidence_score: decision.score,
        confidence_margin: decision.margin,
        ranking_score: decision.ranking_score,
        ranking_minimum_class: decision
            .ranking_minimum_class
            .map(model_class_label)
            .map(str::to_string),
        ranking_maximum_class: decision
            .ranking_maximum_class
            .map(model_class_label)
            .map(str::to_string),
        ranking_minimum_rank: decision.ranking_minimum_rank,
        ranking_maximum_rank: decision.ranking_maximum_rank,
        ranking_target_rank: decision.ranking_target_rank,
        ranking_selected_rank: decision.ranking_selected_rank,
        proposed_provider_id: decision.proposed_route.provider_id,
        proposed_model_slug: decision.proposed_route.model_slug,
        proposed_reasoning_effort: effort_label(decision.proposed_route.reasoning_effort)
            .to_string(),
        effective_route: decision.effective_route.map_or(
            ModelRouterEffectiveRoute::Unavailable,
            |route| ModelRouterEffectiveRoute::Available {
                provider_id: route.provider_id,
                model_slug: route.model_slug,
                reasoning_effort: effort_label(route.reasoning_effort).to_string(),
            },
        ),
        prompt_sha256: decision.prompt.sha256,
        prompt_original_bytes: u64::try_from(decision.prompt.original_bytes).unwrap_or(u64::MAX),
        prompt_truncated: decision.prompt.truncated,
        created_at,
    }
}

pub(crate) fn requires_approval(
    mode: ModelRouterMode,
    approval: ModelRouterApproval,
    decision: &RouteDecision,
) -> bool {
    let required = router_mode(mode).applies(decision.scope)
        && decision.reason != xedoc_model_router::DecisionReason::SteeringBypass
        && match approval {
            ModelRouterApproval::Off => false,
            ModelRouterApproval::Changes => {
                decision.disposition == xedoc_model_router::RouteDisposition::Applied
                    && decision.effective_route != decision.original_route
            }
            ModelRouterApproval::All => {
                decision.disposition != xedoc_model_router::RouteDisposition::Shadow
                    && decision.reason != xedoc_model_router::DecisionReason::ExplicitOverride
            }
        };
    tracing::debug!(
        ?mode,
        ?approval,
        ?decision.scope,
        ?decision.disposition,
        ?decision.reason,
        required,
        "evaluated model-router approval requirement"
    );
    required
}

pub(crate) fn feedback_classifications(
    response: &xedoc_protocol::protocol::ModelRouterApprovalResponse,
) -> BTreeMap<String, String> {
    let mut classifications = response.classifications.clone();
    if let Some(classification) = response.classification.as_ref() {
        classifications
            .entry("work_type".to_string())
            .or_insert_with(|| classification.clone());
    }
    classifications
}

pub(crate) fn approval_event(
    decision: &RouteDecision,
    thread_id: String,
    turn_id: String,
    scope: ModelRouterScope,
) -> xedoc_protocol::protocol::ModelRouterApprovalRequestEvent {
    xedoc_protocol::protocol::ModelRouterApprovalRequestEvent {
        approval_id: uuid::Uuid::now_v7().to_string(),
        thread_id,
        turn_id,
        scope,
        predicted_classification: decision.class_id.clone().unwrap_or_default(),
        classifications: decision.classifications.clone(),
        classification_options: decision.classification_options.clone(),
        available_routes: decision
            .available_routes
            .iter()
            .map(|route| ModelRouterApprovalRoute {
                provider_id: route.provider_id.clone(),
                model_slug: route.model_slug.clone(),
                reasoning_effort: effort_label(route.reasoning_effort).to_string(),
            })
            .collect(),
        classification_ratings: decision
            .approval_routing
            .axes
            .iter()
            .map(|(axis, values)| {
                (
                    axis.clone(),
                    values
                        .iter()
                        .map(|(id, value)| {
                            (
                                id.clone(),
                                ModelRouterApprovalClassRating {
                                    points: value.points,
                                    minimum_model_class: model_class_label(value.minimum_class)
                                        .to_string(),
                                    maximum_model_class: model_class_label(value.maximum_class)
                                        .to_string(),
                                },
                            )
                        })
                        .collect(),
                )
            })
            .collect(),
        ranking_minimum_score: decision.approval_routing.minimum_score,
        ranking_maximum_score: decision.approval_routing.maximum_score,
        ranking_ladder: decision
            .approval_routing
            .ladder
            .iter()
            .map(|entry| ModelRouterApprovalRankedRoute {
                rank: entry.rank,
                model_class: model_class_label(entry.class).to_string(),
                route: ModelRouterApprovalRoute {
                    provider_id: entry.route.provider_id.clone(),
                    model_slug: entry.route.model_slug.clone(),
                    reasoning_effort: effort_label(entry.route.reasoning_effort).to_string(),
                },
            })
            .collect(),
        proposed_provider_id: decision.proposed_route.provider_id.clone(),
        proposed_model_slug: decision.proposed_route.model_slug.clone(),
        proposed_reasoning_effort: effort_label(decision.proposed_route.reasoning_effort)
            .to_string(),
        current_route: decision.original_route.as_ref().map_or(
            ModelRouterEffectiveRoute::Unavailable,
            |route| ModelRouterEffectiveRoute::Available {
                provider_id: route.provider_id.clone(),
                model_slug: route.model_slug.clone(),
                reasoning_effort: effort_label(route.reasoning_effort).to_string(),
            },
        ),
        score: decision.score,
        margin: decision.margin,
        classifier_revision: decision.policy_revision.clone(),
        policy_revision: decision.policy_revision.clone(),
        prompt_sha256: decision.prompt.sha256.clone(),
    }
}

const fn effort_label(effort: RouterReasoningEffort) -> &'static str {
    match effort {
        RouterReasoningEffort::Low => "low",
        RouterReasoningEffort::Medium => "medium",
        RouterReasoningEffort::High => "high",
        RouterReasoningEffort::ExtraHigh => "xhigh",
        RouterReasoningEffort::Max => "max",
    }
}

const fn model_class_label(class: ModelClass) -> &'static str {
    match class {
        ModelClass::Simple => "simple",
        ModelClass::Smart => "smart",
        ModelClass::Intelligent => "intelligent",
    }
}
