//! Root-thread scoped remote session projections.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;
use tokio::sync::Mutex;
use uuid::Uuid;
use xedoc_app_server_protocol::RemoteSessionControlAction;
use xedoc_app_server_protocol::RemoteSessionStatus;
use xedoc_app_server_protocol::RemoteSessionSummary;
use xedoc_app_server_protocol::RemoteSessionUpdateParams;
use xedoc_protocol::ThreadId;

const MAX_IDENTIFIER_CHARS: usize = 128;
const MAX_ACTIVITY_SUMMARY_CHARS: usize = 64;
const MAX_OUTPUT_DELTA_BYTES: usize = 32 * 1024;
const DEFAULT_LIST_LIMIT: usize = 100;
const MAX_LIST_LIMIT: usize = 100;

/// Owns opaque local identities for remote session projections.
#[derive(Clone, Default)]
pub(crate) struct RemoteSessionRegistry {
    state: Arc<Mutex<RemoteSessionRegistryState>>,
}

#[derive(Default)]
struct RemoteSessionRegistryState {
    remote_session_ids_by_identity: HashMap<(ThreadId, String, String), String>,
    entries_by_id: HashMap<String, RemoteSessionEntry>,
}

struct RemoteSessionEntry {
    root_thread_id: ThreadId,
    summary: RemoteSessionSummary,
    activity_cursor: Option<String>,
}

/// One bounded page of remote session projections.
pub(crate) struct RemoteSessionPage {
    pub(crate) data: Vec<RemoteSessionSummary>,
    pub(crate) next_cursor: Option<String>,
}

/// A changed projection and its optional output delta.
pub(crate) struct RemoteSessionProjectionUpdate {
    pub(crate) summary: RemoteSessionSummary,
    pub(crate) output_delta: Option<String>,
}

impl RemoteSessionRegistry {
    /// Registers or returns one remote identity for a root thread.
    pub(crate) async fn register(
        &self,
        root_thread_id: ThreadId,
        host_id: String,
        remote_thread_id: String,
        host_name: Option<String>,
    ) -> Result<RemoteSessionSummary, String> {
        validate_identifier("host id", &host_id)?;
        validate_identifier("remote thread id", &remote_thread_id)?;
        if let Some(host_name) = host_name.as_deref() {
            validate_identifier("host name", host_name)?;
        }
        let mut state = self.state.lock().await;
        let identity = (root_thread_id, host_id.clone(), remote_thread_id.clone());
        if let Some(remote_session_id) = state.remote_session_ids_by_identity.get(&identity) {
            return state
                .entries_by_id
                .get(remote_session_id)
                .map(|entry| entry.summary.clone())
                .ok_or_else(|| "remote session registry is inconsistent".to_string());
        }
        let remote_session_id = Uuid::now_v7().to_string();
        let summary = RemoteSessionSummary {
            remote_session_id: remote_session_id.clone(),
            host_id,
            remote_thread_id,
            workspace_id: None,
            host_name,
            host_role: None,
            status: RemoteSessionStatus::Idle,
            active_turn_id: None,
            activity_summary: None,
            output_cursor: None,
        };
        state
            .remote_session_ids_by_identity
            .insert(identity, remote_session_id.clone());
        state.entries_by_id.insert(
            remote_session_id,
            RemoteSessionEntry {
                root_thread_id,
                summary: summary.clone(),
                activity_cursor: None,
            },
        );
        Ok(summary)
    }

