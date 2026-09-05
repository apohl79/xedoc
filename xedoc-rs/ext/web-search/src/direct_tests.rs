use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;

use super::extract_search_results;
use super::fetch_body;
use super::parse_arguments;
use super::public_url;
use super::search_url;

#[test]
fn extracts_duckduckgo_result_urls_and_titles() {
    let html = r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com">Example &amp; title</a>"#;

    assert_eq!(
        extract_search_results(html),
        Ok("Example & title\nhttps://example.com".to_string())
    );
}

#[tokio::test]
async fn fetch_body_uses_the_route_aware_reqwest_client() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/article"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<h1>Example</h1>"))
        .mount(&server)
        .await;

    let body = fetch_body(
        &HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        format!("{}/article", server.uri())
            .parse()
            .expect("mock server URL should parse"),
    )
    .await
    .expect("route-aware client should fetch mock response");

    assert_eq!(body, "<h1>Example</h1>");
}

#[tokio::test]
#[ignore = "requires internet access"]
async fn live_web_tools_fetch_duckduckgo_and_a_public_page() {
    let http_client_factory = HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy);
    let search_html = fetch_body(
        &http_client_factory,
        search_url("Xedoc agentic coding").expect("search URL should build"),
    )
    .await
    .expect("DuckDuckGo search should succeed");
    let results = extract_search_results(&search_html).expect("search results should parse");
    assert_ne!(results, "No web results found.");

    let page = fetch_body(
        &http_client_factory,
        public_url("https://example.com").expect("public page URL should parse"),
    )
    .await
    .expect("public page fetch should succeed");
    assert!(page.contains("Example Domain"));
}

#[test]
fn rejects_private_fetch_urls() {
    assert_eq!(
        public_url("http://127.0.0.1/admin"),
        Err(xedoc_extension_api::FunctionCallError::RespondToModel(
            "web_fetch requires a public HTTP(S) URL".to_string()
        ))
    );
    assert_eq!(
        public_url("http://[fd00::1]/admin"),
        Err(xedoc_extension_api::FunctionCallError::RespondToModel(
            "web_fetch requires a public HTTP(S) URL".to_string()
        ))
    );
}

#[test]
fn rejects_web_tool_arguments_over_the_byte_limit() {
    let arguments = format!(r#"{{"query":"{}"}}"#, "x".repeat(8_192));

    assert_eq!(
        parse_arguments::<serde_json::Value>(&arguments),
        Err(xedoc_extension_api::FunctionCallError::RespondToModel(
            "web tool arguments exceed the 8192 byte limit".to_string()
        ))
    );
}

#[test]
fn rejects_query_and_url_over_the_character_limits() {
    assert_eq!(
        search_url(&"x".repeat(4_097)),
        Err(xedoc_extension_api::FunctionCallError::RespondToModel(
            "web_search query exceeds the 4096 character limit".to_string()
        ))
    );
    assert_eq!(
        public_url(&format!("https://example.com/{}", "x".repeat(8_192))),
        Err(xedoc_extension_api::FunctionCallError::RespondToModel(
            "web_fetch URL exceeds the 8192 character limit".to_string()
        ))
    );
}
