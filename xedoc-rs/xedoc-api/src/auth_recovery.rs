use bytes::Bytes;
use http::StatusCode;
use std::fmt;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::Arc;
use xedoc_client::Request;
use xedoc_client::TransportError;
use xedoc_http_client::ClientRouteClass;
use xedoc_http_client::HttpClientFactory;

/// Opaque identity for one provider credential.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AuthRecoveryIdentity(Arc<str>);

impl AuthRecoveryIdentity {
    /// Creates an identity that remains private from logs and user-facing errors.
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }
}

impl fmt::Debug for AuthRecoveryIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AuthRecoveryIdentity")
            .field(&"<redacted>")
            .finish()
    }
}

/// Whether this request may refresh its selected credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRefreshPolicy {
    Allowed,
    AlreadyAttempted,
}

/// Action requested by a provider after an authentication failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRecoveryAction {
    RetryAfterRefresh,
    RetryWithNextCredential,
    Propagate,
}

/// Failure while recovering provider authentication.
pub enum AuthRecoveryError {
    CredentialStore,
    InvalidTokenResponse,
    ReauthorizationRequired,
    RequestBuild,
}

impl fmt::Debug for AuthRecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CredentialStore => "CredentialStore",
            Self::InvalidTokenResponse => "InvalidTokenResponse",
            Self::ReauthorizationRequired => "ReauthorizationRequired",
            Self::RequestBuild => "RequestBuild",
        })
    }
}

impl fmt::Display for AuthRecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CredentialStore => "failed to persist provider credentials",
            Self::InvalidTokenResponse => "provider returned an invalid token response",
            Self::ReauthorizationRequired => "provider credential requires reauthorization",
            Self::RequestBuild => "failed to build provider authentication request",
        })
    }
}

impl std::error::Error for AuthRecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl From<AuthRecoveryError> for TransportError {
    fn from(error: AuthRecoveryError) -> Self {
        Self::Build(error.to_string())
    }
}

/// Result of a raw HTTP request to a provider authentication endpoint.
pub struct AuthHttpResponse {
    status: StatusCode,
    body: Bytes,
}

impl AuthHttpResponse {
    /// Creates a response without exposing its credential-bearing body through `Debug`.
    pub fn new(status: StatusCode, body: Bytes) -> Self {
        Self { status, body }
    }

    /// Returns the response status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the raw response body.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Opaque failure from a provider authentication endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthHttpError {
    Build,
    Network,
    Timeout,
}

/// Sends raw authentication requests without logging credentials or response bodies.
pub trait AuthHttpTransport: Send + Sync {
    fn execute(&self, request: Request) -> AuthHttpFuture<'_>;
}

/// Route-aware authentication transport that suppresses request diagnostics.
#[derive(Debug, Clone)]
pub struct RouteAwareAuthHttpTransport {
    factory: HttpClientFactory,
}

impl RouteAwareAuthHttpTransport {
    /// Creates a transport from the session's resolved HTTP policy.
    pub fn new(factory: HttpClientFactory) -> Self {
        Self { factory }
    }
}

impl AuthHttpTransport for RouteAwareAuthHttpTransport {
    fn execute(&self, request: Request) -> AuthHttpFuture<'_> {
        Box::pin(async move {
            let prepared = request
                .prepare_body_for_send()
                .map_err(|_| AuthHttpError::Build)?;
            let url = request.url;
            let method = request.method;
            let timeout = request.timeout;
            let factory = self.factory.clone();
            let route_url = url.clone();
            let client = tokio::task::spawn_blocking(move || {
                factory.build_reqwest_client(
                    reqwest::Client::builder(),
                    &route_url,
                    ClientRouteClass::Auth,
                )
            })
            .await
            .map_err(|_| AuthHttpError::Build)?
            .map_err(|_| AuthHttpError::Build)?;
            let mut request = client.request(method, url).headers(prepared.headers);
            if let Some(timeout) = timeout {
                request = request.timeout(timeout);
            }
            if let Some(body) = prepared.body {
                request = request.body(body);
            }
            let response = request.send().await.map_err(map_reqwest_error)?;
            let status = response.status();
            let body = response.bytes().await.map_err(map_reqwest_error)?;
            Ok(AuthHttpResponse::new(status, body))
        })
    }
}

fn map_reqwest_error(error: reqwest::Error) -> AuthHttpError {
    if error.is_timeout() {
        AuthHttpError::Timeout
    } else {
        AuthHttpError::Network
    }
}

/// Future returned by [`AuthHttpTransport`].
pub type AuthHttpFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AuthHttpResponse, AuthHttpError>> + Send + 'a>>;

/// Future returned by provider authentication recovery hooks.
pub type AuthRecoveryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AuthRecoveryAction, AuthRecoveryError>> + Send + 'a>>;
