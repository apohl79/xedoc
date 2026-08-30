use crate::agent::exceeds_thread_spawn_depth_limit;
use crate::agent::next_thread_spawn_depth;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::tools::context::ToolInvocation;
use crate::tools::handlers::ApplyPatchHandler;
use crate::tools::handlers::CurrentTimeHandler;
use crate::tools::handlers::DynamicToolHandler;
use crate::tools::handlers::ExecCommandHandler;
use crate::tools::handlers::ExecCommandHandlerOptions;
use crate::tools::handlers::GetContextRemainingHandler;
use crate::tools::handlers::ListMcpResourceTemplatesHandler;
use crate::tools::handlers::ListMcpResourcesHandler;
use crate::tools::handlers::NewContextWindowHandler;
use crate::tools::handlers::PlanHandler;
use crate::tools::handlers::ReadMcpResourceHandler;
use crate::tools::handlers::RequestPermissionsHandler;
use crate::tools::handlers::RequestUserInputHandler;
use crate::tools::handlers::ShellCommandHandler;
use crate::tools::handlers::ShellCommandHandlerOptions;
use crate::tools::handlers::SleepHandler;
use crate::tools::handlers::TestSyncHandler;
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::handlers::ViewImageHandler;
use crate::tools::handlers::WaitForEnvironmentHandler;
use crate::tools::handlers::WriteStdinHandler;
use crate::tools::handlers::extension_tools::ExtensionToolAdapter;
use crate::tools::handlers::multi_agents::CloseAgentHandler;
use crate::tools::handlers::multi_agents::ResumeAgentHandler;
use crate::tools::handlers::multi_agents::SendInputHandler;
use crate::tools::handlers::multi_agents::SpawnAgentHandler;
use crate::tools::handlers::multi_agents::WaitAgentHandler;
use crate::tools::handlers::multi_agents_common::DEFAULT_WAIT_TIMEOUT_MS;
use crate::tools::handlers::multi_agents_common::MAX_WAIT_TIMEOUT_MS;
use crate::tools::handlers::multi_agents_common::MIN_WAIT_TIMEOUT_MS;
use crate::tools::handlers::multi_agents_spec::SpawnAgentToolOptions;
use crate::tools::handlers::multi_agents_spec::WaitAgentTimeoutOptions;
use crate::tools::handlers::multi_agents_v2::FollowupTaskHandler as FollowupTaskHandlerV2;
use crate::tools::handlers::multi_agents_v2::InterruptAgentHandler;
use crate::tools::handlers::multi_agents_v2::ListAgentsHandler as ListAgentsHandlerV2;
use crate::tools::handlers::multi_agents_v2::SendMessageHandler as SendMessageHandlerV2;
use crate::tools::handlers::multi_agents_v2::SpawnAgentHandler as SpawnAgentHandlerV2;
use crate::tools::handlers::multi_agents_v2::WaitAgentHandler as WaitAgentHandlerV2;
use crate::tools::handlers::view_image_spec::ViewImageToolOptions;
use crate::tools::hosted_spec::WebSearchToolOptions;
use crate::tools::hosted_spec::create_web_search_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExposure;
use crate::tools::registry::ToolRegistry;
use crate::tools::registry::override_tool_exposure;
use crate::tools::router::ToolRouter;
use crate::tools::router::ToolRouterParams;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::protocol::MultiAgentVersion;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::TOOL_SEARCH_TOOL_NAME;
use codex_tools::ToolCall as ExtensionToolCall;
use codex_tools::ToolEnvironmentMode;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSearchInfo;
use codex_tools::ToolSpec;
use codex_tools::UnifiedExecShellMode;
use codex_tools::can_request_original_image_detail;
use codex_tools::default_namespace_description;
use codex_tools::request_user_input_available_modes;
use codex_tools::shell_command_backend_for_features;
use codex_tools::shell_type_for_model_and_features;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::instrument;
use tracing::warn;

const MULTI_AGENT_V2_NAMESPACE_DESCRIPTION: &str = "Tools for spawning and managing sub-agents.";
const IMAGE_GEN_NAMESPACE: &str = "image_gen";
const IMAGEGEN_TOOL_NAME: &str = "imagegen";

type PlannedRuntime = Arc<dyn CoreToolRuntime>;

