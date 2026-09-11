//! Local model-router host adapter for bounded shadow decisions.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use xedoc_config::ModelRouterClass;
use xedoc_config::ModelRouterMode;
use xedoc_config::ModelRouterPolicy;
use xedoc_config::ModelRouterPolicyRevisionStore;
use xedoc_config::ModelRouterPolicyStore;
use xedoc_core_config::config::Config;
use xedoc_install_context::InstallContext;
use xedoc_model_router::BoundedEmbedder;
use xedoc_model_router::ClassRoute;
use xedoc_model_router::FastEmbedder;
use xedoc_model_router::LocalArtifact;
use xedoc_model_router::ModelCandidate;
use xedoc_model_router::ModelCatalog;
use xedoc_model_router::ModelRoute;
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
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_protocol::protocol::ModelRouterDecisionEvent;
use xedoc_protocol::protocol::ModelRouterDecisionReason;
use xedoc_protocol::protocol::ModelRouterDisposition;
use xedoc_protocol::protocol::ModelRouterEffectiveRoute;
use xedoc_protocol::protocol::ModelRouterScope;

const MODEL_ROUTER_ARTIFACT_PATH: &str = "model-router/arctic-embed-xs";
const ROUTER_WORKER_CAPACITY: usize = 8;

pub(crate) struct ModelRouterService {
    policy_store: ModelRouterPolicyStore,
    policy_revisions: ModelRouterPolicyRevisionStore,
    artifact_path: Option<PathBuf>,
    runtime: OnceLock<Result<RouterRuntime, String>>,
}

impl ModelRouterService {
    fn new(config: &Config) -> Self {
        let policy_path = config
            .model_router
            .resolved_policy_path(config.xedoc_home.as_path());
        Self {
            policy_store: ModelRouterPolicyStore::new(policy_path.clone()),
            policy_revisions: ModelRouterPolicyRevisionStore::new(policy_path),
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
            Err(()) | Ok(_) => fallback_decision(RouteScope::Subagent, current_route, prompt),
        };
        Some(xedoc_model_router::finalize_decision(
            decision,
            mode_with_calibration_gate(
                mode,
                RouteScope::Subagent,
                policy.as_ref(),
                &service.policy_revisions,
            ),
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
            Err(()) | Ok(_) => fallback_decision(RouteScope::Root, current_route, prompt),
        };
        Some(xedoc_model_router::finalize_decision(
            decision,
            mode_with_calibration_gate(
                mode,
                RouteScope::Root,
                policy.as_ref(),
                &service.policy_revisions,
            ),
            explicit_override,
        ))
    }

    fn runtime(&self) -> Result<&RouterRuntime, ()> {
        self.runtime
            .get_or_init(|| {
                let artifact_path = self
                    .artifact_path
                    .as_ref()
                    .ok_or_else(|| "bundled artifact is unavailable".to_string())?;
                let artifact =
                    LocalArtifact::load(artifact_path).map_err(|error| error.to_string())?;
                let embedder = FastEmbedder::from_local_artifact(&artifact)
                    .map_err(|error| error.to_string())?;
                Ok(RouterRuntime {
                    artifact_sha256: artifact.descriptor.sha256,
                    artifact_revision: artifact.descriptor.revision,
                    dimensions: artifact.descriptor.dimensions,
                    embedder: BoundedEmbedder::new(embedder, ROUTER_WORKER_CAPACITY),
                })
            })
            .as_ref()
            .map_err(|_| ())
    }
}

fn mode_with_calibration_gate(
    mode: RouterMode,
    scope: RouteScope,
    policy: &ModelRouterPolicy,
    revisions: &ModelRouterPolicyRevisionStore,
) -> RouterMode {
    if !mode.applies(scope) || revisions.active_revision_is_calibrated(&policy.policy_revision) {
        return mode;
    }
    match scope {
        RouteScope::Root => RouterMode::ShadowFull,
        RouteScope::Subagent => RouterMode::ShadowSubagents,
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
        || config
            .service_tier
            .as_deref()
            .is_some_and(|tier| !model_info.supports_service_tier(tier))
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
        classes: policy
            .classes
            .iter()
            .filter_map(|class| class_route(class, policy))
            .collect(),
    }
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
    ModelCatalog::new(models.into_iter().map(|model| {
        let provider_id = if model.provider_id.is_empty() {
            config.model_provider_id.clone()
        } else {
            model.provider_id
        };
        let price_per_1m_tokens = config
            .model_providers
            .get(&provider_id)
            .and_then(|provider| provider.model_prices.as_ref())
            .and_then(|prices| prices.get(&model.model))
            .map(|prices| prices.input_price_per_1m_tokens + prices.output_price_per_1m_tokens);
        ModelCandidate {
            route: ModelRoute {
                provider_id: provider_id.clone(),
                model_slug: model.model.clone(),
                reasoning_effort: router_reasoning_effort(model.default_reasoning_effort),
            },
            price_per_1m_tokens,
            capabilities: policy
                .capabilities
                .iter()
                .find(|capability| {
                    capability.provider == provider_id && capability.model == model.model
                })
                .map(|capability| capability.tags.iter().cloned().collect())
                .unwrap_or_else(BTreeSet::new),
        }
    }))
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
                classes: BTreeMap::new(),
            },
            &ModelCatalog::default(),
            &UnavailableEmbedder,
        ),
        RouterMode::ShadowSubagents,
        false,
    )
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
        ReasoningEffort::XHigh
        | ReasoningEffort::Max
        | ReasoningEffort::Ultra
        | ReasoningEffort::Custom(_) => RouterReasoningEffort::ExtraHigh,
    }
}

fn protocol_reasoning_effort(effort: RouterReasoningEffort) -> Option<ReasoningEffort> {
    match effort {
        RouterReasoningEffort::Low => Some(ReasoningEffort::Low),
        RouterReasoningEffort::Medium => Some(ReasoningEffort::Medium),
        RouterReasoningEffort::High => Some(ReasoningEffort::High),
        RouterReasoningEffort::ExtraHigh => Some(ReasoningEffort::XHigh),
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
        policy_revision: decision.policy_revision,
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

const fn effort_label(effort: RouterReasoningEffort) -> &'static str {
    match effort {
        RouterReasoningEffort::Low => "low",
        RouterReasoningEffort::Medium => "medium",
        RouterReasoningEffort::High => "high",
        RouterReasoningEffort::ExtraHigh => "xhigh",
    }
}
