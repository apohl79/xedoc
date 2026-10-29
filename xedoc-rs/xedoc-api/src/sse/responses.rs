use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::common::SafetyBuffering;
use crate::common::SafetyBufferingTreatment;
use crate::error::ApiError;
use crate::inter_agent_trace;
use crate::rate_limits::parse_all_rate_limits;
use crate::safety_buffering::treatment_from_headers;
use crate::telemetry::SseTelemetry;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;
use xedoc_client::ByteStream;
use xedoc_client::StreamResponse;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::ModelVerification;
use xedoc_protocol::protocol::TokenUsage;
use xedoc_protocol::protocol::TurnModerationMetadataEvent;

const X_REASONING_INCLUDED_HEADER: &str = "x-reasoning-included";
const X_XEDOC_TURN_STATE_HEADER: &str = "x-codex-turn-state";
const OPENAI_MODEL_HEADER: &str = "openai-model";
const REQUEST_ID_HEADER: &str = "x-request-id";
const TRUSTED_ACCESS_FOR_CYBER_VERIFICATION: &str = "trusted_access_for_cyber";

pub fn spawn_response_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    let rate_limit_snapshots = parse_all_rate_limits(&stream_response.headers);
    let models_etag = stream_response
        .headers
        .get("X-Models-Etag")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let server_model = stream_response
        .headers
        .get(OPENAI_MODEL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let reasoning_included = stream_response
        .headers
        .get(X_REASONING_INCLUDED_HEADER)
        .is_some();
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let safety_buffering_treatment =
        treatment_from_headers(&stream_response.headers).unwrap_or_default();
    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get(X_XEDOC_TURN_STATE_HEADER)
            .and_then(|value| value.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_string());
    }
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        if let Some(model) = server_model {
            let _ = tx_event.send(Ok(ResponseEvent::ServerModel(model))).await;
        }
        for snapshot in rate_limit_snapshots {
            let _ = tx_event.send(Ok(ResponseEvent::RateLimits(snapshot))).await;
        }
        if let Some(etag) = models_etag {
            let _ = tx_event.send(Ok(ResponseEvent::ModelsEtag(etag))).await;
        }
        if reasoning_included {
            let _ = tx_event
                .send(Ok(ResponseEvent::ServerReasoningIncluded(true)))
                .await;
        }
        process_sse_with_treatment(
            stream_response.bytes,
            tx_event,
            idle_timeout,
            telemetry,
            safety_buffering_treatment,
        )
        .await;
    });

    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Error {
    r#type: Option<String>,
    code: Option<String>,
    message: Option<String>,
    plan_type: Option<String>,
    resets_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ResponseCompleted {
    id: String,
    #[serde(default)]
    usage: Option<ResponseCompletedUsage>,
    #[serde(default)]
    end_turn: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedUsage {
    input_tokens: i64,
    input_tokens_details: Option<ResponseCompletedInputTokensDetails>,
    output_tokens: i64,
    output_tokens_details: Option<ResponseCompletedOutputTokensDetails>,
    total_tokens: i64,
}

impl From<ResponseCompletedUsage> for TokenUsage {
    fn from(val: ResponseCompletedUsage) -> Self {
        let input_tokens_details = val.input_tokens_details.unwrap_or_default();
        TokenUsage {
            input_tokens: val.input_tokens,
            cached_input_tokens: input_tokens_details.cached_tokens,
            cache_write_input_tokens: input_tokens_details.cache_write_tokens,
            output_tokens: val.output_tokens,
            reasoning_output_tokens: val
                .output_tokens_details
                .map(|d| d.reasoning_tokens)
                .unwrap_or(0),
            total_tokens: val.total_tokens,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ResponseCompletedInputTokensDetails {
    cached_tokens: i64,
    #[serde(default)]
    cache_write_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ResponseCompletedOutputTokensDetails {
    reasoning_tokens: i64,
}

#[derive(Deserialize, Debug)]
pub struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) headers: Option<Value>,
    metadata: Option<Value>,
    response: Option<Value>,
    item: Option<Value>,
    item_id: Option<String>,
    call_id: Option<String>,
    delta: Option<String>,
    text: Option<String>,
    summary_index: Option<i64>,
    content_index: Option<i64>,
    safety_buffering: Option<Value>,
}

impl ResponsesStreamEvent {
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the effective model reported by the server, if present.
    ///
    /// Precedence:
    /// 1. `response.headers` for standard Responses stream events.
    /// 2. top-level `headers` for websocket metadata events.
    pub fn response_model(&self) -> Option<String> {
        let response_headers_model = self
            .response
            .as_ref()
            .and_then(|response| response.get("headers"))
            .and_then(header_openai_model_value_from_json);

        match response_headers_model {
            Some(model) => Some(model),
            None => self
                .headers
                .as_ref()
                .and_then(header_openai_model_value_from_json),
        }
    }

    pub(crate) fn turn_state(&self) -> Option<String> {
        if self.kind() != "response.metadata" {
            return None;
        }

        self.headers
            .as_ref()
            .and_then(header_turn_state_value_from_json)
    }

    pub(crate) fn model_verifications(&self) -> Option<Vec<ModelVerification>> {
        if self.kind() != "response.metadata" {
            return None;
        }

        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.get("openai_verification_recommendation"))
            .and_then(model_verifications_from_json_value)
    }

    pub(crate) fn turn_moderation_metadata(&self) -> Option<TurnModerationMetadataEvent> {
        if self.kind() != "response.metadata" {
            return None;
        }

        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.get("openai_chatgpt_moderation_metadata"))
            .cloned()
            .map(|metadata| TurnModerationMetadataEvent { metadata })
    }

    pub(crate) fn safety_buffering(
        &self,
        treatment: &SafetyBufferingTreatment,
    ) -> Option<SafetyBuffering> {
        let value = self.safety_buffering.as_ref()?;
        let retry_model_present = value.as_object()?.contains_key("retry_model");
        let mut buffering: SafetyBuffering = serde_json::from_value(value.clone()).ok()?;
        buffering.show_buffering_ui = true;
        if !retry_model_present {
            buffering.faster_model.clone_from(&treatment.faster_model);
        }
        Some(buffering)
    }
}

fn header_openai_model_value_from_json(value: &Value) -> Option<String> {
    let headers = value.as_object()?;
    headers.iter().find_map(|(name, value)| {
        if name.eq_ignore_ascii_case("openai-model") || name.eq_ignore_ascii_case("x-openai-model")
        {
            json_value_as_string(value)
        } else {
            None
        }
    })
}

fn header_turn_state_value_from_json(value: &Value) -> Option<String> {
    let headers = value.as_object()?;
    headers.iter().find_map(|(name, value)| {
        if name.eq_ignore_ascii_case(X_XEDOC_TURN_STATE_HEADER) {
            json_value_as_string(value)
        } else {
            None
        }
    })
}

fn model_verifications_from_json_value(value: &Value) -> Option<Vec<ModelVerification>> {
    let verifications = value
        .as_array()
        .map(|items| {
            let mut verifications = Vec::new();
            for verification in items
                .iter()
                .filter_map(Value::as_str)
                .filter_map(parse_model_verification)
            {
                if !verifications.contains(&verification) {
                    verifications.push(verification);
                }
            }
            verifications
        })
        .unwrap_or_default();

    if verifications.is_empty() {
        None
    } else {
        Some(verifications)
    }
}

fn parse_model_verification(value: &str) -> Option<ModelVerification> {
    match value {
        TRUSTED_ACCESS_FOR_CYBER_VERIFICATION => Some(ModelVerification::TrustedAccessForCyber),
        _ => None,
    }
}

fn json_value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Array(items) => items.first().and_then(json_value_as_string),
        _ => None,
    }
}

