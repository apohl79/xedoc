use super::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Digest;
use sha2::Sha256;
use xedoc_app_server_protocol::InputDeliveryReadParams;
use xedoc_app_server_protocol::InputDeliveryReadResponse;
use xedoc_app_server_protocol::InputDeliveryStatus;
use xedoc_state::PromptDeliveryStore;

pub(super) fn delivery_error(mut error: JSONRPCErrorError, status: &str) -> JSONRPCErrorError {
    let retry_safe = status == "rejected";
    let mut data = error.data.take().unwrap_or_else(|| serde_json::json!({}));
    if !data.is_object() {
        data = serde_json::json!({"details": data});
    }
    data["inputDelivery"] = serde_json::json!({
        "status": status,
        "retrySafe": retry_safe,
        "retryable": retry_safe && error.code == -32603,
    });
    error.data = Some(data);
    error
}

pub(super) fn validate_delivery_key(key: &str) -> Result<(), JSONRPCErrorError> {
    if key.is_empty() || key.len() > 128 {
        return Err(invalid_request("delivery IDs must contain 1 to 128 bytes"));
    }
    Ok(())
}

impl TurnRequestProcessor {
    pub(super) async fn track_input<T: Serialize + DeserializeOwned>(
        &self,
        thread_id: &str,
        client_id: Option<&str>,
        mut payload: serde_json::Value,
        submit: impl Future<Output = Result<T, JSONRPCErrorError>>,
    ) -> Result<T, JSONRPCErrorError> {
        let Some(client_id) = client_id else {
            return submit.await;
        };
        validate_delivery_key(client_id)?;
        let resolved_thread_id =
            ThreadId::from_string(thread_id).map_err(|err| invalid_request(err.to_string()))?;
        let thread_id = resolved_thread_id.to_string();
        let store = PromptDeliveryStore::open(&self.config.xedoc_home)
            .await
            .map_err(|err| delivery_error(internal_error(err.to_string()), "unknown"))?;
        payload.sort_all_objects();
        let payload = format!("{:x}", Sha256::digest(payload.to_string().as_bytes()));
        let (claimed, record) = store
            .claim(&thread_id, client_id, &payload)
            .await
            .map_err(|err| delivery_error(internal_error(err.to_string()), "unknown"))?;
        if record.payload != payload {
            return Err(delivery_error(
                invalid_request("clientUserMessageId was already used with different parameters"),
                "unknown",
            ));
        }
        if !claimed {
            return match record.response {
                Some(response) => serde_json::from_str(&response)
                    .map_err(|err| internal_error(err.to_string()))?,
                None => Err(delivery_error(
                    internal_error("submission already reserved; query inputDelivery/read"),
                    &record.status,
                )),
            };
        }
        let result = if self
            .thread_manager
            .get_thread(resolved_thread_id)
            .await
            .is_err()
        {
            Err(internal_error(
                "thread is not loaded; resume it before submitting a new input ID",
            ))
        } else {
            submit.await
        }
        .map_err(|error| delivery_error(error, "rejected"));
        let status = result.as_ref().err().map(|_| "rejected");
        let response =
            serde_json::to_value(&result).map_err(|err| internal_error(err.to_string()))?;
        let turn_id = response
            .pointer("/Ok/turn/id")
            .or_else(|| response.pointer("/Ok/turnId"))
            .and_then(serde_json::Value::as_str);
        store
            .finish(
                &thread_id,
                client_id,
                &response.to_string(),
                turn_id,
                status,
            )
            .await
            .map_err(|err| delivery_error(internal_error(err.to_string()), "unknown"))?;
        result
    }

    pub(crate) async fn input_delivery_read(
        &self,
        params: InputDeliveryReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        validate_delivery_key(&params.client_user_message_id)?;
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|err| invalid_request(err.to_string()))?
            .to_string();
        let record = PromptDeliveryStore::open(&self.config.xedoc_home)
            .await
            .map_err(|err| internal_error(err.to_string()))?
            .read(&thread_id, &params.client_user_message_id)
            .await
            .map_err(|err| internal_error(err.to_string()))?;
        let status = match record.as_ref().map(|record| record.status.as_str()) {
            Some("received") => InputDeliveryStatus::Received,
            Some("pending") => InputDeliveryStatus::Pending,
            Some("rejected") => InputDeliveryStatus::Rejected,
            _ => InputDeliveryStatus::Unknown,
        };
        Ok(Some(
            InputDeliveryReadResponse {
                status,
                turn_id: record.and_then(|record| record.turn_id),
            }
            .into(),
        ))
    }
}