    /// Lists all projections owned by one root thread.
    pub(crate) async fn list(
        &self,
        root_thread_id: ThreadId,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<RemoteSessionPage, String> {
        if let Some(cursor) = cursor {
            validate_identifier("remote session cursor", cursor)?;
        }
        let limit = limit
            .map(|limit| usize::try_from(limit).unwrap_or(MAX_LIST_LIMIT))
            .unwrap_or(DEFAULT_LIST_LIMIT);
        if limit == 0 || limit > MAX_LIST_LIMIT {
            return Err("remote session list limit is invalid".to_string());
        }
        let state = self.state.lock().await;
        let mut remote_sessions = state
            .entries_by_id
            .values()
            .filter(|entry| entry.root_thread_id == root_thread_id)
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        remote_sessions.sort_by(|left, right| {
            left.remote_session_id
                .cmp(&right.remote_session_id)
                .then_with(|| left.host_id.cmp(&right.host_id))
        });
        if let Some(cursor) = cursor {
            remote_sessions.retain(|session| session.remote_session_id.as_str() > cursor);
        }
        let has_more = remote_sessions.len() > limit;
        remote_sessions.truncate(limit);
        let next_cursor = has_more.then(|| {
            remote_sessions
                .last()
                .expect("non-empty page when more remote sessions exist")
                .remote_session_id
                .clone()
        });
        Ok(RemoteSessionPage {
            data: remote_sessions,
            next_cursor,
        })
    }

    /// Reads one projection summary.
    pub(crate) async fn read(
        &self,
        root_thread_id: ThreadId,
        remote_session_id: &str,
    ) -> Result<RemoteSessionSummary, String> {
        validate_identifier("remote session id", remote_session_id)?;
        let state = self.state.lock().await;
        let entry = entry_for_root(&state, root_thread_id, remote_session_id)?;
        Ok(entry.summary.clone())
    }

    /// Applies a trusted extension update.
    pub(crate) async fn update(
        &self,
        root_thread_id: ThreadId,
        params: RemoteSessionUpdateParams,
    ) -> Result<RemoteSessionProjectionUpdate, String> {
        validate_update(&params)?;
        let mut state = self.state.lock().await;
        let entry = entry_for_root_mut(&mut state, root_thread_id, &params.remote_session_id)?;
        let accepts_update = accepts_update(entry, &params);
        if accepts_update {
            entry.summary.status = params.status;
            if let Some(activity_summary) = params.activity_summary {
                entry.summary.activity_summary = Some(activity_summary);
            } else if is_terminal_status(params.status) {
                entry.summary.activity_summary = None;
            }
            if let Some(remote_turn_id) = params.remote_turn_id {
                entry.summary.active_turn_id = Some(remote_turn_id);
            }
        }
        if let Some(workspace_id) = params.workspace_id {
            entry.summary.workspace_id = Some(workspace_id);
        }
        if let Some(host_name) = params.host_name {
            entry.summary.host_name = Some(host_name);
        }
        if let Some(host_role) = params.host_role {
            entry.summary.host_role = Some(host_role);
        }
        let output_delta = match (params.output_delta, params.output_cursor) {
            (Some(delta), Some(cursor)) if entry.activity_cursor.as_deref() != Some(&cursor) => {
                entry.activity_cursor = Some(cursor);
                Some(delta)
            }
            (Some(_), Some(_)) => None,
            (None, None) => None,
            _ => return Err("output delta and cursor must be provided together".to_string()),
        };
        Ok(RemoteSessionProjectionUpdate {
            summary: entry.summary.clone(),
            output_delta,
        })
    }

    /// Applies a broker control response to a registered projection.
    pub(crate) async fn apply_control_result(
        &self,
        root_thread_id: ThreadId,
        remote_session_id: &str,
        action: RemoteSessionControlAction,
        result: &JsonValue,
    ) -> Result<RemoteSessionProjectionUpdate, String> {
        let mut state = self.state.lock().await;
        let entry = entry_for_root_mut(&mut state, root_thread_id, remote_session_id)?;
        let object = result
            .as_object()
            .ok_or_else(|| "remote-agent control result must be an object".to_string())?;
        let output_delta = apply_broker_result(entry, object)?;
        match action {
            RemoteSessionControlAction::Attach => {
                if entry.summary.status == RemoteSessionStatus::Detached {
                    entry.summary.status = RemoteSessionStatus::Idle;
                }
            }
            RemoteSessionControlAction::Send | RemoteSessionControlAction::Steer => {
                entry.summary.status = RemoteSessionStatus::Running;
                entry.summary.activity_summary = Some("Remote session is running.".to_string());
            }
            RemoteSessionControlAction::Cancel => {
                entry.summary.status = RemoteSessionStatus::Cancelled;
                entry.summary.activity_summary = Some("Remote session cancelled".to_string());
            }
            RemoteSessionControlAction::Detach => {
                entry.summary.status = RemoteSessionStatus::Detached;
            }
            RemoteSessionControlAction::Read => {}
        }
        Ok(RemoteSessionProjectionUpdate {
            summary: entry.summary.clone(),
            output_delta,
        })
    }
}

fn entry_for_root<'a>(
    state: &'a RemoteSessionRegistryState,
    root_thread_id: ThreadId,
    remote_session_id: &str,
) -> Result<&'a RemoteSessionEntry, String> {
    let entry = state
        .entries_by_id
        .get(remote_session_id)
        .ok_or_else(|| "remote session is not registered for this root thread".to_string())?;
    (entry.root_thread_id == root_thread_id)
        .then_some(entry)
        .ok_or_else(|| "remote session is not registered for this root thread".to_string())
}

fn entry_for_root_mut<'a>(
    state: &'a mut RemoteSessionRegistryState,
    root_thread_id: ThreadId,
    remote_session_id: &str,
) -> Result<&'a mut RemoteSessionEntry, String> {
    let entry = state
        .entries_by_id
        .get_mut(remote_session_id)
        .ok_or_else(|| "remote session is not registered for this root thread".to_string())?;
    (entry.root_thread_id == root_thread_id)
        .then_some(entry)
        .ok_or_else(|| "remote session is not registered for this root thread".to_string())
}

