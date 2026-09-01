use std::fmt;
use std::time::Duration;
use std::time::Instant;

use base64::Engine;
use chrono::SecondsFormat;
use chrono::Utc;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use rand::RngCore;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tiny_http::Request as CallbackRequest;
use tiny_http::Response as CallbackResponse;
use tiny_http::Server as CallbackServer;
use url::Url;
use xedoc_api::AuthHttpError;
use xedoc_api::AuthHttpTransport;
use xedoc_client::EncodedJsonBody;
use xedoc_client::Request;
use xedoc_client::RequestBody;

use crate::ANTHROPIC_OAUTH_TOKEN_ENDPOINT;
use crate::AnthropicOAuthCredential;

const ANTHROPIC_OAUTH_AUTHORIZE_ENDPOINT: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_OAUTH_REDIRECT_URI: &str = "http://localhost:54545/callback";
const ANTHROPIC_OAUTH_SCOPE: &str = "org:create_api_key+user:profile+user:inference";
const AUTHORIZATION_CODE_GRANT_TYPE: &str = "authorization_code";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AnthropicOAuthLoginError {
    #[error("failed to bind the Anthropic OAuth callback server")]
    CallbackBind,
    #[error("failed to receive the Anthropic OAuth callback")]
    CallbackReceive,
    #[error("timed out waiting for the Anthropic OAuth callback")]
    CallbackTimeout,
    #[error("the Anthropic OAuth callback URL is invalid")]
    InvalidCallback,
    #[error("Anthropic OAuth authorization was denied")]
    AuthorizationDenied,
    #[error("the Anthropic OAuth callback state did not match")]
    StateMismatch,
    #[error("failed to build the Anthropic OAuth token request")]
    RequestBuild,
    #[error("the Anthropic OAuth token request failed")]
    Network,
    #[error("the Anthropic OAuth token request timed out")]
    Timeout,
    #[error("the Anthropic OAuth token endpoint rejected the request with status {0}")]
    TokenRejected(StatusCode),
    #[error("the Anthropic OAuth token response is invalid")]
    InvalidTokenResponse,
}

pub struct AnthropicOAuthSession {
    auth_url: String,
    redirect_uri: String,
    state: String,
    code_verifier: String,
}

impl fmt::Debug for AnthropicOAuthSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicOAuthSession")
            .field("auth_url", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .field("state", &"<redacted>")
            .field("code_verifier", &"<redacted>")
            .finish()
    }
}

pub struct AnthropicOAuthBrowserLogin {
    session: AnthropicOAuthSession,
    server: CallbackServer,
    callback_origin: String,
}

impl fmt::Debug for AnthropicOAuthBrowserLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicOAuthBrowserLogin")
            .field("session", &self.session)
            .field("callback_origin", &self.callback_origin)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct AuthorizationCodeRequest<'a> {
    code: &'a str,
    grant_type: &'static str,
    client_id: &'static str,
    redirect_uri: &'a str,
    code_verifier: &'a str,
    state: &'a str,
}

#[derive(Deserialize)]
struct AuthorizationCodeResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    account: Option<AuthorizationAccount>,
}

#[derive(Deserialize)]
struct AuthorizationAccount {
    email_address: Option<String>,
    uuid: Option<String>,
}

impl AnthropicOAuthSession {
    pub fn new() -> Result<Self, AnthropicOAuthLoginError> {
        Self::with_material(
            ANTHROPIC_OAUTH_REDIRECT_URI,
            generate_secret(32),
            generate_secret(64),
        )
    }

    pub fn auth_url(&self) -> &str {
        &self.auth_url
    }

    pub async fn exchange_callback(
        self,
        callback_url: &str,
        transport: &dyn AuthHttpTransport,
    ) -> Result<AnthropicOAuthCredential, AnthropicOAuthLoginError> {
        self.exchange_callback_at(callback_url, ANTHROPIC_OAUTH_TOKEN_ENDPOINT, transport)
            .await
    }

    fn with_material(
        redirect_uri: &str,
        state: String,
        code_verifier: String,
    ) -> Result<Self, AnthropicOAuthLoginError> {
        let code_challenge = code_challenge(&code_verifier);
        let auth_url = authorization_url(redirect_uri, &state, &code_challenge)?;
        Ok(Self {
            auth_url,
            redirect_uri: redirect_uri.to_string(),
            state,
            code_verifier,
        })
    }