#[derive(Debug)]
pub enum ResponsesEventError {
    Api(ApiError),
}

impl ResponsesEventError {
    pub fn into_api_error(self) -> ApiError {
        match self {
            Self::Api(error) => error,
        }
    }
}

pub fn process_responses_event(
    event: ResponsesStreamEvent,
) -> std::result::Result<Option<ResponseEvent>, ResponsesEventError> {
    match event.kind.as_str() {
        "response.output_item.done" => {
            if let Some(item_val) = event.item {
                if let Ok(item) = serde_json::from_value::<ResponseItem>(item_val) {
                    return Ok(Some(ResponseEvent::OutputItemDone(item)));
                }
                debug!("failed to parse ResponseItem from output_item.done");
            }
        }
        "response.output_text.delta" => {
            if let Some(delta) = event.delta {
                return Ok(Some(ResponseEvent::OutputTextDelta(delta)));
            }
        }
        "response.custom_tool_call_input.delta" => {
            if let (Some(delta), Some(item_id)) =
                (event.delta, event.item_id.clone().or(event.call_id.clone()))
            {
                return Ok(Some(ResponseEvent::ToolCallInputDelta {
                    item_id,
                    call_id: event.call_id,
                    delta,
                }));
            }
        }
        "response.reasoning_summary_text.delta" => {
            if let (Some(delta), Some(summary_index)) = (event.delta, event.summary_index) {
                return Ok(Some(ResponseEvent::ReasoningSummaryDelta {
                    delta,
                    summary_index,
                }));
            }
        }
        "response.reasoning_summary_text.done" => {
            if let (Some(item_id), Some(text), Some(summary_index)) =
                (event.item_id, event.text, event.summary_index)
            {
                return Ok(Some(ResponseEvent::ReasoningSummaryDone {
                    item_id,
                    text,
                    summary_index,
                }));
            }
        }
        "response.reasoning_text.delta" => {
            if let (Some(delta), Some(content_index)) = (event.delta, event.content_index) {
                return Ok(Some(ResponseEvent::ReasoningContentDelta {
                    delta,
                    content_index,
                }));
            }
        }
        "response.created" => {
            if event.response.is_some() {
                return Ok(Some(ResponseEvent::Created {}));
            }
        }
        "response.failed" => {
            if let Some(resp_val) = event.response {
                let mut response_error = ApiError::Stream("response.failed event received".into());
                if let Some(error) = resp_val.get("error")
                    && let Ok(error) = serde_json::from_value::<Error>(error.clone())
                {
                    if is_context_window_error(&error) {
                        response_error = ApiError::ContextWindowExceeded;
                    } else if is_quota_exceeded_error(&error) {
                        response_error = ApiError::QuotaExceeded;
                    } else if is_usage_not_included(&error) {
                        response_error = ApiError::UsageNotIncluded;
                    } else if is_cyber_policy_error(&error) {
                        let message = cyber_policy_message(error.message);
                        response_error = ApiError::CyberPolicy { message };
                    } else if matches!(error.code.as_deref(), Some("invalid_prompt" | "bio_policy"))
                    {
                        let message = error
                            .message
                            .unwrap_or_else(|| "Invalid request.".to_string());
                        response_error = ApiError::InvalidRequest { message };
                    } else if is_server_overloaded_error(&error) {
                        response_error = ApiError::ServerOverloaded;
                    } else {
                        let delay = try_parse_retry_after(&error);
                        let message = error.message.unwrap_or_default();
                        response_error = ApiError::Retryable { message, delay };
                    }
                }
                return Err(ResponsesEventError::Api(response_error));
            }

            return Err(ResponsesEventError::Api(ApiError::Stream(
                "response.failed event received".into(),
            )));
        }
        "response.incomplete" => {
            let reason = event.response.as_ref().and_then(|response| {
                response
                    .get("incomplete_details")
                    .and_then(|details| details.get("reason"))
                    .and_then(Value::as_str)
            });
            if reason == Some("max_output_tokens")
                && let Some(resp_val) = event.response
            {
                match serde_json::from_value::<ResponseCompleted>(resp_val) {
                    Ok(resp) => {
                        return Ok(Some(ResponseEvent::Completed {
                            response_id: resp.id,
                            token_usage: resp.usage.map(Into::into),
                            // A token-limited response is a valid partial turn. Let the
                            // session request a continuation instead of treating it as a
                            // transport failure and reconnecting the same request.
                            end_turn: Some(false),
                        }));
                    }
                    Err(err) => {
                        let error = format!("failed to parse incomplete Responses response: {err}");
                        debug!("{error}");
                        return Err(ResponsesEventError::Api(ApiError::Stream(error)));
                    }
                }
            }

            let reason = reason.unwrap_or("unknown");
            let message = format!("Incomplete response returned, reason: {reason}");
            return Err(ResponsesEventError::Api(ApiError::Stream(message)));
        }
        "response.completed" => {
            if let Some(resp_val) = event.response {
                match serde_json::from_value::<ResponseCompleted>(resp_val) {
                    Ok(resp) => {
                        return Ok(Some(ResponseEvent::Completed {
                            response_id: resp.id,
                            token_usage: resp.usage.map(Into::into),
                            end_turn: resp.end_turn,
                        }));
                    }
                    Err(err) => {
                        let error = format!("failed to parse ResponseCompleted: {err}");
                        debug!("{error}");
                        return Err(ResponsesEventError::Api(ApiError::Stream(error)));
                    }
                }
            }
        }
        "response.output_item.added" => {
            if let Some(item_val) = event.item {
                if let Ok(item) = serde_json::from_value::<ResponseItem>(item_val) {
                    return Ok(Some(ResponseEvent::OutputItemAdded(item)));
                }
                debug!("failed to parse ResponseItem from output_item.added");
            }
        }
        "response.reasoning_summary_part.added" => {
            if let Some(summary_index) = event.summary_index {
                return Ok(Some(ResponseEvent::ReasoningSummaryPartAdded {
                    summary_index,
                }));
            }
        }
        _ => {
            trace!("unhandled responses event: {}", event.kind);
        }
    }

    Ok(None)
}

