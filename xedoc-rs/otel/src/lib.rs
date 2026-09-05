pub(crate) mod config;
mod events;
pub(crate) mod metrics;
pub(crate) mod provider;
pub(crate) mod trace_context;

mod otlp;
mod targets;

use crate::metrics::Result as MetricsResult;
use serde::Serialize;
use strum_macros::Display;
use xedoc_protocol::auth::AuthMode;

pub use crate::config::OtelExporter;
pub use crate::config::OtelHttpProtocol;
pub use crate::config::OtelSettings;
pub use crate::config::OtelTlsConfig;
pub use crate::config::validate_span_attributes;
pub use crate::events::session_telemetry::AuthEnvTelemetryMetadata;
pub use crate::events::session_telemetry::SessionTelemetry;
pub use crate::events::session_telemetry::SessionTelemetryMetadata;
pub use crate::metrics::runtime_metrics::RuntimeMetricTotals;
pub use crate::metrics::runtime_metrics::RuntimeMetricsSummary;
pub use crate::metrics::timer::Timer;
pub use crate::metrics::*;
pub use crate::provider::OtelProvider;
pub use crate::trace_context::context_from_w3c_trace_context;
pub use crate::trace_context::current_span_trace_id;
pub use crate::trace_context::current_span_w3c_trace_context;
pub use crate::trace_context::inject_span_w3c_trace_headers;
pub use crate::trace_context::set_parent_from_context;
pub use crate::trace_context::set_parent_from_w3c_trace_context;
pub use crate::trace_context::span_w3c_trace_context;
pub use crate::trace_context::traceparent_context_from_env;
pub use crate::trace_context::validate_tracestate_entries;
pub use crate::trace_context::validate_tracestate_member;
pub use xedoc_utils_string::sanitize_metric_tag_value;

#[derive(Debug, Clone, Serialize, Display)]
#[serde(rename_all = "snake_case")]
pub enum ToolDecisionSource {
    Config,
    User,
}

/// Coarsens the authentication domain into the dimensions used by telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
pub enum TelemetryAuthMode {
    ApiKey,
    Chatgpt,
}

impl From<AuthMode> for TelemetryAuthMode {
    fn from(mode: AuthMode) -> Self {
        match mode {
            AuthMode::ApiKey | AuthMode::BedrockApiKey => Self::ApiKey,
            AuthMode::Chatgpt
            | AuthMode::ChatgptAuthTokens
            | AuthMode::Headers
            | AuthMode::AgentIdentity
            | AuthMode::PersonalAccessToken => Self::Chatgpt,
        }
    }
}

/// Start a metrics timer using the globally installed metrics client.
pub fn start_global_timer(name: &str, tags: &[(&str, &str)]) -> MetricsResult<Timer> {
    let Some(metrics) = crate::metrics::global() else {
        return Err(MetricsError::ExporterDisabled);
    };
    metrics.start_timer(name, tags)
}

/// Emit baseline tool-output reduction measurements when metrics are enabled.
pub fn record_tool_output_reduction(
    tokens_in: i64,
    tokens_out: i64,
    duration_us: u64,
    tool: &str,
    kind: &str,
    level: &str,
    reducer: &str,
) {
    let Some(metrics) = crate::metrics::global() else {
        return;
    };
    let tags = [
        ("tool", tool),
        ("kind", kind),
        ("level", level),
        ("reducer", reducer),
    ];
    let _ = metrics.counter(TOOL_OUTPUT_REDUCTION_TOKENS_IN_METRIC, tokens_in, &tags);
    let _ = metrics.counter(TOOL_OUTPUT_REDUCTION_TOKENS_OUT_METRIC, tokens_out, &tags);
    let _ = metrics.histogram(
        TOOL_OUTPUT_REDUCTION_DURATION_US_METRIC,
        duration_us.min(i64::MAX as u64) as i64,
        &tags,
    );
}

/// Emit a counter when a model reads a previously spilled output.
pub fn record_tool_output_retrieval(tool: &str) {
    let Some(metrics) = crate::metrics::global() else {
        return;
    };
    let _ = metrics.counter(TOOL_OUTPUT_RETRIEVAL_METRIC, 1, &[("tool", tool)]);
}
