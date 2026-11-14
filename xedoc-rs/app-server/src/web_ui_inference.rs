//! Read-only inference snapshots for host-launched plugin panels.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;
use xedoc_app_server_protocol::Model;
use xedoc_protocol::ThreadId;
use xedoc_protocol::openai_models::ReasoningEffort;

use super::SessionState;
use super::authorized_headers;
use super::secured;

const SNAPSHOT_LIMIT: usize = 1024 * 1024;
const MODEL_LIMIT: usize = 1000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InferenceSnapshot {
    thread_id: String,
    model: String,
    model_provider: String,
    reasoning_effort: Option<ReasoningEffort>,
    models: Vec<Model>,
}

pub(super) async fn snapshot(
    State(state): State<Arc<SessionState>>,
    Path((token, requested_thread)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if token != state.browser_token || !authorized_headers(&state, &headers) {
        return secured(StatusCode::FORBIDDEN);
    }
    let primary_thread = {
        let frame = state.frame.borrow();
        match frame.as_deref() {
            None if !state.frame_received.load(Ordering::SeqCst) => state.thread_id.clone(),
            None => return secured(StatusCode::SERVICE_UNAVAILABLE),
            Some(frame) => {
                let Ok(frame) = serde_json::from_str::<serde_json::Value>(frame) else {
                    return secured(StatusCode::SERVICE_UNAVAILABLE);
                };
                let Some(primary) = frame["primaryThreadId"].as_str() else {
                    return secured(StatusCode::SERVICE_UNAVAILABLE);
                };
                primary.to_owned()
            }
        }
    };
    if requested_thread != primary_thread {
        return secured((
            StatusCode::CONFLICT,
            "The main session changed; reopen the plugin panel",
        ));
    }
    let Ok(thread_id) = ThreadId::from_string(&primary_thread) else {
        return secured(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Ok(thread) = state.threads.get_thread(thread_id).await else {
        return secured(StatusCode::SERVICE_UNAVAILABLE);
    };
    let settings = thread.config_snapshot().await;
    let Ok(presets) = state.threads.get_models_manager().try_list_models() else {
        return secured((
            StatusCode::SERVICE_UNAVAILABLE,
            "Xedoc's cached model catalog is busy; retry opening the panel",
        ));
    };
    if presets.is_empty() || presets.len() > MODEL_LIMIT {
        return secured(StatusCode::SERVICE_UNAVAILABLE);
    }
    if !presets.iter().any(|preset| {
        preset.model == settings.model && preset.provider_id == settings.model_provider_id
    }) {
        return secured((
            StatusCode::SERVICE_UNAVAILABLE,
            "The active provider/model is missing from Xedoc's cached catalog",
        ));
    }
    let snapshot = InferenceSnapshot {
        thread_id: primary_thread,
        model: settings.model,
        model_provider: settings.model_provider_id,
        reasoning_effort: settings.reasoning_effort,
        models: presets
            .into_iter()
            .map(crate::models::model_from_preset)
            .collect(),
    };
    let Ok(body) = serde_json::to_vec(&snapshot) else {
        return secured(StatusCode::SERVICE_UNAVAILABLE);
    };
    if body.len() > SNAPSHOT_LIMIT {
        return secured(StatusCode::SERVICE_UNAVAILABLE);
    }
    secured((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    ))
}
