use pretty_assertions::assert_eq;

use super::ModelRouterScriptFailure;
use super::append_classifier_event_text;
use super::classifier_continuation_params;
use crate::client_common::ResponseEvent;
use xedoc_protocol::models::ContentItem;
use xedoc_protocol::models::ResponseItem;

#[test]
fn classifier_collects_non_streaming_provider_output() {
    let item = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "{\"complexity\":\"low\"}".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let mut output = String::new();

    append_classifier_event_text(&mut output, &ResponseEvent::OutputItemDone(item));

    assert_eq!(output, "{\"complexity\":\"low\"}");
}

#[test]
fn classifier_does_not_duplicate_completed_output_after_deltas() {
    let item = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "{\"complexity\":\"low\"}".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let mut output = String::new();

    append_classifier_event_text(
        &mut output,
        &ResponseEvent::OutputTextDelta("{\"complexity\":\"low\"}".to_string()),
    );
    append_classifier_event_text(&mut output, &ResponseEvent::OutputItemDone(item));

    assert_eq!(output, "{\"complexity\":\"low\"}");
}

#[test]
fn cancelled_classifier_does_not_create_a_continuation_request() {
    let params = classifier_continuation_params(
        "classifier:state",
        Err(ModelRouterScriptFailure::ClassifierCancelled),
        /*elapsed_ms*/ 0,
        /*cancelled*/ true,
    );

    assert!(matches!(
        params,
        Err(ModelRouterScriptFailure::ClassifierCancelled)
    ));
}