fn validate_update(params: &RemoteSessionUpdateParams) -> Result<(), String> {
    validate_identifier("remote session id", &params.remote_session_id)?;
    for (field, value) in [
        ("output cursor", params.output_cursor.as_deref()),
        ("remote turn id", params.remote_turn_id.as_deref()),
        ("workspace id", params.workspace_id.as_deref()),
        ("host name", params.host_name.as_deref()),
    ] {
        if let Some(value) = value {
            validate_identifier(field, value)?;
        }
    }
    if let Some(activity_summary) = params.activity_summary.as_deref() {
        validate_activity_summary(activity_summary)?;
    }
    if let Some(output_delta) = params.output_delta.as_deref() {
        if output_delta.is_empty() || output_delta.len() > MAX_OUTPUT_DELTA_BYTES {
            return Err("output delta is invalid".to_string());
        }
    }
    if params.output_delta.is_some() != params.output_cursor.is_some() {
        return Err("output delta and cursor must be provided together".to_string());
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().count() > MAX_IDENTIFIER_CHARS {
        return Err(format!("{field} is invalid"));
    }
    Ok(())
}

fn validate_activity_summary(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().count() > MAX_ACTIVITY_SUMMARY_CHARS {
        return Err("activity summary is invalid".to_string());
    }
    Ok(())
}

fn apply_broker_result(
    entry: &mut RemoteSessionEntry,
    object: &serde_json::Map<String, JsonValue>,
) -> Result<Option<String>, String> {
    if let Some(workspace_id) = optional_identifier(object.get("workspaceId"), "workspace id")? {
        entry.summary.workspace_id = Some(workspace_id);
    }
    if let Some(host_name) = optional_identifier(object.get("hostName"), "host name")? {
        entry.summary.host_name = Some(host_name);
    }
    let remote_turn_id = optional_identifier(
        object.get("activeTurnId").or_else(|| object.get("turnId")),
        "turn id",
    )?;
    let new_turn = remote_turn_id.as_deref().is_some_and(|remote_turn_id| {
        entry.summary.active_turn_id.as_deref() != Some(remote_turn_id)
    });
    if let Some(remote_turn_id) = remote_turn_id {
        entry.summary.active_turn_id = Some(remote_turn_id);
    }
    let broker_status = optional_status(object.get("status"))?;
    let accepts_activity = broker_status
        .map(|status| {
            let accepted = apply_status(entry, status, new_turn);
            if accepted && is_terminal_status(status) && object.get("activitySummary").is_none() {
                entry.summary.activity_summary = None;
            }
            accepted
        })
        .unwrap_or(!is_terminal_status(entry.summary.status));
    if optional_bool(object.get("isRunning"), "is running")? == Some(true)
        && !is_terminal_status(entry.summary.status)
    {
        entry.summary.status = RemoteSessionStatus::Running;
    }
    if object.contains_key("nextCursor") {
        entry.summary.output_cursor = optional_identifier(object.get("nextCursor"), "next cursor")?;
    }
    if accepts_activity
        && let Some(activity_summary) = optional_activity_summary(object.get("activitySummary"))?
    {
        entry.summary.activity_summary = Some(activity_summary);
    }

    let mut output_delta = optional_output_text(object.get("outputText"))?;
    if let Some(events) = object.get("events") {
        let events = events
            .as_array()
            .ok_or_else(|| "remote-agent events are invalid".to_string())?;
        for event in events {
            let event = event
                .as_object()
                .ok_or_else(|| "remote-agent event is invalid".to_string())?;
            let event_turn_id = optional_identifier(event.get("turnId"), "turn id")?;
            let new_event_turn = event_turn_id
                .as_deref()
                .is_some_and(|turn_id| entry.summary.active_turn_id.as_deref() != Some(turn_id));
            if let Some(event_turn_id) = event_turn_id {
                entry.summary.active_turn_id = Some(event_turn_id);
            }
            let event_status = optional_status(event.get("status"))?;
            let accepts_event_activity = event_status
                .map(|status| {
                    let accepted = apply_status(entry, status, new_event_turn);
                    if accepted
                        && is_terminal_status(status)
                        && event.get("activitySummary").is_none()
                    {
                        entry.summary.activity_summary = None;
                    }
                    accepted
                })
                .unwrap_or(!is_terminal_status(entry.summary.status));
            if accepts_event_activity
                && let Some(activity_summary) =
                    optional_activity_summary(event.get("activitySummary"))?
            {
                entry.summary.activity_summary = Some(activity_summary);
            }
            if let Some(cursor) = optional_identifier(event.get("outputCursor"), "output cursor")? {
                entry.activity_cursor = Some(cursor);
            }
            let event_output = if event.get("type").and_then(JsonValue::as_str) == Some("activity")
            {
                None
            } else {
                optional_output_text(event.get("outputDelta").or_else(|| event.get("text")))?
            };
            if let Some(event_output) = event_output {
                append_output_delta(&mut output_delta, event_output);
            }
        }
    }
    if let Some(nested_result) = object.get("result") {
        let nested_result = nested_result
            .as_object()
            .ok_or_else(|| "remote-agent nested result is invalid".to_string())?;
        if let Some(nested_output_delta) = apply_broker_result(entry, nested_result)? {
            append_output_delta(&mut output_delta, nested_output_delta);
        }
    }
    Ok(output_delta)
}

fn apply_status(
    entry: &mut RemoteSessionEntry,
    status: RemoteSessionStatus,
    new_turn: bool,
) -> bool {
    if !is_terminal_status(entry.summary.status)
        || status == entry.summary.status
        || (status == RemoteSessionStatus::Running && new_turn)
    {
        entry.summary.status = status;
        true
    } else {
        false
    }
}

fn accepts_update(entry: &RemoteSessionEntry, params: &RemoteSessionUpdateParams) -> bool {
    if is_terminal_status(entry.summary.status) && params.status == RemoteSessionStatus::Running {
        return params
            .remote_turn_id
            .as_deref()
            .is_some_and(|remote_turn_id| {
                entry.summary.active_turn_id.as_deref() != Some(remote_turn_id)
            });
    }
    if is_terminal_status(params.status)
        && entry.summary.status == RemoteSessionStatus::Running
        && let Some(active_turn_id) = entry.summary.active_turn_id.as_deref()
        && params.remote_turn_id.as_deref() != Some(active_turn_id)
    {
        return false;
    }
    !is_terminal_status(entry.summary.status) || params.status == entry.summary.status
}

fn append_output_delta(output_delta: &mut Option<String>, next: String) {
    match output_delta {
        Some(previous) => {
            if !previous.ends_with('\n') && !next.starts_with('\n') {
                previous.push('\n');
            }
            previous.push_str(&next);
        }
        None => *output_delta = Some(next),
    }
}

fn optional_identifier(value: Option<&JsonValue>, field: &str) -> Result<Option<String>, String> {
    let value = optional_string(value, field)?;
    if let Some(value) = value.as_deref() {
        validate_identifier(field, value)?;
    }
    Ok(value)
}

fn optional_activity_summary(value: Option<&JsonValue>) -> Result<Option<String>, String> {
    let value = optional_string(value, "activity summary")?;
    if let Some(value) = value.as_deref() {
        validate_activity_summary(value)?;
    }
    Ok(value)
}

fn optional_output_text(value: Option<&JsonValue>) -> Result<Option<String>, String> {
    let value = optional_string(value, "output text")?;
    Ok(value)
}

fn optional_status(value: Option<&JsonValue>) -> Result<Option<RemoteSessionStatus>, String> {
    optional_string(value, "status")?
        .map(|value| status_from_broker(&value))
        .transpose()
}

fn optional_bool(value: Option<&JsonValue>, field: &str) -> Result<Option<bool>, String> {
    value
        .map(|value| value.as_bool().ok_or_else(|| format!("{field} is invalid")))
        .transpose()
}

fn optional_string(value: Option<&JsonValue>, field: &str) -> Result<Option<String>, String> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{field} is invalid"))
            .map(Some),
    }
}

fn status_from_broker(value: &str) -> Result<RemoteSessionStatus, String> {
    match value {
        "idle" | "pending" | "unknown" => Ok(RemoteSessionStatus::Idle),
        "active" | "inProgress" | "running" => Ok(RemoteSessionStatus::Running),
        "completed" => Ok(RemoteSessionStatus::Completed),
        "failed" | "error" | "systemError" => Ok(RemoteSessionStatus::Failed),
        "cancelled" | "interrupted" | "aborted" => Ok(RemoteSessionStatus::Cancelled),
        "expired" | "notLoaded" => Ok(RemoteSessionStatus::Expired),
        "detached" => Ok(RemoteSessionStatus::Detached),
        _ => Err("remote session status is invalid".to_string()),
    }
}

fn is_terminal_status(status: RemoteSessionStatus) -> bool {
    matches!(
        status,
        RemoteSessionStatus::Completed
            | RemoteSessionStatus::Failed
            | RemoteSessionStatus::Cancelled
            | RemoteSessionStatus::Expired
    )
}
