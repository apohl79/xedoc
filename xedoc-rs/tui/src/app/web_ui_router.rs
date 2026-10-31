//! Routing-table shortcuts through the router's existing script-owned settings.

use color_eyre::Result;
use color_eyre::eyre::eyre;
use serde::Serialize;
use serde_json::json;
use xedoc_app_server_client::AppServerRequestHandle;
use xedoc_app_server_protocol::ExtensionInteractionOutcome;
use xedoc_app_server_protocol::ExtensionInteractionRequestResponse;
use xedoc_app_server_protocol::ExtensionInteractionSelectedAction;
use xedoc_app_server_protocol::ExtensionInteractionSurface;
use xedoc_app_server_protocol::ModelRouterSettingsInteraction;
use xedoc_protocol::ThreadId;

use super::App;
use crate::app_server_session::AppServerSession;
use crate::tui::Tui;

impl App {
    pub(super) async fn update_web_ui_routing_tables(
        &mut self,
        tui: &mut Tui,
        app_server: &AppServerSession,
        thread_id: ThreadId,
        table: Option<&str>,
    ) {
        let result = routing_tables(
            app_server.request_handle(),
            self.chat_widget.thread_id().unwrap_or(thread_id),
            table,
        )
        .await;
        match result {
            Ok(tables) => {
                if let Some(session) = &mut tui.web_ui {
                    session.routing_tables = tables;
                    session.last_publish = None;
                }
            }
            Err(error) => self
                .chat_widget
                .add_error_message(format!("Could not load routing tables: {error:#}")),
        }
    }
}

#[derive(Default, Serialize)]
pub(crate) struct RoutingTables {
    pub(crate) current: Option<String>,
    pub(crate) options: Vec<TableOption>,
}

#[derive(Serialize)]
pub(crate) struct TableOption {
    id: String,
    label: String,
}

async fn routing_tables(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    selected: Option<&str>,
) -> Result<RoutingTables> {
    let opened =
        crate::config_update::open_model_router_settings(handle.clone(), Some(thread_id)).await?;
    let Some(root) = opened.interaction else {
        return if selected.is_none() {
            Ok(RoutingTables::default())
        } else {
            Err(eyre!(opened.error.unwrap_or_else(|| {
                "Router settings are unavailable".to_string()
            })))
        };
    };
    let ExtensionInteractionSurface::Menu { items, .. } = &root.surface else {
        return Ok(RoutingTables::default());
    };
    let Some(shortcut) = items
        .iter()
        .find(|item| item.action.id == "open-table-picker" && item.disabled != Some(true))
    else {
        return Ok(RoutingTables::default());
    };
    let mut picker = respond(handle.clone(), thread_id, &root, &shortcut.action.id).await?;
    if picker.continuation != "settings:table-picker" {
        return Err(eyre!("Router settings changed; reopen the table menu"));
    }
    if let Some(selected) = selected {
        let ExtensionInteractionSurface::Menu { items, .. } = &picker.surface else {
            return Err(eyre!("Routing tables are unavailable"));
        };
        let option = items
            .iter()
            .find(|item| {
                item.id == selected
                    && item.disabled != Some(true)
                    && item.action.id.starts_with("select-table-")
            })
            .ok_or_else(|| eyre!("Choose an available routing table"))?;
        picker = respond(handle, thread_id, &picker, &option.action.id).await?;
    }
    if picker.continuation != "settings:table-picker" {
        return Err(eyre!("Router settings changed; reopen the table menu"));
    }
    let ExtensionInteractionSurface::Menu { items, .. } = picker.surface else {
        return Err(eyre!("Routing tables are unavailable"));
    };
    let mut tables = RoutingTables::default();
    for item in items
        .into_iter()
        .filter(|item| item.disabled != Some(true) && item.action.id.starts_with("select-table-"))
    {
        if item.current == Some(true) {
            tables.current = Some(item.id.clone());
        }
        tables.options.push(TableOption {
            id: item.id,
            label: item.label,
        });
    }
    Ok(tables)
}

async fn respond(
    handle: AppServerRequestHandle,
    thread_id: ThreadId,
    interaction: &ModelRouterSettingsInteraction,
    action_id: &str,
) -> Result<ModelRouterSettingsInteraction> {
    let result = crate::config_update::respond_model_router_settings(
        handle,
        ExtensionInteractionRequestResponse {
            extension_id: "model-router".to_string(),
            interaction_id: interaction.interaction_id.clone(),
            continuation: interaction.continuation.clone(),
            state_revision: Some(interaction.state_revision.clone()),
            outcome: ExtensionInteractionOutcome::Accepted,
            action: Some(ExtensionInteractionSelectedAction {
                id: action_id.to_string(),
            }),
            values: json!({}),
        },
        /*jev_api_key*/ None,
        Some(thread_id),
    )
    .await?;
    result.interaction.ok_or_else(|| {
        eyre!(
            result
                .error
                .unwrap_or_else(|| "Router settings are unavailable".to_string())
        )
    })
}
