use super::App;
use crate::app_server_session::AppServerSession;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId as AppServerRequestId;

pub(super) use codex_tui_thread_state::PendingAppServerRequests;

impl App {
    pub(super) async fn reject_app_server_request(
        &self,
        app_server_client: &AppServerSession,
        request_id: AppServerRequestId,
        reason: String,
    ) -> std::result::Result<(), String> {
        app_server_client
            .reject_server_request(
                request_id,
                JSONRPCErrorError {
                    code: -32000,
                    message: reason,
                    data: None,
                },
            )
            .await
            .map_err(|err| format!("failed to reject app-server request: {err}"))
    }
}
