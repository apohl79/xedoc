//! Shared ChatWidget fixtures for tests in dependent crates.

use super::*;
use crate::test_support::PathBufExt;
use crate::test_support::TEST_MODEL_PRESETS;
use crate::test_support::session_source_cli;
use crate::test_support::test_path_display;
use codex_config::ConfigLayerStack;
use codex_models_manager::test_support::construct_model_info_offline_for_tests;
use codex_models_manager::test_support::get_model_offline_for_tests;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use serde_json::json;
use tokio::sync::mpsc::unbounded_channel;

pub(crate) async fn test_config() -> Config {
    let codex_home = tempfile::Builder::new()
        .prefix("chatwidget-tests-")
        .tempdir()
        .expect("tempdir")
        .keep();
    let mut config =
        Config::load_default_with_cli_overrides_for_codex_home(codex_home.clone(), Vec::new())
            .await
            .expect("config");
    config.codex_home = codex_home.abs();
    config.sqlite_home = codex_home.clone();
    config.log_dir = codex_home.join("log");
    config.cwd = PathBuf::from(test_path_display("/tmp/project")).abs();
    config.config_layer_stack = ConfigLayerStack::default();
    config.startup_warnings.clear();
    config
}

pub(crate) fn test_session_telemetry(config: &Config, model: &str) -> SessionTelemetry {
    let model_info =
        construct_model_info_offline_for_tests(model, &config.to_models_manager_config());
    SessionTelemetry::new(
        ThreadId::new(),
        model,
        model_info.slug.as_str(),
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test_originator".to_string(),
        /*log_user_prompts*/ false,
        "test".to_string(),
        session_source_cli(),
    )
}

pub(crate) fn test_model_catalog(_config: &Config) -> Arc<ModelCatalog> {
    Arc::new(ModelCatalog::new(TEST_MODEL_PRESETS.clone()))
}

pub(crate) async fn make_chatwidget_manual(
    model_override: Option<&str>,
) -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<AppCommand>,
) {
    make_chatwidget_manual_with_auth(
        model_override,
        /*has_chatgpt_account*/ false,
        /*has_codex_backend_auth*/ false,
        FrameRequester::test_dummy(),
    )
    .await
}

pub(crate) async fn make_chatwidget_manual_with_auth(
    model_override: Option<&str>,
    has_chatgpt_account: bool,
    has_codex_backend_auth: bool,
    frame_requester: FrameRequester,
) -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<AppCommand>,
) {
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(tx_raw);
    let (op_tx, op_rx) = unbounded_channel::<AppCommand>();
    let mut config = test_config().await;
    let resolved_model = model_override
        .map(str::to_owned)
        .unwrap_or_else(|| get_model_offline_for_tests(config.model.as_deref()));
    if let Some(model) = model_override {
        config.model = Some(model.to_string());
    }
    let session_telemetry = test_session_telemetry(&config, resolved_model.as_str());
    let model_catalog = test_model_catalog(&config);
    let common = ChatWidgetInit {
        config,
        frame_requester,
        app_event_tx,
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: false,
        has_chatgpt_account,
        has_codex_backend_auth,
        model_catalog,
        feedback: codex_feedback::CodexFeedback::new(),
        is_first_run: true,
        status_account_display: None,
        runtime_model_provider_base_url: None,
        initial_plan_type: None,
        model: Some(resolved_model.clone()),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        session_telemetry,
    };
    let mut widget = ChatWidget::new_with_op_target(common, CodexOpTarget::Direct(op_tx));
    widget.transcript.active_cell = None;
    widget.transcript.active_cell_revision = 0;
    widget.normal_placeholder_text = "Ask Codex to do anything".to_string();
    widget.side_placeholder_text =
        "Check recently modified functions for compatibility".to_string();
    widget
        .bottom_pane
        .set_placeholder_text(widget.normal_placeholder_text.clone());
    widget.set_model(&resolved_model);
    (widget, rx, op_rx)
}

pub fn set_active_cell(chat: &mut ChatWidget, cell: Box<dyn HistoryCell>) {
    chat.transcript.active_cell = Some(cell);
}

pub fn set_chatgpt_auth(chat: &mut ChatWidget) {
    chat.has_chatgpt_account = true;
    chat.has_codex_backend_auth = true;
    chat.model_catalog = test_model_catalog(&chat.config);
}

fn test_model_info(slug: &str, priority: i32, supports_fast_mode: bool) -> ModelInfo {
    let mut service_tiers = Vec::new();
    if supports_fast_mode {
        service_tiers.push(json!({
            "id": ServiceTier::Fast.request_value(),
            "name": "fast",
            "description": "Fastest inference with increased plan usage"
        }));
    }
    serde_json::from_value(json!({
        "slug": slug,
        "display_name": slug,
        "description": format!("{slug} description"),
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [{"effort": "medium", "description": "medium"}],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": priority,
        "additional_speed_tiers": [],
        "service_tiers": service_tiers,
        "default_service_tier": null,
        "availability_nux": null,
        "upgrade": null,
        "base_instructions": "base instructions",
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272_000,
        "experimental_supported_tools": [],
    }))
    .expect("valid model info")
}

pub fn set_fast_mode_test_catalog(chat: &mut ChatWidget) {
    let models: Vec<ModelPreset> = ModelsResponse {
        models: vec![
            test_model_info(
                "gpt-5.4", /*priority*/ 0, /*supports_fast_mode*/ true,
            ),
            test_model_info(
                "gpt-5.2", /*priority*/ 1, /*supports_fast_mode*/ false,
            ),
        ],
    }
    .models
    .into_iter()
    .map(Into::into)
    .collect();

    chat.model_catalog = Arc::new(ModelCatalog::new(models));
}

pub async fn make_chatwidget_manual_with_sender() -> (
    ChatWidget,
    AppEventSender,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<AppCommand>,
) {
    let (widget, rx, op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let app_event_tx = widget.app_event_tx.clone();
    (widget, app_event_tx, rx, op_rx)
}

pub fn render_bottom_popup(chat: &ChatWidget, width: u16) -> String {
    let height = chat.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    chat.render(area, &mut buf);

    let mut lines: Vec<String> = (0..area.height)
        .map(|row| {
            let mut line = String::new();
            for col in 0..area.width {
                let symbol = buf[(area.x + col, area.y + row)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            line.trim_end().to_string()
        })
        .collect();

    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}
