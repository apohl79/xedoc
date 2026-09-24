use crate::outgoing_message::ClientRequestResult;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::session_script_registry::OpenApprovalPrompt;
use tokio::sync::oneshot;
use xedoc_app_server_protocol::RequestId;
use xedoc_app_server_protocol::ServerRequestPayload;

pub(crate) async fn await_response(
    prompt: OpenApprovalPrompt,
    request: ServerRequestPayload,
    outgoing: ThreadScopedOutgoingMessageSender,
) -> (Option<RequestId>, oneshot::Receiver<ClientRequestResult>) {
    let (request_id, client_receiver) = outgoing.send_request(request).await;
    if let Some(script_receiver) = prompt.response_receiver {
        let response_request_id = request_id.clone();
        tokio::spawn(async move {
            if let Ok(response) = script_receiver.await {
                outgoing
                    .try_notify_client_response(&response_request_id, Ok(response))
                    .await;
            }
        });
    }
    (Some(request_id), client_receiver)
}
