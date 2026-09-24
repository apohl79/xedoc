//! Script-owned model-router report rendering.

use serde_json::Value;
use tokio_util::sync::CancellationToken;
use xedoc_script_protocol::ReportDocument;

use crate::config::Config;
use crate::model_router_script_host::ModelRouterScriptHost;

/// Renders one bounded raw model-router report through the configured router script.
pub async fn render(config: &Config, report: Value) -> Result<ReportDocument, String> {
    let host = ModelRouterScriptHost::from_config(config)
        .ok_or_else(|| "model-router script is not configured".to_string())?;
    host.render_report(report, CancellationToken::new())
        .await
        .map_err(|failure| failure.diagnostic())
}