async fn process_sse_with_treatment(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    safety_buffering_treatment: SafetyBufferingTreatment,
) {
    let mut stream = stream.eventsource();
    let mut response_error: Option<ApiError> = None;
    let mut last_server_model: Option<String> = None;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("SSE Error: {e:#}");
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                let error = response_error.unwrap_or(ApiError::Stream(
                    "stream closed before response.completed".into(),
                ));
                let _ = tx_event.send(Err(error)).await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        trace!("SSE event: {}", &sse.data);
        inter_agent_trace::log_stream_event("responses_sse", &sse.data);

        let event: ResponsesStreamEvent = match serde_json::from_str(&sse.data) {
            Ok(event) => event,
            Err(e) => {
                debug!("Failed to parse SSE event: {e}, data: {}", &sse.data);
                continue;
            }
        };
        let model_verifications = event.model_verifications();
        let turn_moderation_metadata = event.turn_moderation_metadata();
        let safety_buffering = event.safety_buffering(&safety_buffering_treatment);

        if let Some(model) = event.response_model()
            && last_server_model.as_deref() != Some(model.as_str())
        {
            if tx_event
                .send(Ok(ResponseEvent::ServerModel(model.clone())))
                .await
                .is_err()
            {
                return;
            }
            last_server_model = Some(model);
        }
        if let Some(verifications) = model_verifications
            && tx_event
                .send(Ok(ResponseEvent::ModelVerifications(verifications)))
                .await
                .is_err()
        {
            return;
        }
        if let Some(metadata) = turn_moderation_metadata
            && tx_event
                .send(Ok(ResponseEvent::TurnModerationMetadata(metadata)))
                .await
                .is_err()
        {
            return;
        }
        if let Some(buffering) = safety_buffering
            && tx_event
                .send(Ok(ResponseEvent::SafetyBuffering(buffering)))
                .await
                .is_err()
        {
            return;
        }

        match process_responses_event(event) {
            Ok(Some(event)) => {
                let is_completed = matches!(event, ResponseEvent::Completed { .. });
                if tx_event.send(Ok(event)).await.is_err() {
                    return;
                }
                if is_completed {
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                response_error = Some(error.into_api_error());
            }
        };
    }
}