    async fn exchange_callback_at(
        self,
        callback_url: &str,
        token_endpoint: &str,
        transport: &dyn AuthHttpTransport,
    ) -> Result<AnthropicOAuthCredential, AnthropicOAuthLoginError> {
        let (code, state) = parse_callback(callback_url)?;
        if state != self.state {
            return Err(AnthropicOAuthLoginError::StateMismatch);
        }
        exchange_authorization_code(
            token_endpoint,
            &code,
            &state,
            &self.redirect_uri,
            &self.code_verifier,
            transport,
        )
        .await
    }
}

impl AnthropicOAuthBrowserLogin {
    pub fn start() -> Result<Self, AnthropicOAuthLoginError> {
        Self::start_on_port(54545)
    }

    pub fn auth_url(&self) -> &str {
        self.session.auth_url()
    }

    pub fn open_browser(&self) {
        let _ = webbrowser::open(self.auth_url());
    }

    pub async fn complete(
        self,
        transport: &dyn AuthHttpTransport,
    ) -> Result<AnthropicOAuthCredential, AnthropicOAuthLoginError> {
        self.complete_at(ANTHROPIC_OAUTH_TOKEN_ENDPOINT, transport)
            .await
    }

    fn start_on_port(port: u16) -> Result<Self, AnthropicOAuthLoginError> {
        let server = CallbackServer::http(("127.0.0.1", port))
            .map_err(|_| AnthropicOAuthLoginError::CallbackBind)?;
        let actual_port = server
            .server_addr()
            .to_ip()
            .map(|address| address.port())
            .ok_or(AnthropicOAuthLoginError::CallbackBind)?;
        let callback_origin = format!("http://localhost:{actual_port}");
        let session = AnthropicOAuthSession::with_material(
            &format!("{callback_origin}/callback"),
            generate_secret(32),
            generate_secret(64),
        )?;
        Ok(Self {
            session,
            server,
            callback_origin,
        })
    }

    async fn complete_at(
        self,
        token_endpoint: &str,
        transport: &dyn AuthHttpTransport,
    ) -> Result<AnthropicOAuthCredential, AnthropicOAuthLoginError> {
        let callback_origin = self.callback_origin;
        let request = tokio::task::spawn_blocking(move || receive_callback(self.server))
            .await
            .map_err(|_| AnthropicOAuthLoginError::CallbackReceive)??;
        let callback_url = format!("{callback_origin}{}", request.url());
        let result = self
            .session
            .exchange_callback_at(&callback_url, token_endpoint, transport)
            .await;
        respond_to_callback(request, result.is_ok()).await;
        result
    }
}

fn authorization_url(
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String, AnthropicOAuthLoginError> {
    let mut url = Url::parse(ANTHROPIC_OAUTH_AUTHORIZE_ENDPOINT)
        .map_err(|_| AnthropicOAuthLoginError::RequestBuild)?;
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", ANTHROPIC_OAUTH_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    let mut auth_url = url.to_string();
    auth_url.push_str("&scope=");
    auth_url.push_str(ANTHROPIC_OAUTH_SCOPE);
    Ok(auth_url)
}

fn parse_callback(callback_url: &str) -> Result<(String, String), AnthropicOAuthLoginError> {
    let url = Url::parse(callback_url).map_err(|_| AnthropicOAuthLoginError::InvalidCallback)?;
    if url.path() != "/callback" {
        return Err(AnthropicOAuthLoginError::InvalidCallback);
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => return Err(AnthropicOAuthLoginError::AuthorizationDenied),
            _ => {}
        }
    }
    let code = code
        .filter(|code| !code.trim().is_empty())
        .ok_or(AnthropicOAuthLoginError::InvalidCallback)?;
    let state = state
        .filter(|state| !state.trim().is_empty())
        .ok_or(AnthropicOAuthLoginError::InvalidCallback)?;
    Ok((code, state))
}

