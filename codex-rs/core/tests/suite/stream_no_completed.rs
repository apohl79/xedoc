//! Verifies that the agent retries when the SSE stream terminates before
//! delivering a `response.completed` event.

use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;

fn sse_incomplete() -> String {
    responses::sse(vec![serde_json::json!({
        "type": "response.output_item.done",
    })])
}

fn sse_empty_completed_message_with_streamed_text() -> String {
    responses::sse(vec![
        responses::ev_response_created("resp-delta"),
        serde_json::json!({
            "type": "response.output_item.added",
            "item": {
                "id": "msg-delta",
                "type": "message",
                "status": "in_progress",
                "role": "assistant",
                "content": []
            }
        }),
        responses::ev_output_text_delta("4"),
        serde_json::json!({
            "type": "response.output_text.done",
            "item_id": "msg-delta",
            "output_index": 0,
            "content_index": 0,
            "text": "4"
        }),
        serde_json::json!({
            "type": "response.content_part.done",
            "item_id": "msg-delta",
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "4",
                "annotations": []
            }
        }),
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id": "msg-delta",
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": []
            }
        }),
        responses::ev_completed("resp-delta"),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_on_early_close() {
    skip_if_no_network!();

    let incomplete_sse = sse_incomplete();
    let completed_sse = responses::sse_completed("resp_ok");

    let (server, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: incomplete_sse,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: completed_sse,
        }],
    ])
    .await;

    // Configure retry behavior explicitly to avoid mutating process-wide
    // environment variables.

    let model_provider = ModelProviderInfo {
        name: "openai".into(),
        base_url: Some(format!("{}/v1", server.uri())),
        // Environment variable that should exist in the test environment.
        // ModelClient will return an error if the environment variable for the
        // provider is not set.
        env_key: Some("PATH".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        aws: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        // exercise retry path: first attempt yields incomplete stream, so allow 1 retry
        request_max_retries: Some(0),
        stream_max_retries: Some(1),
        stream_idle_timeout_ms: Some(2000),
        websocket_connect_timeout_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        namespace_tools: true,
        model_prices: None,
    };

    let TestCodex { codex, .. } = test_codex()
        .with_config(move |config| {
            config.model_provider = model_provider;
        })
        .build_with_streaming_server(&server)
        .await
        .unwrap();

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .unwrap();

    // Wait until TurnComplete (should succeed after retry).
    wait_for_event(&codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let requests = server.requests().await;
    assert_eq!(
        requests.len(),
        2,
        "expected retry after incomplete SSE stream"
    );

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_streamed_text_when_completed_message_content_is_empty() {
    skip_if_no_network!();

    let server = responses::start_mock_server().await;
    responses::mount_sse_once(&server, sse_empty_completed_message_with_streamed_text()).await;
    let TestCodex { codex, .. } = test_codex().build(&server).await.unwrap();

    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "whats 2+2".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .unwrap();

    let last_agent_message = wait_for_event_match(&codex, |event| match event {
        EventMsg::TurnComplete(event) => event.last_agent_message.clone(),
        _ => None,
    })
    .await;

    assert_eq!(last_agent_message, "4");
}