#[derive(Default)]
struct PlannedTools {
    runtimes: Vec<PlannedRuntime>,
    hosted_specs: Vec<ToolSpec>,
}

impl PlannedTools {
    fn add<T>(&mut self, handler: T)
    where
        T: CoreToolRuntime + 'static,
    {
        self.runtimes.push(Arc::new(handler));
    }

    fn add_arc(&mut self, handler: PlannedRuntime) {
        self.runtimes.push(handler);
    }

    fn add_with_exposure<T>(&mut self, handler: T, exposure: ToolExposure)
    where
        T: CoreToolRuntime + 'static,
    {
        self.runtimes
            .push(override_tool_exposure(Arc::new(handler), exposure));
    }

    fn add_dispatch_only<T>(&mut self, handler: T)
    where
        T: CoreToolRuntime + 'static,
    {
        self.add_with_exposure(handler, ToolExposure::Hidden);
    }

    fn add_hosted_spec(&mut self, spec: ToolSpec) {
        self.hosted_specs.push(spec);
    }

    fn runtimes(&self) -> &[PlannedRuntime] {
        &self.runtimes
    }
}

#[derive(Clone, Copy)]
struct CoreToolPlanContext<'a> {
    step_context: &'a StepContext,
    tool_runtimes: &'a [PlannedRuntime],
    extension_tool_executors: &'a [Arc<dyn ToolExecutor<ExtensionToolCall>>],
    dynamic_tools: &'a [DynamicToolSpec],
    tool_search_handler_cache: &'a ToolSearchHandlerCache,
    default_agent_type_description: &'a str,
    wait_agent_timeouts: WaitAgentTimeoutOptions,
}

#[instrument(level = "trace", skip_all)]
pub(crate) fn build_tool_router(
    step_context: &StepContext,
    params: ToolRouterParams<'_>,
    tool_search_handler_cache: &ToolSearchHandlerCache,
) -> ToolRouter {
    let (model_visible_specs, registry) =
        build_tool_specs_and_registry(step_context, params, tool_search_handler_cache);
    ToolRouter::from_parts(registry, model_visible_specs)
}

#[instrument(level = "trace", skip_all)]
fn build_tool_specs_and_registry(
    step_context: &StepContext,
    params: ToolRouterParams<'_>,
    tool_search_handler_cache: &ToolSearchHandlerCache,
) -> (Vec<ToolSpec>, ToolRegistry) {
    let turn_context = step_context.turn.as_ref();
    let ToolRouterParams {
        tool_runtimes,
        extension_tool_executors,
        dynamic_tools,
    } = params;
    let default_agent_type_description =
        crate::agent::role::spawn_tool_spec::build(&std::collections::BTreeMap::new());
    let context = CoreToolPlanContext {
        step_context,
        tool_runtimes: &tool_runtimes,
        extension_tool_executors: &extension_tool_executors,
        dynamic_tools,
        tool_search_handler_cache,
        default_agent_type_description: &default_agent_type_description,
        wait_agent_timeouts: wait_agent_timeout_options(turn_context),
    };
    let mut planned_tools = PlannedTools::default();
    add_tool_sources(&context, &mut planned_tools);
    append_tool_search_executor(&context, &mut planned_tools);
    build_model_visible_specs_and_registry(turn_context, planned_tools)
}

#[instrument(level = "trace", skip_all)]
fn build_model_visible_specs_and_registry(
    turn_context: &TurnContext,
    planned_tools: PlannedTools,
) -> (Vec<ToolSpec>, ToolRegistry) {
    let PlannedTools {
        runtimes,
        hosted_specs,
    } = planned_tools;
    let mut specs = Vec::new();
    let mut seen_tool_names = HashSet::new();
    for runtime in &runtimes {
        let tool_name = runtime.tool_name();
        if !seen_tool_names.insert(tool_name.clone()) {
            continue;
        }
        let exposure = runtime.exposure();
        if exposure.is_direct() {
            specs.push(runtime.spec());
        }
    }
    specs.extend(hosted_specs);

    let registry = ToolRegistry::from_tools(runtimes);
    let model_visible_specs = merge_into_namespaces(specs)
        .into_iter()
        .filter(|spec| {
            namespace_tools_enabled(turn_context) || !matches!(spec, ToolSpec::Namespace(_))
        })
        .collect();

    (model_visible_specs, registry)
}

