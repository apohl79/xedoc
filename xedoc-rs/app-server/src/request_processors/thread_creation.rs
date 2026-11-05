use super::*;
use sha2::Digest;
use sha2::Sha256;
use xedoc_state::PromptDeliveryStore;

impl ThreadRequestProcessor {
    pub(super) async fn reserve_creation(
        &self,
        params: &ThreadStartParams,
    ) -> Result<Option<ThreadStartResponse>, JSONRPCErrorError> {
        let Some(key) = params.creation_id.as_deref() else {
            return Ok(None);
        };
        let thread_id = ThreadId::from_string(key)
            .map_err(|err| invalid_request(format!("creationId must be a UUID: {err}")))?;
        if params.ephemeral == Some(true) {
            return Err(invalid_request("creationId requires a persistent thread"));
        }
        let store = PromptDeliveryStore::open(&self.config.xedoc_home)
            .await
            .map_err(|err| internal_error(err.to_string()))?;
        let mut payload =
            serde_json::to_value(params).map_err(|err| internal_error(err.to_string()))?;
        payload.sort_all_objects();
        let payload = format!("{:x}", Sha256::digest(payload.to_string().as_bytes()));
        let (claimed, record) = store
            .claim("$creation", key, &payload)
            .await
            .map_err(|err| internal_error(err.to_string()))?;
        if record.payload != payload {
            return Err(invalid_request(
                "creationId was already used with different parameters",
            ));
        }
        if !claimed {
            return match record.response {
                Some(response) => serde_json::from_str(&response)
                    .map_err(|err| internal_error(err.to_string()))?,
                None => {
                    let mut error = internal_error(
                        "creation is pending or ambiguous; reconcile the reserved thread ID with thread/resume",
                    );
                    error.data = Some(serde_json::json!({
                        "threadId": key, "retrySafe": false, "status": "unknown"
                    }));
                    Err(error)
                }
            };
        }
        if self.thread_manager.get_thread(thread_id).await.is_ok() {
            return Err(invalid_request(
                "creationId must not identify an existing thread",
            ));
        }
        for rollout in [
            xedoc_rollout::find_thread_path_by_id_str(
                &self.config.xedoc_home,
                key,
                self.state_db.as_deref(),
            )
            .await,
            xedoc_rollout::find_archived_thread_path_by_id_str(
                &self.config.xedoc_home,
                key,
                self.state_db.as_deref(),
            )
            .await,
        ] {
            if rollout
                .map_err(|err| internal_error(err.to_string()))?
                .is_some()
            {
                return Err(invalid_request(
                    "creationId must not identify an existing thread",
                ));
            }
        }
        Ok(None)
    }
}
