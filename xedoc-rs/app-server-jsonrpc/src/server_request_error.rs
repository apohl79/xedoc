use xedoc_app_server_protocol::JSONRPCErrorError;

pub const TURN_TRANSITION_PENDING_REQUEST_ERROR_REASON: &str = "turnTransition";

pub fn is_turn_transition_server_request_error(error: &JSONRPCErrorError) -> bool {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("reason"))
        .and_then(serde_json::Value::as_str)
        == Some(TURN_TRANSITION_PENDING_REQUEST_ERROR_REASON)
}