#[instrument(level = "trace", skip_all)]
fn hosted_model_tool_specs(context: &CoreToolPlanContext<'_>) -> Vec<ToolSpec> {
    let turn_context = context.step_context.turn.as_ref();
    // Responses Lite accepts schemas for client-executed tools, not hosted Responses tools.
    if turn_context.model_info.use_responses_lite {
        return Vec::new();
    }

    let mut specs = Vec::new();
    let standalone_web_search_available = standalone_web_search_enabled(turn_context)
        && context
            .extension_tool_executors
            .iter()
            .any(|executor| executor.tool_name() == ToolName::namespaced("web", "run"));
    // `Some(Cached/Live/Disabled)` are the options for mode when standalone search is unavailable
    // and the provider supports hosted search. `None` prevents emitting a hosted search tool.
    let web_search_mode = (!standalone_web_search_available
        && turn_context.provider.capabilities().web_search)
        .then_some(turn_context.config.web_search_mode.value());
    let web_search_config = web_search_mode
        .as_ref()
        .and(turn_context.config.web_search_config.as_ref());
    if let Some(hosted_web_search_tool) = create_web_search_tool(WebSearchToolOptions {
        web_search_mode,
        web_search_config,
        web_search_tool_type: turn_context.model_info.web_search_tool_type,
    }) {
        specs.push(hosted_web_search_tool);
    }
    specs
}

pub(crate) fn search_tool_enabled(turn_context: &TurnContext) -> bool {
    turn_context.model_info.supports_search_tool && namespace_tools_enabled(turn_context)
}

fn namespace_tools_enabled(turn_context: &TurnContext) -> bool {
    turn_context.provider.capabilities().namespace_tools
}

fn multi_agent_v2_enabled(turn_context: &TurnContext) -> bool {
    turn_context.multi_agent_version == MultiAgentVersion::V2
}

fn collab_tools_enabled(turn_context: &TurnContext) -> bool {
    match turn_context.multi_agent_version {
        MultiAgentVersion::Disabled => false,
        MultiAgentVersion::V1 => !exceeds_thread_spawn_depth_limit(
            next_thread_spawn_depth(&turn_context.session_source),
            turn_context.config.agent_max_depth,
        ),
        MultiAgentVersion::V2 => true,
    }
}

fn image_generation_available(turn_context: &TurnContext) -> bool {
    if !turn_context
        .config
        .features
        .get()
        .enabled(Feature::ImageGeneration)
    {
        return false;
    }

    let capabilities = turn_context.provider.capabilities();
    if !capabilities.image_generation || !capabilities.namespace_tools {
        return false;
    }

    if !turn_context
        .model_info
        .input_modalities
        .contains(&InputModality::Image)
    {
        return false;
    }

    let provider = turn_context.provider.info();
    provider.uses_openai_actor_authorization()
        || (provider.requires_openai_auth
            && turn_context
                .auth_manager
                .as_deref()
                .is_some_and(AuthManager::current_auth_uses_codex_backend))
}

fn wait_agent_timeout_options(turn_context: &TurnContext) -> WaitAgentTimeoutOptions {
    if multi_agent_v2_enabled(turn_context) {
        return WaitAgentTimeoutOptions {
            default_timeout_ms: turn_context.config.multi_agent_v2.default_wait_timeout_ms,
            min_timeout_ms: turn_context.config.multi_agent_v2.min_wait_timeout_ms,
            max_timeout_ms: turn_context.config.multi_agent_v2.max_wait_timeout_ms,
        };
    }

    WaitAgentTimeoutOptions {
        default_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
        min_timeout_ms: MIN_WAIT_TIMEOUT_MS,
        max_timeout_ms: MAX_WAIT_TIMEOUT_MS,
    }
}

fn agent_type_description(
    turn_context: &TurnContext,
    default_agent_type_description: &str,
) -> String {
    let agent_type_description =
        crate::agent::role::spawn_tool_spec::build(&turn_context.config.agent_roles);
    if agent_type_description.is_empty() {
        default_agent_type_description.to_string()
    } else {
        agent_type_description
    }
}

