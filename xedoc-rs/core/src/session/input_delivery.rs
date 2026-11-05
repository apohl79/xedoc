use super::session::Session;
use super::turn_context::TurnContext;
use crate::client_common::ResponseEvent;
use xedoc_protocol::models::ResponseItem;
use xedoc_protocol::protocol::EventMsg;
use xedoc_state::PromptDeliveryStore;

pub(super) fn input_identity(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message {
            id: Some(id),
            role,
            content,
            ..
        } if role == "user" => serde_json::to_string(&(id.as_str(), content)).ok(),
        _ => None,
    }
}

impl Session {
    pub(super) async fn input_delivery_for_request(
        &self,
        turn: &TurnContext,
        input: &[ResponseItem],
    ) -> std::io::Result<Option<(PromptDeliveryStore, Vec<String>)>> {
        if !tokio::fs::try_exists(turn.config.xedoc_home.join("prompt_delivery_1.sqlite")).await? {
            return Ok(None);
        }
        let store = PromptDeliveryStore::open(&turn.config.xedoc_home)
            .await
            .map_err(std::io::Error::other)?;
        let items = input.iter().filter_map(input_identity).collect::<Vec<_>>();
        let ids = store
            .mark_dispatched(&self.thread_id.to_string(), &items)
            .await
            .map_err(std::io::Error::other)?;
        Ok(Some((store, ids)))
    }

    pub(super) async fn confirm_input_delivery(
        &self,
        turn: &TurnContext,
        store: &PromptDeliveryStore,
        client_ids: &[String],
    ) -> std::io::Result<()> {
        let received = store
            .receive(&self.thread_id.to_string(), client_ids, &turn.sub_id)
            .await
            .map_err(std::io::Error::other)?;
        for client_user_message_id in received {
            self.send_event(
                turn,
                EventMsg::InputReceived {
                    client_user_message_id,
                },
            )
            .await;
        }
        Ok(())
    }
}

pub(super) fn is_generation_evidence(event: &ResponseEvent) -> bool {
    match event {
        ResponseEvent::OutputTextDelta(delta)
        | ResponseEvent::ToolCallInputDelta { delta, .. }
        | ResponseEvent::ReasoningSummaryDelta { delta, .. }
        | ResponseEvent::ReasoningContentDelta { delta, .. } => !delta.is_empty(),
        ResponseEvent::OutputItemDone(
            ResponseItem::FunctionCall { .. } | ResponseItem::CustomToolCall { .. },
        ) => true,
        // Created and completion-only streams are deliberately not sufficient:
        // some adapters synthesize them for candidate-free or blocked responses.
        _ => false,
    }
}
