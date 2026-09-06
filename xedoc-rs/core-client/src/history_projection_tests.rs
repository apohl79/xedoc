use pretty_assertions::assert_eq;
use xedoc_model_provider_info::WireApi;
use xedoc_protocol::ResponseItemId;
use xedoc_protocol::models::ReasoningItemReasoningSummary;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::provider_item_metadata::ProviderItemMetadata;

use super::ProviderProvenance;

fn target(provider_id: &str, model: &str, wire_api: WireApi) -> ProviderProvenance {
    ProviderProvenance {
        provider_id: provider_id.to_string(),
        model: model.to_string(),
        wire_api,
    }
}

fn reasoning(
    metadata: Option<ProviderItemMetadata>,
    encrypted_content: Option<&str>,
) -> ResponseItem {
    ResponseItem::Reasoning {
        id: Some(ResponseItemId::from_server("rs_provider_item".to_string())),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "private reasoning".to_string(),
        }],
        content: None,
        encrypted_content: encrypted_content.map(ToString::to_string),
        provider_metadata: metadata,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn projection_drops_foreign_responses_reasoning_for_any_configured_provider() {
    let mut input = vec![reasoning(
        Some(ProviderItemMetadata::Responses {
            provider_id: "local-responses-a".to_string(),
        }),
        Some("opaque-a"),
    )];

    target("local-responses-b", "model-b", WireApi::Responses).project(&mut input);

    assert_eq!(input, Vec::new());
}

#[test]
fn projection_preserves_same_provider_responses_reasoning() {
    let item = reasoning(
        Some(ProviderItemMetadata::Responses {
            provider_id: "local-responses".to_string(),
        }),
        Some("opaque"),
    );
    let mut input = vec![item.clone()];

    target("local-responses", "model-b", WireApi::Responses).project(&mut input);

    assert_eq!(input, vec![item]);
}

#[test]
fn projection_restores_anthropic_signature_only_for_its_issuing_model() {
    let metadata = ProviderItemMetadata::Anthropic {
        provider_id: "anthropic-compatible".to_string(),
        model: "claude-a".to_string(),
        thinking_signature: Some("signature".to_string()),
    };
    let mut input = vec![reasoning(Some(metadata), None)];

    target("anthropic-compatible", "claude-a", WireApi::Anthropic).project(&mut input);

    assert_eq!(
        input,
        vec![reasoning(
            Some(ProviderItemMetadata::Anthropic {
                provider_id: "anthropic-compatible".to_string(),
                model: "claude-a".to_string(),
                thinking_signature: Some("signature".to_string()),
            }),
            Some("signature"),
        )]
    );
}

#[test]
fn projection_drops_anthropic_reasoning_when_the_model_changes() {
    let mut input = vec![reasoning(
        Some(ProviderItemMetadata::Anthropic {
            provider_id: "anthropic-compatible".to_string(),
            model: "claude-a".to_string(),
            thinking_signature: Some("signature".to_string()),
        }),
        None,
    )];

    target("anthropic-compatible", "claude-b", WireApi::Anthropic).project(&mut input);

    assert_eq!(input, Vec::new());
}

#[test]
fn projection_drops_foreign_opaque_compaction() {
    let mut input = vec![ResponseItem::Compaction {
        id: None,
        encrypted_content: "opaque-summary".to_string(),
        provider_metadata: Some(ProviderItemMetadata::Responses {
            provider_id: "local-responses-a".to_string(),
        }),
        internal_chat_message_metadata_passthrough: None,
    }];

    target("local-responses-b", "model-b", WireApi::Responses).project(&mut input);

    assert_eq!(input, Vec::new());
}

#[test]
fn projection_clears_foreign_gemini_tool_signature() {
    let mut input = vec![ResponseItem::FunctionCall {
        id: None,
        name: "tool".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: "call-1".to_string(),
        provider_metadata: Some(ProviderItemMetadata::Gemini {
            provider_id: "gemini-a".to_string(),
            thought_signature: "signature".to_string(),
        }),
        internal_chat_message_metadata_passthrough: None,
    }];

    target("gemini-b", "model-b", WireApi::Gemini).project(&mut input);

    assert_eq!(
        input,
        vec![ResponseItem::FunctionCall {
            id: None,
            name: "tool".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: "call-1".to_string(),
            provider_metadata: None,
            internal_chat_message_metadata_passthrough: None,
        }]
    );
}