#[instrument(level = "trace", skip_all, fields(tool_spec_count = specs.len()))]
fn merge_into_namespaces(specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let mut merged_specs = Vec::with_capacity(specs.len());
    let mut namespace_indices = BTreeMap::<String, usize>::new();
    for spec in specs {
        match spec {
            ToolSpec::Namespace(mut namespace) => {
                if let Some(index) = namespace_indices.get(&namespace.name).copied() {
                    let ToolSpec::Namespace(existing_namespace) = &mut merged_specs[index] else {
                        unreachable!("namespace index must point to a namespace spec");
                    };
                    if existing_namespace.description.trim().is_empty()
                        && !namespace.description.trim().is_empty()
                    {
                        existing_namespace.description = namespace.description;
                    }
                    existing_namespace.tools.append(&mut namespace.tools);
                    continue;
                }

                namespace_indices.insert(namespace.name.clone(), merged_specs.len());
                merged_specs.push(ToolSpec::Namespace(namespace));
            }
            spec => merged_specs.push(spec),
        }
    }

    for spec in &mut merged_specs {
        let ToolSpec::Namespace(namespace) = spec else {
            continue;
        };

        namespace.tools.sort_by(|left, right| match (left, right) {
            (
                ResponsesApiNamespaceTool::Function(left),
                ResponsesApiNamespaceTool::Function(right),
            ) => left.name.cmp(&right.name),
        });

        if namespace.description.trim().is_empty() {
            namespace.description = default_namespace_description(&namespace.name);
        }
    }

    merged_specs
}

#[instrument(level = "trace", skip_all)]
fn add_tool_sources(context: &CoreToolPlanContext<'_>, planned_tools: &mut PlannedTools) {
    add_shell_tools(context, planned_tools);
    add_mcp_resource_tools(context, planned_tools);
    add_core_utility_tools(context, planned_tools);
    add_collaboration_tools(context, planned_tools);
    for runtime in context.tool_runtimes {
        planned_tools.add_arc(Arc::clone(runtime));
    }
    add_extension_tools(context, planned_tools);
    add_dynamic_tools(context, planned_tools);
    for spec in hosted_model_tool_specs(context) {
        planned_tools.add_hosted_spec(spec);
    }
}

fn standalone_web_search_enabled(turn_context: &TurnContext) -> bool {
    namespace_tools_enabled(turn_context)
        && (turn_context.model_info.use_responses_lite
            || turn_context
                .config
                .features
                .get()
                .enabled(Feature::StandaloneWebSearch))
}

fn tool_environment_mode(step_context: &StepContext) -> ToolEnvironmentMode {
    ToolEnvironmentMode::from_count(step_context.environments.turn_environments().count())
}

#[instrument(level = "trace", skip_all)]
fn add_shell_tools(context: &CoreToolPlanContext<'_>, planned_tools: &mut PlannedTools) {
    let turn_context = context.step_context.turn.as_ref();
    let features = turn_context.config.features.get();
    let environment_mode = tool_environment_mode(context.step_context);
    if !environment_mode.has_environment() {
        return;
    }

    let allow_login_shell = turn_context.config.permissions.allow_login_shell;
    let exec_permission_approvals_enabled = features.enabled(Feature::ExecPermissionApprovals);
    let include_environment_id = matches!(environment_mode, ToolEnvironmentMode::Multiple);
    let shell_command_options = ShellCommandHandlerOptions {
        backend_config: shell_command_backend_for_features(features),
        allow_login_shell,
        exec_permission_approvals_enabled,
    };

    match shell_type_for_model_and_features(&turn_context.model_info, features) {
        ConfigShellToolType::UnifiedExec => {
            planned_tools.add(ExecCommandHandler::new(ExecCommandHandlerOptions {
                allow_login_shell,
                exec_permission_approvals_enabled,
                include_environment_id,
                include_shell_parameter: unified_exec_should_include_shell_parameter(
                    turn_context,
                    context.step_context,
                ),
            }));
            planned_tools.add(WriteStdinHandler);

            // Keep the legacy shell tool registered while unified exec is
            // model-visible.
            planned_tools.add_dispatch_only(ShellCommandHandler::new(shell_command_options));
        }
        ConfigShellToolType::Disabled => {}
        ConfigShellToolType::Default
        | ConfigShellToolType::Local
        | ConfigShellToolType::ShellCommand => {
            planned_tools.add(ShellCommandHandler::new(shell_command_options));
        }
    }
}

