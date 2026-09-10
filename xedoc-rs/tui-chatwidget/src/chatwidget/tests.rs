//! Exercises `ChatWidget` event handling and rendering invariants.
//!
//! These tests cover both app-server-native inputs and focused widget helpers. Many assertions are
//! snapshot-based so that layout regressions and status/header changes show up as stable,
//! reviewable diffs.

pub(super) use super::*;
pub(super) use crate::app_command::AppCommand as Op;
pub(super) use crate::app_event::AppEvent;
pub(super) use crate::app_event::ExitMode;
pub(super) use crate::app_event_sender::AppEventSender;
pub(super) use crate::approval_events::ApplyPatchApprovalRequestEvent;
pub(super) use crate::approval_events::ExecApprovalRequestEvent;
pub(super) use crate::bottom_pane::LocalImageAttachment;
pub(super) use crate::bottom_pane::MentionBinding;
pub(super) use crate::bottom_pane::QueuedInputAction;
pub(super) use crate::diff_model::FileChange;
pub(super) use crate::history_cell::UserHistoryCell;
pub(super) use crate::legacy_core::config::ConfigBuilder;
pub(super) use crate::model_catalog::ModelCatalog;
pub(super) use crate::test_backend::VT100Backend;
pub(super) use crate::test_support::PathBufExt;
pub(super) use crate::test_support::test_path_buf;
pub(super) use crate::test_support::test_path_display;
pub(super) use crate::token_usage::TokenUsage;
pub(super) use crate::token_usage::TokenUsageInfo;
pub(super) use crate::tui::FrameRequester;
pub(super) use assert_matches::assert_matches;
pub(super) use crossterm::event::KeyCode;
pub(super) use crossterm::event::KeyEvent;
pub(super) use crossterm::event::KeyModifiers;
pub(super) use insta::assert_snapshot;
pub(super) use serde_json::json;
#[cfg(target_os = "windows")]
pub(super) use serial_test::serial;
pub(super) use std::collections::HashMap;
pub(super) use std::path::PathBuf;
pub(super) use tempfile::NamedTempFile;
pub(super) use tempfile::tempdir;
pub(super) use tokio::sync::mpsc::error::TryRecvError;
pub(super) use tokio::sync::mpsc::unbounded_channel;
pub(super) use toml::Value as TomlValue;
pub(super) use xedoc_app_server_protocol::AdditionalFileSystemPermissions as AppServerAdditionalFileSystemPermissions;
pub(super) use xedoc_app_server_protocol::AdditionalNetworkPermissions as AppServerAdditionalNetworkPermissions;
pub(super) use xedoc_app_server_protocol::AdditionalPermissionProfile as AppServerAdditionalPermissionProfile;
pub(super) use xedoc_app_server_protocol::CollabAgentState as AppServerCollabAgentState;
pub(super) use xedoc_app_server_protocol::CollabAgentStatus as AppServerCollabAgentStatus;
pub(super) use xedoc_app_server_protocol::CollabAgentTool as AppServerCollabAgentTool;
pub(super) use xedoc_app_server_protocol::CollabAgentToolCallStatus as AppServerCollabAgentToolCallStatus;
pub(super) use xedoc_app_server_protocol::CommandAction as AppServerCommandAction;
pub(super) use xedoc_app_server_protocol::CommandExecutionRequestApprovalParams as AppServerCommandExecutionRequestApprovalParams;
pub(super) use xedoc_app_server_protocol::CommandExecutionSource as ExecCommandSource;
pub(super) use xedoc_app_server_protocol::CommandExecutionSource as AppServerCommandExecutionSource;
pub(super) use xedoc_app_server_protocol::CommandExecutionStatus as AppServerCommandExecutionStatus;
pub(super) use xedoc_app_server_protocol::CompactionProgressNotification;
pub(super) use xedoc_app_server_protocol::ConfigWarningNotification;
pub(super) use xedoc_app_server_protocol::CreditsSnapshot;
pub(super) use xedoc_app_server_protocol::ErrorNotification;
pub(super) use xedoc_app_server_protocol::ExecPolicyAmendment;
pub(super) use xedoc_app_server_protocol::FileUpdateChange;
pub(super) use xedoc_app_server_protocol::HookCompletedNotification as AppServerHookCompletedNotification;
pub(super) use xedoc_app_server_protocol::HookEventName as AppServerHookEventName;
pub(super) use xedoc_app_server_protocol::HookExecutionMode as AppServerHookExecutionMode;
pub(super) use xedoc_app_server_protocol::HookHandlerType as AppServerHookHandlerType;
pub(super) use xedoc_app_server_protocol::HookOutputEntry as AppServerHookOutputEntry;
pub(super) use xedoc_app_server_protocol::HookOutputEntryKind as AppServerHookOutputEntryKind;
pub(super) use xedoc_app_server_protocol::HookRunStatus as AppServerHookRunStatus;
pub(super) use xedoc_app_server_protocol::HookRunSummary as AppServerHookRunSummary;
pub(super) use xedoc_app_server_protocol::HookScope as AppServerHookScope;
pub(super) use xedoc_app_server_protocol::HookStartedNotification as AppServerHookStartedNotification;
pub(super) use xedoc_app_server_protocol::ItemCompletedNotification;
pub(super) use xedoc_app_server_protocol::ItemStartedNotification;
pub(super) use xedoc_app_server_protocol::MarketplaceAddResponse;
pub(super) use xedoc_app_server_protocol::MarketplaceInterface;
pub(super) use xedoc_app_server_protocol::MarketplaceUpgradeErrorInfo;
pub(super) use xedoc_app_server_protocol::MarketplaceUpgradeResponse;
pub(super) use xedoc_app_server_protocol::McpServerStartupState;
pub(super) use xedoc_app_server_protocol::McpServerStatusDetail;
pub(super) use xedoc_app_server_protocol::McpServerStatusUpdatedNotification;
pub(super) use xedoc_app_server_protocol::ModelSafetyBufferingUpdatedNotification;
pub(super) use xedoc_app_server_protocol::ModelVerification as AppServerModelVerification;
pub(super) use xedoc_app_server_protocol::ModelVerificationNotification;
pub(super) use xedoc_app_server_protocol::NonSteerableTurnKind;
pub(super) use xedoc_app_server_protocol::PatchApplyStatus as AppServerPatchApplyStatus;
pub(super) use xedoc_app_server_protocol::PatchChangeKind;
pub(super) use xedoc_app_server_protocol::PermissionsRequestApprovalParams as AppServerPermissionsRequestApprovalParams;
pub(super) use xedoc_app_server_protocol::PluginAuthPolicy;
pub(super) use xedoc_app_server_protocol::PluginDetail;
pub(super) use xedoc_app_server_protocol::PluginInstallPolicy;
pub(super) use xedoc_app_server_protocol::PluginInterface;
pub(super) use xedoc_app_server_protocol::PluginListResponse;
pub(super) use xedoc_app_server_protocol::PluginMarketplaceEntry;
pub(super) use xedoc_app_server_protocol::PluginReadResponse;
pub(super) use xedoc_app_server_protocol::PluginSource;
pub(super) use xedoc_app_server_protocol::PluginSummary;
pub(super) use xedoc_app_server_protocol::RateLimitReachedType;
pub(super) use xedoc_app_server_protocol::RateLimitSnapshot;
pub(super) use xedoc_app_server_protocol::RateLimitWindow;
pub(super) use xedoc_app_server_protocol::ReasoningSummaryTextDeltaNotification;
pub(super) use xedoc_app_server_protocol::ReviewTarget;
pub(super) use xedoc_app_server_protocol::ServerNotification;
pub(super) use xedoc_app_server_protocol::SkillMetadata;
pub(super) use xedoc_app_server_protocol::SkillSummary;
pub(super) use xedoc_app_server_protocol::ThreadClosedNotification;
pub(super) use xedoc_app_server_protocol::ThreadItem as AppServerThreadItem;
pub(super) use xedoc_app_server_protocol::ToolRequestUserInputOption;
pub(super) use xedoc_app_server_protocol::ToolRequestUserInputParams;
pub(super) use xedoc_app_server_protocol::ToolRequestUserInputQuestion;
pub(super) use xedoc_app_server_protocol::Turn as AppServerTurn;
pub(super) use xedoc_app_server_protocol::TurnCompletedNotification;
pub(super) use xedoc_app_server_protocol::TurnError as AppServerTurnError;
pub(super) use xedoc_app_server_protocol::TurnStartedNotification;
pub(super) use xedoc_app_server_protocol::TurnStatus as AppServerTurnStatus;
pub(super) use xedoc_app_server_protocol::UserInput;
pub(super) use xedoc_app_server_protocol::UserInput as AppServerUserInput;
pub(super) use xedoc_app_server_protocol::WarningNotification;
pub(super) use xedoc_app_server_protocol::XedocErrorInfo;
pub(super) use xedoc_config::ConfigLayerStack;
pub(super) use xedoc_config::Constrained;
pub(super) use xedoc_config::ConstraintError;
pub(super) use xedoc_config::RequirementSource;
pub(super) use xedoc_config::types::Notifications;
pub(super) use xedoc_core_plugins::OPENAI_CURATED_MARKETPLACE_NAME;
pub(super) use xedoc_features::Feature;
pub(super) use xedoc_git_utils::CommitLogEntry;
pub(super) use xedoc_models_manager::test_support::get_model_offline_for_tests;
pub(super) use xedoc_otel::RuntimeMetricsSummary;
pub(super) use xedoc_protocol::ThreadId;
pub(super) use xedoc_protocol::account::PlanType;
pub(super) use xedoc_protocol::config_types::CollaborationMode;
pub(super) use xedoc_protocol::config_types::ModeKind;
pub(super) use xedoc_protocol::config_types::Personality;
pub(super) use xedoc_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
pub(super) use xedoc_protocol::config_types::ServiceTier;
pub(super) use xedoc_protocol::models::ActivePermissionProfile;
pub(super) use xedoc_protocol::models::FileSystemPermissions;
pub(super) use xedoc_protocol::models::MessagePhase;
pub(super) use xedoc_protocol::models::NetworkPermissions;
pub(super) use xedoc_protocol::models::PermissionProfile;
pub(super) use xedoc_protocol::openai_models::ModelPreset;
pub(super) use xedoc_protocol::openai_models::ReasoningEffortPreset;
pub(super) use xedoc_protocol::openai_models::default_input_modalities;
pub(super) use xedoc_protocol::plan_tool::PlanItemArg;
pub(super) use xedoc_protocol::plan_tool::StepStatus;
pub(super) use xedoc_protocol::plan_tool::UpdatePlanArgs;
pub(super) use xedoc_protocol::request_permissions::RequestPermissionProfile;
pub(super) use xedoc_protocol::user_input::TextElement;
pub(super) use xedoc_terminal_detection::Multiplexer;
pub(super) use xedoc_terminal_detection::TerminalInfo;
pub(super) use xedoc_terminal_detection::TerminalName;
pub(super) use xedoc_utils_absolute_path::AbsolutePathBuf;
pub(super) use xedoc_utils_approval_presets::builtin_approval_presets;
pub(super) use xedoc_utils_path_uri::LegacyAppPathString;