fn try_parse_retry_after(err: &Error) -> Option<Duration> {
    if err.code.as_deref() != Some("rate_limit_exceeded") {
        return None;
    }

    let re = rate_limit_regex();
    if let Some(message) = &err.message
        && let Some(captures) = re.captures(message)
    {
        let seconds = captures.get(1);
        let unit = captures.get(2);

        if let (Some(value), Some(unit)) = (seconds, unit) {
            let value = value.as_str().parse::<f64>().ok()?;
            let unit = unit.as_str().to_ascii_lowercase();

            if unit == "s" || unit.starts_with("second") {
                return Some(Duration::from_secs_f64(value));
            } else if unit == "ms" {
                return Some(Duration::from_millis(value as u64));
            }
        }
    }
    None
}

fn is_context_window_error(error: &Error) -> bool {
    error.code.as_deref() == Some("context_length_exceeded")
}

fn is_quota_exceeded_error(error: &Error) -> bool {
    error.code.as_deref() == Some("insufficient_quota")
}

fn is_usage_not_included(error: &Error) -> bool {
    error.code.as_deref() == Some("usage_not_included")
}

fn is_cyber_policy_error(error: &Error) -> bool {
    error.code.as_deref() == Some("cyber_policy")
}

fn is_server_overloaded_error(error: &Error) -> bool {
    error.code.as_deref() == Some("server_is_overloaded")
        || error.code.as_deref() == Some("slow_down")
}

fn cyber_policy_fallback_message() -> String {
    "This request has been flagged for possible cybersecurity risk.".to_string()
}

fn cyber_policy_message(message: Option<String>) -> String {
    message
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(cyber_policy_fallback_message)
}

fn rate_limit_regex() -> &'static regex_lite::Regex {
    static RE: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
    #[expect(clippy::unwrap_used)]
    RE.get_or_init(|| {
        regex_lite::Regex::new(r"(?i)try again in\s*(\d+(?:\.\d+)?)\s*(s|ms|seconds?)").unwrap()
    })
}
