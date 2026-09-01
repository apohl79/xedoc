use bytes::Bytes;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use xedoc_api::ApiError;
use xedoc_api::AuthProvider;
use xedoc_api::Provider;
use xedoc_client::HttpTransport;
use xedoc_client::Request;
use xedoc_client::Response;
use xedoc_client::StreamResponse;
use xedoc_client::TransportError;

use super::GeminiCatalogClient;
use super::GeminiCatalogModel;
use super::MAX_CATALOG_PAGES;

#[derive(Clone)]
struct RecordingTransport {
    requests: Arc<Mutex<Vec<Request>>>,
    responses: Arc<Mutex<VecDeque<Result<Response, TransportError>>>>,
}

impl HttpTransport for RecordingTransport {
    async fn execute(&self, request: Request) -> Result<Response, TransportError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        self.responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Err(TransportError::Build(
                    "missing fixture response".to_string(),
                ))
            })
    }

    async fn stream(&self, _request: Request) -> Result<StreamResponse, TransportError> {
        Err(TransportError::Build("stream should not run".to_string()))
    }
}

struct FixtureAuth;

impl AuthProvider for FixtureAuth {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        let _ = headers.insert("x-goog-api-key", HeaderValue::from_static("fixture-auth"));
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ObservedRequest {
    method: Method,
    url: String,
    api_key: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ObservedCall {
    result: ObservedResult,
    requests: Vec<ObservedRequest>,
}

#[derive(Debug, PartialEq, Eq)]
enum ObservedResult {
    Models(Vec<GeminiCatalogModel>),
    StreamError(String),
    TransportBuildError(String),
    OtherError(String),
}

fn provider() -> Provider {
    Provider {
        name: "Gemini".to_string(),
        base_url: "https://example.test/v1beta".to_string(),
        query_params: Some(HashMap::from([(
            "quotaUser".to_string(),
            "xedoc".to_string(),
        )])),
        headers: HeaderMap::new(),
        retry: xedoc_api::RetryConfig {
            max_attempts: 0,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(1),
    }
}

fn transport(
    responses: impl IntoIterator<Item = Result<Response, TransportError>>,
) -> RecordingTransport {
    RecordingTransport {
        requests: Arc::new(Mutex::new(Vec::new())),
        responses: Arc::new(Mutex::new(responses.into_iter().collect())),
    }
}

fn json_response(body: Value) -> Result<Response, TransportError> {
    Ok(Response {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from(serde_json::to_vec(&body).unwrap()),
    })
}

fn model(name: &str, methods: &[&str]) -> Value {
    json!({
        "name": name,
        "displayName": format!("Display {name}"),
        "description": format!("Description {name}"),
        "inputTokenLimit": 1_048_576,
        "supportedGenerationMethods": methods,
    })
}

fn expected_model(name: &str) -> GeminiCatalogModel {
    GeminiCatalogModel {
        slug: name.strip_prefix("models/").unwrap_or(name).to_string(),
        display_name: format!("Display {name}"),
        description: Some(format!("Description {name}")),
        input_token_limit: Some(1_048_576),
    }
}

async fn observe(transport: RecordingTransport) -> ObservedCall {
    let client = GeminiCatalogClient::new(transport.clone(), provider(), Arc::new(FixtureAuth));
    let result = match client.list_models().await {
        Ok(models) => ObservedResult::Models(models),
        Err(ApiError::Stream(message)) => ObservedResult::StreamError(message),
        Err(ApiError::Transport(TransportError::Build(message))) => {
            ObservedResult::TransportBuildError(message)
        }
        Err(error) => ObservedResult::OtherError(error.to_string()),
    };
    let requests = transport
        .requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(observed_request)
        .collect();
    ObservedCall { result, requests }
}

fn observed_request(request: &Request) -> ObservedRequest {
    ObservedRequest {
        method: request.method.clone(),
        url: request.url.clone(),
        api_key: request
            .headers
            .get("x-goog-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    }
}

fn page_with_token(token: usize) -> Result<Response, TransportError> {
    json_response(json!({
        "models": [],
        "nextPageToken": token.to_string(),
    }))
}

#[tokio::test]
async fn lists_native_gemini_models_with_google_request_contract() {
    let transport = transport([json_response(json!({
        "models": [model("models/gemini-3.6-flash", &["generateContent"])],
    }))]);

    let actual = observe(transport).await;

    assert_eq!(
        actual,
        ObservedCall {
            result: ObservedResult::Models(vec![expected_model("models/gemini-3.6-flash")]),
            requests: vec![ObservedRequest {
                method: Method::GET,
                url: "https://example.test/v1beta/models?quotaUser=xedoc&pageSize=1000".to_string(),
                api_key: Some("fixture-auth".to_string()),
            }],
        }
    );
}

#[tokio::test]
async fn includes_only_gemini_generate_content_models() {
    let transport = transport([json_response(json!({
        "models": [
            model("models/gemini-3.5-flash", &["generateContent"]),
            model("models/gemini-tts", &["textToSpeech"]),
            model("models/gemma-3", &["generateContent"]),
        ],
    }))]);

    let actual = observe(transport).await.result;

    assert_eq!(
        actual,
        ObservedResult::Models(vec![expected_model("models/gemini-3.5-flash")])
    );
}

#[tokio::test]
async fn follows_encoded_page_tokens_until_the_final_page() {
    let transport = transport([
        json_response(json!({
            "models": [model("models/gemini-first", &["generateContent"])],
            "nextPageToken": "next page/+",
        })),
        json_response(json!({
            "models": [model("models/gemini-second", &["generateContent"])],
        })),
    ]);

    let actual = observe(transport).await;

    assert_eq!(
        actual,
        ObservedCall {
            result: ObservedResult::Models(vec![
                expected_model("models/gemini-first"),
                expected_model("models/gemini-second"),
            ]),
            requests: vec![
                ObservedRequest {
                    method: Method::GET,
                    url: "https://example.test/v1beta/models?quotaUser=xedoc&pageSize=1000"
                        .to_string(),
                    api_key: Some("fixture-auth".to_string()),
                },
                ObservedRequest {
                    method: Method::GET,
                    url: concat!(
                        "https://example.test/v1beta/models?quotaUser=xedoc&pageSize=1000",
                        "&pageToken=next%20page%2F%2B"
                    )
                    .to_string(),
                    api_key: Some("fixture-auth".to_string()),
                },
            ],
        }
    );
}

#[tokio::test]
async fn rejects_repeated_page_tokens() {
    let transport = transport([
        page_with_token(1),
        json_response(json!({"models": [], "nextPageToken": "1"})),
    ]);

    let actual = observe(transport).await.result;

    assert_eq!(
        actual,
        ObservedResult::StreamError("Gemini model catalog repeated a page token".to_string())
    );
}

#[tokio::test]
async fn rejects_catalogs_larger_than_the_page_limit() {
    let responses = (1..=MAX_CATALOG_PAGES).map(page_with_token);
    let transport = transport(responses);

    let actual = observe(transport).await.result;

    assert_eq!(
        actual,
        ObservedResult::StreamError(format!(
            "Gemini model catalog exceeded {MAX_CATALOG_PAGES} pages"
        ))
    );
}

#[tokio::test]
async fn reports_malformed_catalog_json() {
    let transport = transport([Ok(Response {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"{"),
    })]);

    let actual = observe(transport).await.result;

    assert_eq!(
        actual,
        ObservedResult::StreamError(
            "failed to decode Gemini models response: EOF while parsing an object at line 1 column 1"
                .to_string()
        )
    );
}

#[tokio::test]
async fn propagates_transport_build_errors() {
    let transport = transport([Err(TransportError::Build(
        "fixture transport failed".to_string(),
    ))]);

    let actual = observe(transport).await.result;

    assert_eq!(
        actual,
        ObservedResult::TransportBuildError("fixture transport failed".to_string())
    );
}
