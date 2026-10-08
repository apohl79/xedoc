use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::error;
use xedoc_app_server_protocol::DynamicToolCallOutputContentItem;
use xedoc_app_server_protocol::DynamicToolCallResponse;
use xedoc_core::XedocThread;
use xedoc_protocol::dynamic_tools::DynamicToolCallOutputContentItem as CoreDynamicToolCallOutputContentItem;
use xedoc_protocol::dynamic_tools::DynamicToolResponse as CoreDynamicToolResponse;
use xedoc_protocol::protocol::Op;

use crate::image_url::REMOTE_IMAGE_URL_ERROR;
use crate::image_url::is_remote_image_url;
use crate::outgoing_message::ClientRequestResult;
use crate::server_request_error::is_turn_transition_server_request_error;

const INVALID_AUDIO_URL_ERROR: &str = "audio URLs must use an inline data URL";

pub(crate) async fn on_call_response(
    call_id: String,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<XedocThread>,
) {
    on_call_response_with_failure(
        call_id,
        receiver,
        conversation,
        "dynamic tool request failed",
    )
    .await;
}

pub(crate) async fn on_targeted_call_response(
    call_id: String,
    response: ClientRequestResult,
    conversation: Arc<XedocThread>,
) {
    on_resolved_call_response(
        call_id,
        response,
        conversation,
        "remote-agent operation became unavailable; restart the local remote-agent broker",
    )
    .await
}

async fn on_call_response_with_failure(
    call_id: String,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<XedocThread>,
    failure_message: &str,
) {
    match receiver.await {
        Ok(response) => {
            on_resolved_call_response(call_id, response, conversation, failure_message).await;
        }
        Err(err) => {
            error!("request failed: {err:?}");
            let (response, _error) = fallback_response(failure_message);
            submit_dynamic_tool_response(call_id, response, conversation).await;
        }
    }
}

async fn on_resolved_call_response(
    call_id: String,
    response: ClientRequestResult,
    conversation: Arc<XedocThread>,
    failure_message: &str,
) {
    let (response, _error) = match response {
        Ok(value) => decode_response(value),
        Err(err) if is_turn_transition_server_request_error(&err) => return,
        Err(err) => {
            error!("request failed with client error: {err:?}");
            fallback_response(failure_message)
        }
    };

    submit_dynamic_tool_response(call_id, response, conversation).await;
}

async fn submit_dynamic_tool_response(
    call_id: String,
    response: DynamicToolCallResponse,
    conversation: Arc<XedocThread>,
) {
    let DynamicToolCallResponse {
        content_items,
        success,
    } = response;
    let core_response = CoreDynamicToolResponse {
        content_items: content_items
            .into_iter()
            .map(CoreDynamicToolCallOutputContentItem::from)
            .collect(),
        success,
    };
    submit_core_response(call_id, core_response, conversation).await;
}

pub(crate) async fn submit_unavailable_response(
    call_id: String,
    message: String,
    conversation: Arc<XedocThread>,
) {
    submit_core_response(
        call_id,
        CoreDynamicToolResponse {
            content_items: vec![CoreDynamicToolCallOutputContentItem::InputText { text: message }],
            success: false,
        },
        conversation,
    )
    .await;
}

async fn submit_core_response(
    call_id: String,
    response: CoreDynamicToolResponse,
    conversation: Arc<XedocThread>,
) {
    if let Err(err) = conversation
        .submit(Op::DynamicToolResponse {
            id: call_id,
            response,
        })
        .await
    {
        error!("failed to submit DynamicToolResponse: {err}");
    }
}

fn decode_response(value: serde_json::Value) -> (DynamicToolCallResponse, Option<String>) {
    match serde_json::from_value::<DynamicToolCallResponse>(value) {
        Ok(response)
            if response.content_items.iter().any(|item| {
                matches!(
                    item,
                    DynamicToolCallOutputContentItem::InputImage { image_url }
                        if is_remote_image_url(image_url)
                )
            }) =>
        {
            error!(
                message = REMOTE_IMAGE_URL_ERROR,
                "dynamic tool response was invalid"
            );
            fallback_response(REMOTE_IMAGE_URL_ERROR)
        }
        Ok(response)
            if response.content_items.iter().any(|item| {
                matches!(
                    item,
                    DynamicToolCallOutputContentItem::InputAudio { audio_url }
                        if !audio_url
                            .get(.."data:".len())
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
                )
            }) =>
        {
            error!(
                message = INVALID_AUDIO_URL_ERROR,
                "dynamic tool response was invalid"
            );
            fallback_response(INVALID_AUDIO_URL_ERROR)
        }
        Ok(response) => (response, None),
        Err(err) => {
            error!("failed to deserialize DynamicToolCallResponse: {err}");
            fallback_response("dynamic tool response was invalid")
        }
    }
}

fn fallback_response(message: &str) -> (DynamicToolCallResponse, Option<String>) {
    (
        DynamicToolCallResponse {
            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                text: message.to_string(),
            }],
            success: false,
        },
        Some(message.to_string()),
    )
}