fn unified_exec_should_include_shell_parameter(
    turn_context: &TurnContext,
    step_context: &StepContext,
) -> bool {
    !matches!(
        &turn_context.unified_exec_shell_mode,
        UnifiedExecShellMode::ZshFork(_)
    ) || step_context
        .environments
        .turn_environments()
        .any(|environment| environment.environment.is_remote())
}

#[instrument(level = "trace", skip_all)]
fn add_mcp_resource_tools(context: &CoreToolPlanContext<'_>, planned_tools: &mut PlannedTools) {
    if context.step_context.mcp.manager().has_servers() {
        planned_tools.add(ListMcpResourcesHandler);
        planned_tools.add(ListMcpResourceTemplatesHandler);
        planned_tools.add(ReadMcpResourceHandler);
    }
}

#[instrument(level = "trace", skip_all)]
fn add_core_utility_tools(context: &CoreToolPlanContext<'_>, planned_tools: &mut PlannedTools) {
    let turn_context = context.step_context.turn.as_ref();
    let features = turn_context.config.features.get();
    let environment_mode = tool_environment_mode(context.step_context);

    planned_tools.add(PlanHandler);

    if features.enabled(Feature::DeferredExecutor) {
        planned_tools.add(WaitForEnvironmentHandler);
    }

    if turn_context.config.experimental_request_user_input_enabled {
        planned_tools.add(RequestUserInputHandler {
            available_modes: request_user_input_available_modes(features),
        });
    }

    if environment_mode.has_environment() && features.enabled(Feature::RequestPermissionsTool) {
        planned_tools.add(RequestPermissionsHandler);
    }

    if features.enabled(Feature::TokenBudget) {
        planned_tools.add(NewContextWindowHandler);
        planned_tools.add(GetContextRemainingHandler);
    }

    if features.enabled(Feature::CurrentTimeReminder) {
        planned_tools.add(CurrentTimeHandler);
        if turn_context
            .config
            .current_time_reminder
            .as_ref()
            .is_some_and(|config| config.sleep_tool)
        {
            planned_tools.add(SleepHandler);
        }
    }

    if environment_mode.has_environment() && turn_context.model_info.apply_patch_tool_type.is_some()
    {
        let include_environment_id = matches!(environment_mode, ToolEnvironmentMode::Multiple);
        planned_tools.add(ApplyPatchHandler::new(include_environment_id));
    }

    if turn_context
        .model_info
        .experimental_supported_tools
        .iter()
        .any(|tool| tool == "test_sync_tool")
    {
        planned_tools.add(TestSyncHandler);
    }

    if environment_mode.has_environment() {
        let include_environment_id = matches!(environment_mode, ToolEnvironmentMode::Multiple);
        planned_tools.add(ViewImageHandler::new(ViewImageToolOptions {
            can_request_original_image_detail: can_request_original_image_detail(
                &turn_context.model_info,
            ),
            include_environment_id,
        }));
    }
}

