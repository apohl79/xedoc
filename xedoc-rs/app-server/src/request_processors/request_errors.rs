use super::*;

pub(super) fn environment_selection_error(err: XedocErr) -> JSONRPCErrorError {
    match err {
        XedocErr::InvalidRequest(message) => invalid_request(message),
        err => internal_error(format!("failed to validate environment selections: {err}")),
    }
}
