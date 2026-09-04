use std::collections::BTreeMap;
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;

use pretty_assertions::assert_eq;
use serde_json::json;
use url::Url;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_api::RouteAwareAuthHttpTransport;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;

use super::AnthropicOAuthBrowserLogin;
use super::AnthropicOAuthLoginError;
use super::AnthropicOAuthSession;

const CODE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CODE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT_URI: &str = "http://localhost:54545/callback";
const STATE: &str = "fixed-state";

fn transport() -> RouteAwareAuthHttpTransport {
    RouteAwareAuthHttpTransport::new(HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault))
}

fn session() -> AnthropicOAuthSession {
    AnthropicOAuthSession::with_material(REDIRECT_URI, STATE.to_string(), CODE_VERIFIER.to_string())
        .expect("build OAuth session")
}

fn token_response() -> serde_json::Value {
    json!({
        "access_token": "new-access",
        "refresh_token": "new-refresh",
        "expires_in": 3600,
        "account": {
            "email_address": "user@example.com",
            "uuid": "account-id"
        }
    })
}

#[test]
fn builds_the_anthropic_authorization_contract() {
    let session = session();
    let url = Url::parse(session.auth_url()).expect("parse authorization URL");
    let query = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        (
            url.scheme(),
            url.host_str(),
            url.path(),
            query,
            session
                .auth_url()
                .contains("scope=org:create_api_key+user:profile+user:inference"),
        ),
        (
            "https",
            Some("claude.ai"),
            "/oauth/authorize",
            BTreeMap::from([
                (
                    "client_id".to_string(),
                    "9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string()
                ),
                ("code".to_string(), "true".to_string()),
                ("code_challenge".to_string(), CODE_CHALLENGE.to_string()),
                ("code_challenge_method".to_string(), "S256".to_string()),
                ("redirect_uri".to_string(), REDIRECT_URI.to_string()),
                ("response_type".to_string(), "code".to_string()),
                (
                    "scope".to_string(),
                    "org:create_api_key user:profile user:inference".to_string(),
                ),
                ("state".to_string(), STATE.to_string()),
            ]),
            true,
        )
    );
}

#[tokio::test]
async fn exchanges_a_manual_callback_for_an_oauth_credential() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_json(json!({
            "code": "authorization-code",
            "grant_type": "authorization_code",
            "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            "redirect_uri": REDIRECT_URI,
            "code_verifier": CODE_VERIFIER,
            "state": STATE,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response()))
        .expect(1)
        .mount(&server)
        .await;

    let credential = session()
        .exchange_callback_at(
            &format!("{REDIRECT_URI}?code=authorization-code&state={STATE}"),
            &format!("{}/v1/oauth/token", server.uri()),
            &transport(),
        )
        .await
        .expect("exchange callback");

    assert_eq!(
        (
            credential.access_token,
            credential.refresh_token,
            credential.email,
            credential.account_id,
            credential.last_refresh_at.is_some(),
        ),
        (
            "new-access".to_string(),
            "new-refresh".to_string(),
            "user@example.com".to_string(),
            "account-id".to_string(),
            true,
        )
    );
}

#[tokio::test]
async fn rejects_a_callback_with_the_wrong_state_before_token_exchange() {
    let result = session()
        .exchange_callback_at(
            &format!("{REDIRECT_URI}?code=authorization-code&state=wrong-state"),
            "http://127.0.0.1:9/v1/oauth/token",
            &transport(),
        )
        .await;

    assert_eq!(result, Err(AnthropicOAuthLoginError::StateMismatch));
}

#[tokio::test]
async fn returns_only_the_token_endpoint_status_on_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(r#"{"error":"private-provider-detail"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let result = session()
        .exchange_callback_at(
            &format!("{REDIRECT_URI}?code=authorization-code&state={STATE}"),
            &format!("{}/v1/oauth/token", server.uri()),
            &transport(),
        )
        .await;

    assert_eq!(
        result,
        Err(AnthropicOAuthLoginError::TokenRejected(
            http::StatusCode::UNAUTHORIZED
        ))
    );
}

#[tokio::test]
async fn browser_callback_completes_the_same_token_exchange() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response()))
        .expect(1)
        .mount(&server)
        .await;
    let login = AnthropicOAuthBrowserLogin::start_on_port(0).expect("start callback server");
    let port = Url::parse(&login.callback_origin)
        .expect("parse callback origin")
        .port()
        .expect("callback port");
    let state = login.session.state.clone();
    let callback = std::thread::spawn(move || send_callback(port, &state));

    let credential = login
        .complete_at(&format!("{}/v1/oauth/token", server.uri()), &transport())
        .await
        .expect("complete browser callback");
    let callback_response = callback
        .join()
        .expect("callback thread")
        .expect("callback response");

    assert_eq!(
        (
            credential.access_token,
            credential.refresh_token,
            credential.email,
            credential.account_id,
            callback_response.contains("Anthropic login complete"),
        ),
        (
            "new-access".to_string(),
            "new-refresh".to_string(),
            "user@example.com".to_string(),
            "account-id".to_string(),
            true,
        )
    );
}

#[tokio::test]
async fn cancelling_browser_login_releases_the_callback_port() {
    let login = AnthropicOAuthBrowserLogin::start_on_port(0).expect("start callback server");
    let port = Url::parse(&login.callback_origin)
        .expect("parse callback origin")
        .port()
        .expect("callback port");
    let cancellation = login.cancellation_handle();
    let completion = tokio::spawn(async move {
        login
            .complete_at("http://unused.invalid/token", &transport())
            .await
    });

    cancellation.cancel().await;
    assert_eq!(
        completion.await.expect("join callback receiver"),
        Err(AnthropicOAuthLoginError::CallbackTimeout)
    );
    let replacement = AnthropicOAuthBrowserLogin::start_on_port(port).expect("reuse callback port");
    drop(replacement);
}

fn send_callback(port: u16, state: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    write!(
        stream,
        "GET /callback?code=authorization-code&state={state} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}
