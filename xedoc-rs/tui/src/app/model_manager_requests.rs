//! Model-manager requests launched by the TUI app.

use super::*;
use crate::app_event::ProviderApiKey;
use xedoc_app_server_protocol::LoginAccountParams;
use xedoc_app_server_protocol::LoginAccountResponse;
use xedoc_app_server_protocol::LoginAppBrand;
use xedoc_app_server_protocol::LogoutAccountResponse;
use xedoc_app_server_protocol::ModelManagerReadParams;
use xedoc_app_server_protocol::ModelManagerReadResponse;
use xedoc_app_server_protocol::ModelManagerUpdateParams;
use xedoc_app_server_protocol::ModelManagerUpdateResponse;
use xedoc_app_server_protocol::ModelProviderApiKeyDeleteParams;
use xedoc_app_server_protocol::ModelProviderApiKeyDeleteResponse;
use xedoc_app_server_protocol::ModelProviderApiKeySetParams;
use xedoc_app_server_protocol::ModelProviderApiKeySetResponse;
use xedoc_app_server_protocol::ModelProviderOauthStartParams;
use xedoc_app_server_protocol::ModelProviderOauthStartResponse;
use xedoc_app_server_protocol::RequestId;

const OPENAI_PROVIDER_ID: &str = "openai";

impl App {
    pub(super) fn fetch_model_manager(&mut self, app_server: &AppServerSession) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = read_model_manager(request_handle)
                .await
                .map_err(|error| error.to_string());
            app_event_tx.send(AppEvent::ModelManagerLoaded { result });
        });
    }

    pub(super) fn update_model_manager(
        &mut self,
        app_server: &AppServerSession,
        params: ModelManagerUpdateParams,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = async {
                let request_id =
                    RequestId::String(format!("model-manager-update-{}", Uuid::new_v4()));
                request_handle
                    .request_typed::<ModelManagerUpdateResponse>(
                        ClientRequest::ModelManagerUpdate { request_id, params },
                    )
                    .await
                    .wrap_err("modelManager/update failed in TUI")?;
                read_model_manager(request_handle).await
            }
            .await
            .map_err(|error| error.to_string());
            app_event_tx.send(AppEvent::ModelManagerLoaded { result });
        });
    }

    pub(super) fn set_provider_api_key(
        &mut self,
        app_server: &AppServerSession,
        provider_id: String,
        api_key: ProviderApiKey,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = async {
                store_provider_api_key(request_handle.clone(), provider_id, api_key.into_inner())
                    .await?;
                read_model_manager(request_handle).await
            }
            .await
            .map_err(|error| error.to_string());
            app_event_tx.send(AppEvent::ModelManagerLoaded { result });
        });
    }

    pub(super) fn delete_provider_api_key(
        &mut self,
        app_server: &AppServerSession,
        provider_id: String,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = async {
                remove_provider_api_key(request_handle.clone(), provider_id).await?;
                read_model_manager(request_handle).await
            }
            .await
            .map_err(|error| error.to_string());
            app_event_tx.send(AppEvent::ModelManagerLoaded { result });
        });
    }

    pub(super) fn start_provider_oauth(
        &mut self,
        app_server: &AppServerSession,
        provider_id: String,
    ) {
        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = start_provider_oauth(request_handle, &provider_id)
                .await
                .map_err(|error| error.to_string());
            app_event_tx.send(AppEvent::ProviderOauthStarted {
                provider_id,
                result,
            });
        });
    }
}

async fn read_model_manager(
    request_handle: AppServerRequestHandle,
) -> Result<ModelManagerReadResponse> {
    let request_id = RequestId::String(format!("model-manager-read-{}", Uuid::new_v4()));
    request_handle
        .request_typed(ClientRequest::ModelManagerRead {
            request_id,
            params: ModelManagerReadParams {},
        })
        .await
        .wrap_err("modelManager/read failed in TUI")
}

async fn store_provider_api_key(
    request_handle: AppServerRequestHandle,
    provider_id: String,
    api_key: String,
) -> Result<()> {
    let request_id = RequestId::String(format!("model-provider-api-key-{}", Uuid::new_v4()));
    if provider_id == OPENAI_PROVIDER_ID {
        let response = request_handle
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id,
                params: LoginAccountParams::ApiKey { api_key },
            })
            .await
            .wrap_err("account/login/start failed while storing the OpenAI API key")?;
        if !matches!(response, LoginAccountResponse::ApiKey {}) {
            color_eyre::eyre::bail!("unexpected OpenAI API-key login response: {response:?}");
        }
        return Ok(());
    }

    request_handle
        .request_typed::<ModelProviderApiKeySetResponse>(ClientRequest::ModelProviderApiKeySet {
            request_id,
            params: ModelProviderApiKeySetParams {
                provider_id,
                api_key,
            },
        })
        .await
        .wrap_err("modelProvider/apiKey/set failed in TUI")?;
    Ok(())
}

async fn remove_provider_api_key(
    request_handle: AppServerRequestHandle,
    provider_id: String,
) -> Result<()> {
    let request_id = RequestId::String(format!("model-provider-api-key-delete-{}", Uuid::new_v4()));
    if provider_id == OPENAI_PROVIDER_ID {
        request_handle
            .request_typed::<LogoutAccountResponse>(ClientRequest::LogoutAccount {
                request_id,
                params: None,
            })
            .await
            .wrap_err("account/logout failed while removing OpenAI credentials")?;
        return Ok(());
    }

    request_handle
        .request_typed::<ModelProviderApiKeyDeleteResponse>(
            ClientRequest::ModelProviderApiKeyDelete {
                request_id,
                params: ModelProviderApiKeyDeleteParams { provider_id },
            },
        )
        .await
        .wrap_err("modelProvider/apiKey/delete failed in TUI")?;
    Ok(())
}

async fn start_provider_oauth(
    request_handle: AppServerRequestHandle,
    provider_id: &str,
) -> Result<String> {
    let request_id = RequestId::String(format!("model-provider-oauth-{}", Uuid::new_v4()));
    if provider_id == OPENAI_PROVIDER_ID {
        let response = request_handle
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id,
                params: LoginAccountParams::Chatgpt {
                    codex_streamlined_login: false,
                    use_hosted_login_success_page: false,
                    app_brand: Some(LoginAppBrand::Xedoc),
                },
            })
            .await
            .wrap_err("account/login/start failed while starting OpenAI OAuth")?;
        return match response {
            LoginAccountResponse::Chatgpt { auth_url, .. } => Ok(auth_url),
            response => {
                color_eyre::eyre::bail!("unexpected OpenAI OAuth login response: {response:?}")
            }
        };
    }

    request_handle
        .request_typed::<ModelProviderOauthStartResponse>(ClientRequest::ModelProviderOauthStart {
            request_id,
            params: ModelProviderOauthStartParams {
                provider_id: provider_id.to_string(),
            },
        })
        .await
        .map(|response| response.auth_url)
        .wrap_err("modelProvider/oauth/start failed in TUI")
}
