use pretty_assertions::assert_eq;
use std::sync::Arc;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use xedoc_extension_api::FunctionCallError;
use xedoc_extension_items::ExtensionItem;
use xedoc_extension_items::web_search::WebSearchAction;
use xedoc_extension_items::web_search::WebSearchItem;
use xedoc_http_client::HttpClientFactory;
use xedoc_http_client::OutboundProxyPolicy;
use xedoc_protocol::models::WebSearchAction as CoreWebSearchAction;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::TruncationPolicy;
use xedoc_protocol::protocol::WebSearchBeginEvent;
use xedoc_protocol::protocol::WebSearchEndEvent;
use xedoc_tools::ConversationHistory;
use xedoc_tools::ExtensionTurnItem;
use xedoc_tools::ToolCall;
use xedoc_tools::ToolExecutor;
use xedoc_tools::ToolName;
use xedoc_tools::ToolPayload;
use xedoc_tools::TurnItemEmissionFuture;
use xedoc_tools::TurnItemEmitter;

use super::DirectWebSearchTool;
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
async fn direct_web_search_emits_query_lifecycle_items() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com">Example</a>"#,
        ))
        .mount(&server)
        .await;
    let emitter = Arc::new(CapturingTurnItemEmitter::default());
    let tool = DirectWebSearchTool::with_search_url(
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        format!("{}/search", server.uri())
            .parse()
            .expect("mock server URL should parse"),
    );

    tool.handle(tool_call(emitter.clone(), "local models"))
        .await
        .expect("direct web search should succeed");

    let started = emitter.started.lock().await;
    assert_eq!(started.len(), 1);
    assert_eq!(
        started[0].item,
        ExtensionItem::WebSearch(WebSearchItem {
            id: "search-1".to_string(),
            query: String::new(),
            action: None,
            results: None,
        })
    );
    assert!(matches!(
        started[0].legacy_events.as_slice(),
        [EventMsg::WebSearchBegin(WebSearchBeginEvent { call_id })] if call_id == "search-1"
    ));
    drop(started);

    let completed = emitter.completed.lock().await;
    assert_eq!(completed.len(), 1);
    assert_eq!(
        completed[0].item,
        ExtensionItem::WebSearch(WebSearchItem {
            id: "search-1".to_string(),
            query: "local models".to_string(),
            action: Some(WebSearchAction::Search {
                query: Some("local models".to_string()),
                queries: None,
            }),
            results: None,
        })
    );
    assert!(matches!(
        completed[0].legacy_events.as_slice(),
        [EventMsg::WebSearchEnd(WebSearchEndEvent {
            call_id,
            query,
            action: CoreWebSearchAction::Search {
                query: Some(action_query),
                queries: None,
            },
            results: None,
        })] if call_id == "search-1" && query == "local models" && action_query == "local models"
    ));
}

#[tokio::test]
async fn invalid_direct_web_search_does_not_emit_lifecycle_items() {
    let emitter = Arc::new(CapturingTurnItemEmitter::default());
    let tool =
        DirectWebSearchTool::new(HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault));

    let result = tool.handle(tool_call(emitter.clone(), "")).await;

    assert!(matches!(
        result,
        Err(FunctionCallError::RespondToModel(message))
            if message == "web_search requires a non-empty query"
    ));
    assert!(emitter.started.lock().await.is_empty());
    assert!(emitter.completed.lock().await.is_empty());
}

#[derive(Default)]
struct CapturingTurnItemEmitter {
    started: tokio::sync::Mutex<Vec<ExtensionTurnItem>>,
    completed: tokio::sync::Mutex<Vec<ExtensionTurnItem>>,
}

impl TurnItemEmitter for CapturingTurnItemEmitter {
    fn emit_started<'a>(&'a self, item: ExtensionTurnItem) -> TurnItemEmissionFuture<'a> {
        Box::pin(async move {
            self.started.lock().await.push(item);
        })
    }

    fn emit_completed<'a>(&'a self, item: ExtensionTurnItem) -> TurnItemEmissionFuture<'a> {
        Box::pin(async move {
            self.completed.lock().await.push(item);
        })
    }
}

fn tool_call(turn_item_emitter: Arc<dyn TurnItemEmitter>, query: &str) -> ToolCall {
    ToolCall {
        turn_id: "turn-1".to_string(),
        call_id: "search-1".to_string(),
        tool_name: ToolName::plain("web_search"),
        model: "model-1".to_string(),
        xedoc_turn_metadata: None,
        truncation_policy: TruncationPolicy::Bytes(/*bytes*/ 1_024),
        conversation_history: ConversationHistory::default(),
        turn_item_emitter,
        environments: Vec::new(),
        payload: ToolPayload::Function {
            arguments: serde_json::json!({ "query": query }).to_string(),
        },
    }
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