async fn exchange_authorization_code(
    token_endpoint: &str,
    code: &str,
    state: &str,
    redirect_uri: &str,
    code_verifier: &str,
    transport: &dyn AuthHttpTransport,
) -> Result<AnthropicOAuthCredential, AnthropicOAuthLoginError> {
    let request = build_token_request(token_endpoint, code, state, redirect_uri, code_verifier)?;
    let response = transport
        .execute(request)
        .await
        .map_err(|error| match error {
            AuthHttpError::Build => AnthropicOAuthLoginError::RequestBuild,
            AuthHttpError::Network => AnthropicOAuthLoginError::Network,
            AuthHttpError::Timeout => AnthropicOAuthLoginError::Timeout,
        })?;
    if !response.status().is_success() {
        return Err(AnthropicOAuthLoginError::TokenRejected(response.status()));
    }
    let response: AuthorizationCodeResponse = serde_json::from_slice(response.body())
        .map_err(|_| AnthropicOAuthLoginError::InvalidTokenResponse)?;
    credential_from_response(response)
}

fn build_token_request(
    token_endpoint: &str,
    code: &str,
    state: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<Request, AnthropicOAuthLoginError> {
    let body = EncodedJsonBody::encode(&AuthorizationCodeRequest {
        code,
        grant_type: AUTHORIZATION_CODE_GRANT_TYPE,
        client_id: ANTHROPIC_OAUTH_CLIENT_ID,
        redirect_uri,
        code_verifier,
        state,
    })
    .map_err(|_| AnthropicOAuthLoginError::RequestBuild)?;
    let mut request = Request::new(Method::POST, token_endpoint.to_string());
    request.headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    request.body = Some(RequestBody::EncodedJson(body));
    request.timeout = Some(TOKEN_EXCHANGE_TIMEOUT);
    request
        .into_prepared()
        .map_err(|_| AnthropicOAuthLoginError::RequestBuild)
}

fn credential_from_response(
    response: AuthorizationCodeResponse,
) -> Result<AnthropicOAuthCredential, AnthropicOAuthLoginError> {
    let account = response
        .account
        .ok_or(AnthropicOAuthLoginError::InvalidTokenResponse)?;
    let email = account
        .email_address
        .filter(|email| !email.trim().is_empty())
        .ok_or(AnthropicOAuthLoginError::InvalidTokenResponse)?;
    if response.access_token.trim().is_empty()
        || response.refresh_token.trim().is_empty()
        || response.expires_in <= 0
    {
        return Err(AnthropicOAuthLoginError::InvalidTokenResponse);
    }
    let account_id = account
        .uuid
        .filter(|account_id| !account_id.trim().is_empty())
        .unwrap_or_else(|| email.clone());
    let now = Utc::now();
    Ok(AnthropicOAuthCredential {
        access_token: response.access_token,
        refresh_token: response.refresh_token,
        email,
        expires_at: (now + chrono::Duration::seconds(response.expires_in))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        account_id,
        last_refresh_at: Some(now.to_rfc3339_opts(SecondsFormat::Millis, true)),
    })
}

fn receive_callback(server: CallbackServer) -> Result<CallbackRequest, AnthropicOAuthLoginError> {
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AnthropicOAuthLoginError::CallbackTimeout);
        }
        let request = server
            .recv_timeout(remaining)
            .map_err(|_| AnthropicOAuthLoginError::CallbackReceive)?
            .ok_or(AnthropicOAuthLoginError::CallbackTimeout)?;
        if request.method() == &tiny_http::Method::Get
            && request.url().split('?').next() == Some("/callback")
        {
            return Ok(request);
        }
        let _ = request.respond(CallbackResponse::empty(404));
    }
}

async fn respond_to_callback(request: CallbackRequest, succeeded: bool) {
    let message = if succeeded {
        "Anthropic login complete. You can close this window."
    } else {
        "Anthropic login failed. Return to the terminal for details."
    };
    let _ = tokio::task::spawn_blocking(move || {
        request.respond(CallbackResponse::from_string(message))
    })
    .await;
}

fn generate_secret(byte_count: usize) -> String {
    let mut bytes = vec![0u8; byte_count];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn code_challenge(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
#[path = "oauth_login_tests.rs"]
mod tests;
