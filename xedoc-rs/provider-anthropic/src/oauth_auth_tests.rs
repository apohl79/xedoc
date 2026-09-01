use super::AnthropicOAuthAuthProvider;
use http::HeaderMap;
use http::HeaderValue;
use pretty_assertions::assert_eq;
use xedoc_api::AuthProvider;

#[test]
fn adds_oauth_headers_without_replacing_request_betas() {
    let auth = AnthropicOAuthAuthProvider::new("private-access-token".to_string());
    let mut headers = HeaderMap::new();
    let _ = headers.insert(
        "anthropic-beta",
        HeaderValue::from_static("thinking-binding-controls-2026-08-01"),
    );

    auth.add_auth_headers(&mut headers);

    assert_eq!(
        headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer private-access-token")
    );
    assert_eq!(
        headers
            .get("anthropic-beta")
            .and_then(|value| value.to_str().ok()),
        Some("thinking-binding-controls-2026-08-01,oauth-2025-04-20")
    );
    assert_eq!(
        headers
            .get("anthropic-dangerous-direct-browser-access")
            .and_then(|value| value.to_str().ok()),
        Some("true")
    );
    assert_eq!(
        headers.get("x-app").and_then(|value| value.to_str().ok()),
        Some("cli")
    );
}

#[test]
fn debug_output_redacts_access_token() {
    let auth = AnthropicOAuthAuthProvider::new("private-access-token".to_string());

    let actual = format!("{auth:?}");

    assert!(!actual.contains("private-access-token"));
    assert!(actual.contains("<redacted>"));
}
