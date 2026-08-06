use super::*;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

fn message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn chunks_preserve_input_order_at_budget_boundary() {
    let items = vec![message("first"), message("second"), message("third")];
    let budget = crate::context_manager::estimate_item_token_count(&items[0]);

    let chunks = chunk_history(&items, budget);

    assert_eq!(chunks.len(), 3);
    assert_eq!(
        chunks
            .into_iter()
            .flat_map(|chunk| chunk.items)
            .collect::<Vec<_>>(),
        items
    );
}

#[test]
fn chunks_keep_matching_tool_call_and_output_together() {
    let call = ResponseItem::FunctionCall {
        id: None,
        name: "lookup".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: "call-1".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "call-1".to_string(),
        output: FunctionCallOutputPayload::from_text("result".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    let budget = crate::context_manager::estimate_item_token_count(&call);

    let chunks = chunk_history(&[call.clone(), output.clone()], budget);

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].items, vec![call, output]);
}
