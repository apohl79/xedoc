pub const TOOL_CALL_COUNT_METRIC: &str = "xedoc.tool.call";
pub const TOOL_CALL_DURATION_METRIC: &str = "xedoc.tool.call.duration_ms";
pub const TOOL_CALL_UNIFIED_EXEC_METRIC: &str = "xedoc.tool.unified_exec";
pub const TOOL_OUTPUT_REDUCTION_TOKENS_IN_METRIC: &str = "xedoc.tool_output.reduction.tokens_in";
pub const TOOL_OUTPUT_REDUCTION_TOKENS_OUT_METRIC: &str = "xedoc.tool_output.reduction.tokens_out";
pub const TOOL_OUTPUT_REDUCTION_DURATION_US_METRIC: &str =
    "xedoc.tool_output.reduction.duration_us";
pub const TOOL_OUTPUT_RETRIEVAL_METRIC: &str = "xedoc.tool_output.reduction.retrieval";
pub const PROCESS_START_METRIC: &str = "xedoc.process.start";
pub const API_CALL_COUNT_METRIC: &str = "xedoc.api_request";
pub const API_CALL_DURATION_METRIC: &str = "xedoc.api_request.duration_ms";
pub const SSE_EVENT_COUNT_METRIC: &str = "xedoc.sse_event";
pub const SSE_EVENT_DURATION_METRIC: &str = "xedoc.sse_event.duration_ms";
pub const WEBSOCKET_REQUEST_COUNT_METRIC: &str = "xedoc.websocket.request";
pub const WEBSOCKET_REQUEST_DURATION_METRIC: &str = "xedoc.websocket.request.duration_ms";
pub const WEBSOCKET_EVENT_COUNT_METRIC: &str = "xedoc.websocket.event";
pub const WEBSOCKET_EVENT_DURATION_METRIC: &str = "xedoc.websocket.event.duration_ms";
pub const RESPONSES_API_OVERHEAD_DURATION_METRIC: &str = "xedoc.responses_api_overhead.duration_ms";
pub const RESPONSES_API_INFERENCE_TIME_DURATION_METRIC: &str =
    "xedoc.responses_api_inference_time.duration_ms";
pub const RESPONSES_API_ENGINE_IAPI_TTFT_DURATION_METRIC: &str =
    "xedoc.responses_api_engine_iapi_ttft.duration_ms";
pub const RESPONSES_API_ENGINE_SERVICE_TTFT_DURATION_METRIC: &str =
    "xedoc.responses_api_engine_service_ttft.duration_ms";
pub const RESPONSES_API_ENGINE_IAPI_TBT_DURATION_METRIC: &str =
    "xedoc.responses_api_engine_iapi_tbt.duration_ms";
pub const RESPONSES_API_ENGINE_SERVICE_TBT_DURATION_METRIC: &str =
    "xedoc.responses_api_engine_service_tbt.duration_ms";
pub const TURN_E2E_DURATION_METRIC: &str = "xedoc.turn.e2e_duration_ms";
pub const TURN_TTFT_DURATION_METRIC: &str = "xedoc.turn.ttft.duration_ms";
pub const TURN_TTFM_DURATION_METRIC: &str = "xedoc.turn.ttfm.duration_ms";
pub const TURN_NETWORK_PROXY_METRIC: &str = "xedoc.turn.network_proxy";
pub const TURN_TOOL_CALL_METRIC: &str = "xedoc.turn.tool.call";
pub const TURN_TOKEN_USAGE_METRIC: &str = "xedoc.turn.token_usage";
pub const GOAL_CREATED_METRIC: &str = "xedoc.goal.created";
pub const GOAL_RESUMED_METRIC: &str = "xedoc.goal.resumed";
pub const GOAL_COMPLETED_METRIC: &str = "xedoc.goal.completed";
pub const GOAL_BUDGET_LIMITED_METRIC: &str = "xedoc.goal.budget_limited";
pub const GOAL_USAGE_LIMITED_METRIC: &str = "xedoc.goal.usage_limited";
pub const GOAL_BLOCKED_METRIC: &str = "xedoc.goal.blocked";
pub const GOAL_TOKEN_COUNT_METRIC: &str = "xedoc.goal.token_count";
pub const GOAL_DURATION_SECONDS_METRIC: &str = "xedoc.goal.duration_s";
pub const CURATED_PLUGINS_STARTUP_SYNC_METRIC: &str = "xedoc.plugins.startup_sync";
pub const CURATED_PLUGINS_STARTUP_SYNC_FINAL_METRIC: &str = "xedoc.plugins.startup_sync.final";
pub const HOOK_RUN_METRIC: &str = "xedoc.hooks.run";
pub const HOOK_RUN_DURATION_METRIC: &str = "xedoc.hooks.run.duration_ms";
/// Duration for coarse startup phases, tagged by low-cardinality phase and status.
pub const STARTUP_PHASE_DURATION_METRIC: &str = "xedoc.startup.phase.duration_ms";
/// Total runtime of a startup prewarm attempt until it completes, tagged by final status.
pub const STARTUP_PREWARM_DURATION_METRIC: &str = "xedoc.startup_prewarm.duration_ms";
/// Age of the startup prewarm attempt when the first real turn resolves it, tagged by outcome.
pub const STARTUP_PREWARM_AGE_AT_FIRST_TURN_METRIC: &str =
    "xedoc.startup_prewarm.age_at_first_turn_ms";
pub const THREAD_STARTED_METRIC: &str = "xedoc.thread.started";
pub const THREAD_SKILLS_ENABLED_TOTAL_METRIC: &str = "xedoc.thread.skills.enabled_total";
pub const THREAD_SKILLS_KEPT_TOTAL_METRIC: &str = "xedoc.thread.skills.kept_total";
pub const THREAD_SKILLS_DESCRIPTION_TRUNCATED_CHARS_METRIC: &str =
    "xedoc.thread.skills.description_truncated_chars";
pub const THREAD_SKILLS_TRUNCATED_METRIC: &str = "xedoc.thread.skills.truncated";