#[instrument(level = "trace", skip_all)]
fn add_collaboration_tools(context: &CoreToolPlanContext<'_>, planned_tools: &mut PlannedTools) {
    let turn_context = context.step_context.turn.as_ref();
    if collab_tools_enabled(turn_context) {
        if multi_agent_v2_enabled(turn_context) {
            let exposure = ToolExposure::Direct;
            let tool_namespace = namespace_tools_enabled(turn_context)
                .then_some(turn_context.config.multi_agent_v2.tool_namespace.as_deref())
                .flatten();
            let agent_type_description =
                agent_type_description(turn_context, context.default_agent_type_description);
            let hide_spawn_agent_metadata =
                turn_context.config.multi_agent_v2.hide_spawn_agent_metadata;
            if !exceeds_thread_spawn_depth_limit(
                next_thread_spawn_depth(&turn_context.session_source),
                turn_context.config.agent_max_depth,
            ) {
                planned_tools.add_arc(override_tool_exposure(
                    multi_agent_v2_handler(
                        SpawnAgentHandlerV2::new(SpawnAgentToolOptions {
                            available_models: turn_context.available_models.clone(),
                            active_model_provider_id: turn_context.config.model_provider_id.clone(),
                            agent_type_description,
                            expose_agent_type: !turn_context.config.agent_roles.is_empty(),
                            hide_agent_type_model_reasoning: hide_spawn_agent_metadata,
                            expose_spawn_agent_model_overrides: turn_context
                                .config
                                .multi_agent_v2
                                .expose_spawn_agent_model_overrides,
                            multi_agent_version: turn_context.multi_agent_version,
                            usage_hint_text: turn_context
                                .config
                                .multi_agent_v2
                                .usage_hint_text
                                .clone(),
                        }),
                        tool_namespace,
                    ),
                    exposure,
                ));
            }
            planned_tools.add_arc(override_tool_exposure(
                multi_agent_v2_handler(SendMessageHandlerV2, tool_namespace),
                exposure,
            ));
            planned_tools.add_arc(override_tool_exposure(
                multi_agent_v2_handler(FollowupTaskHandlerV2, tool_namespace),
                exposure,
            ));
            planned_tools.add_arc(override_tool_exposure(
                multi_agent_v2_handler(
                    WaitAgentHandlerV2::new(context.wait_agent_timeouts),
                    tool_namespace,
                ),
                exposure,
            ));
            planned_tools.add_arc(override_tool_exposure(
                multi_agent_v2_handler(InterruptAgentHandler, tool_namespace),
                exposure,
            ));
            planned_tools.add_arc(override_tool_exposure(
                multi_agent_v2_handler(ListAgentsHandlerV2, tool_namespace),
                exposure,
            ));
        } else {
            let agent_type_description =
                agent_type_description(turn_context, context.default_agent_type_description);
            let exposure = if search_tool_enabled(turn_context) {
                ToolExposure::Deferred
            } else {
                ToolExposure::Direct
            };
            planned_tools.add_with_exposure(
                SpawnAgentHandler::new(SpawnAgentToolOptions {
                    available_models: turn_context.available_models.clone(),
                    active_model_provider_id: turn_context.config.model_provider_id.clone(),
                    agent_type_description,
                    expose_agent_type: !turn_context.config.agent_roles.is_empty(),
                    hide_agent_type_model_reasoning: false,
                    expose_spawn_agent_model_overrides: true,
                    multi_agent_version: turn_context.multi_agent_version,
                    usage_hint_text: turn_context.config.multi_agent_v2.usage_hint_text.clone(),
                }),
                exposure,
            );
            planned_tools.add_with_exposure(SendInputHandler, exposure);
            planned_tools.add_with_exposure(ResumeAgentHandler, exposure);
            planned_tools
                .add_with_exposure(WaitAgentHandler::new(context.wait_agent_timeouts), exposure);
            planned_tools.add_with_exposure(CloseAgentHandler, exposure);
        }
    }
}

#[instrument(level = "trace", skip_all, fields(dynamic_tool_count = context.dynamic_tools.len()))]
fn add_dynamic_tools(context: &CoreToolPlanContext<'_>, planned_tools: &mut PlannedTools) {
    for spec in context.dynamic_tools {
        match spec {
            DynamicToolSpec::Function(tool) => {
                let Some(handler) = DynamicToolHandler::new(tool) else {
                    tracing::error!(
                        "Failed to convert dynamic tool {:?} to OpenAI tool",
                        tool.name
                    );
                    continue;
                };
                planned_tools.add(handler);
            }
            DynamicToolSpec::Namespace(namespace) => {
                for tool in &namespace.tools {
                    let DynamicToolNamespaceTool::Function(tool) = tool;
                    let Some(handler) = DynamicToolHandler::new_in_namespace(namespace, tool)
                    else {
                        tracing::error!(
                            "Failed to convert dynamic tool {:?}.{:?} to OpenAI tool",
                            namespace.name,
                            tool.name
                        );
                        continue;
                    };
                    planned_tools.add(handler);
                }
            }
        }
    }
}

#[instrument(
    level = "trace",
    skip_all,
    fields(extension_tool_executor_count = context.extension_tool_executors.len())
)]
fn add_extension_tools(context: &CoreToolPlanContext<'_>, planned_tools: &mut PlannedTools) {
    // Extension ToolContributor implementations are resolved into executors
    // before planning. Core only adapts those executors into its runtime set.
    append_extension_tool_executors(
        context.step_context.turn.as_ref(),
        context.extension_tool_executors,
        planned_tools,
    );
}