pub(super) fn chatwidget_snapshot_dir() -> PathBuf {
    let snapshot_file = xedoc_utils_cargo_bin::find_resource!(
        "src/chatwidget/snapshots/xedoc_tui__chatwidget__tests__mcp_startup_header_booting.snap"
    )
    .expect("snapshot file");
    snapshot_file
        .parent()
        .unwrap_or_else(|| panic!("snapshot file has no parent: {}", snapshot_file.display()))
        .to_path_buf()
}

macro_rules! assert_chatwidget_snapshot {
    ($name:expr, $value:expr $(,)?) => {{
        let mut settings = insta::Settings::clone_current();
        settings.set_prepend_module_to_snapshot(false);
        settings.set_snapshot_path(crate::chatwidget::tests::chatwidget_snapshot_dir());
        settings.bind(|| {
            insta::assert_snapshot!(format!("xedoc_tui__chatwidget__tests__{}", $name), $value);
        });
    }};
    ($name:expr, $value:expr, @$snapshot:literal $(,)?) => {{
        let mut settings = insta::Settings::clone_current();
        settings.set_prepend_module_to_snapshot(false);
        settings.set_snapshot_path(crate::chatwidget::tests::chatwidget_snapshot_dir());
        settings.bind(|| {
            insta::assert_snapshot!(
                format!("xedoc_tui__chatwidget__tests__{}", $name),
                &($value),
                @$snapshot
            );
        });
    }};
}

fn next_goal_draft(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    expected_thread_id: ThreadId,
) -> crate::goal_files::GoalDraft {
    loop {
        let event = rx.try_recv().expect("expected goal draft event");
        if let AppEvent::SetThreadGoalDraft {
            thread_id, draft, ..
        } = event
        {
            assert_eq!(thread_id, expected_thread_id);
            return draft;
        }
    }
}

mod app_server;
mod approval_requests;
mod composer_submission;
#[path = "tests/config_errors_tests.rs"]
mod config_errors;
mod exec_flow;
mod goal_menu;
mod goal_validation;
pub mod helpers;
mod history_replay;
mod mcp_startup;
mod permissions;
mod plan_mode;
#[path = "tests/plugin_catalog_tests.rs"]
mod plugin_catalog;
mod popups_and_settings;
mod review_mode;
mod side;
mod slash_commands;
mod status_and_layout;
mod status_command_tests;
mod status_surface_previews;
mod terminal_title;
#[path = "tests/tool_summary.rs"]
mod tool_summary;

pub use helpers::make_chatwidget_manual_with_sender;
pub use helpers::set_chatgpt_auth;
pub use helpers::set_fast_mode_test_catalog;
pub(super) use helpers::*;