#[instrument(level = "trace", skip_all)]
fn append_tool_search_executor(
    context: &CoreToolPlanContext<'_>,
    planned_tools: &mut PlannedTools,
) {
    let turn_context = context.step_context.turn.as_ref();
    if !search_tool_enabled(turn_context) {
        return;
    }

    let search_infos = planned_tools
        .runtimes()
        .iter()
        .filter(|executor| executor.exposure() == ToolExposure::Deferred)
        .filter_map(|executor| executor.search_info())
        .collect::<Vec<_>>();
    if search_infos.is_empty() {
        return;
    }

    let handler: PlannedRuntime = context.tool_search_handler_cache.get_or_build(search_infos);
    planned_tools.add_arc(handler);
}

fn append_extension_tool_executors(
    turn_context: &TurnContext,
    executors: &[Arc<dyn ToolExecutor<ExtensionToolCall>>],
    planned_tools: &mut PlannedTools,
) {
    if executors.is_empty() {
        return;
    }

    let mut reserved_tool_names = planned_tools
        .runtimes()
        .iter()
        .map(|executor| executor.tool_name())
        .collect::<HashSet<_>>();
    if search_tool_enabled(turn_context)
        && planned_tools
            .runtimes()
            .iter()
            .any(|executor| executor.exposure() == ToolExposure::Deferred)
    {
        reserved_tool_names.insert(ToolName::plain(TOOL_SEARCH_TOOL_NAME));
    }

    let standalone_web_search_enabled = standalone_web_search_enabled(turn_context);
    let web_search_mode_on = turn_context.config.web_search_mode.value() != WebSearchMode::Disabled;

    for executor in executors.iter().cloned() {
        let tool_name = executor.tool_name();
        if tool_name == ToolName::namespaced("web", "run")
            && (!standalone_web_search_enabled || !web_search_mode_on)
        {
            continue;
        }
        if tool_name == ToolName::namespaced(IMAGE_GEN_NAMESPACE, IMAGEGEN_TOOL_NAME)
            && !image_generation_available(turn_context)
        {
            continue;
        }
        if !reserved_tool_names.insert(tool_name.clone()) {
            warn!("Skipping extension tool `{tool_name}`: tool already registered");
            continue;
        }
        planned_tools.add(ExtensionToolAdapter::new(executor));
    }
}

fn multi_agent_v2_handler(
    handler: impl CoreToolRuntime + 'static,
    namespace: Option<&str>,
) -> Arc<dyn CoreToolRuntime> {
    match namespace {
        Some(namespace) => Arc::new(MultiAgentV2NamespaceOverride {
            handler: Arc::new(handler),
            namespace: namespace.to_string(),
        }),
        None => Arc::new(handler),
    }
}

struct MultiAgentV2NamespaceOverride {
    handler: Arc<dyn CoreToolRuntime>,
    namespace: String,
}

impl ToolExecutor<ToolInvocation> for MultiAgentV2NamespaceOverride {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(self.namespace.clone(), self.handler.tool_name().name)
    }

    fn spec(&self) -> ToolSpec {
        match self.handler.spec() {
            ToolSpec::Function(tool) => ToolSpec::Namespace(ResponsesApiNamespace {
                name: self.namespace.clone(),
                description: MULTI_AGENT_V2_NAMESPACE_DESCRIPTION.to_string(),
                tools: vec![ResponsesApiNamespaceTool::Function(tool)],
            }),
            spec => spec,
        }
    }

    fn exposure(&self) -> ToolExposure {
        self.handler.exposure()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.handler.supports_parallel_tool_calls()
    }

    fn search_info(&self) -> Option<ToolSearchInfo> {
        self.handler.search_info()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        self.handler.handle(invocation)
    }
}

impl CoreToolRuntime for MultiAgentV2NamespaceOverride {
    fn matches_kind(&self, payload: &crate::tools::context::ToolPayload) -> bool {
        self.handler.matches_kind(payload)
    }

    fn create_diff_consumer(
        &self,
    ) -> Option<Box<dyn crate::tools::registry::ToolArgumentDiffConsumer>> {
        self.handler.create_diff_consumer()
    }
}

#[cfg(test)]
#[path = "spec_plan_tests.rs"]
mod tests;
