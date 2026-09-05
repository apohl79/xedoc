use futures::StreamExt;
use regex::Regex;
use reqwest::redirect::Policy;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;
use url::Host;
use url::Url;
use xedoc_extension_api::ExtensionTurnItem;
use xedoc_extension_api::FunctionCallError;
use xedoc_extension_api::ResponsesApiTool;
use xedoc_extension_api::ToolCall;
use xedoc_extension_api::ToolExecutor;
use xedoc_extension_api::ToolName;
use xedoc_extension_api::ToolOutput;
use xedoc_extension_api::ToolSpec;
use xedoc_extension_items::ExtensionItem;
use xedoc_extension_items::web_search::WebSearchAction;
use xedoc_extension_items::web_search::WebSearchItem;
use xedoc_http_client::ClientRouteClass;
use xedoc_http_client::HttpClientFactory;
use xedoc_protocol::models::WebSearchAction as CoreWebSearchAction;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::WebSearchBeginEvent;
use xedoc_protocol::protocol::WebSearchEndEvent;
use xedoc_tools::JsonSchema;
use xedoc_tools::ToolExposure;

use crate::output::SearchOutput;

const FETCH_TOOL_NAME: &str = "web_fetch";
const MAX_ARGUMENT_BYTES: usize = 8_192;
const MAX_RESPONSE_BYTES: usize = 1_000_000;
const MAX_OUTPUT_CHARS: usize = 32_000;
const MAX_QUERY_CHARS: usize = 4_096;
const MAX_URL_CHARS: usize = 8_192;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const SEARCH_TOOL_NAME: &str = "web_search";
const USER_AGENT: &str = concat!("Xedoc ", env!("CARGO_PKG_VERSION"), " web tool");

pub(crate) struct DirectWebFetchTool {
    http_client_factory: HttpClientFactory,
}

impl DirectWebFetchTool {
    pub(crate) const fn new(http_client_factory: HttpClientFactory) -> Self {
        Self {
            http_client_factory,
        }
    }
}

impl ToolExecutor<ToolCall> for DirectWebFetchTool {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(FETCH_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        function_spec(
            FETCH_TOOL_NAME,
            "Fetch a public web page and return its readable text. Only HTTP(S) URLs are supported.",
            "url",
            "Public HTTP(S) URL to fetch.",
        )
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    fn handle(&self, call: ToolCall) -> xedoc_extension_api::ToolExecutorFuture<'_> {
        Box::pin(async move {
            let args: FetchArgs = parse_arguments(call.function_arguments()?)?;
            let url = public_url(&args.url)?;
            let body = fetch_body(&self.http_client_factory, url).await?;
            let output = truncate_output(strip_html(&body));
            Ok(Box::new(SearchOutput::new(output)) as Box<dyn ToolOutput>)
        })
    }
}

pub(crate) struct DirectWebSearchTool {
    http_client_factory: HttpClientFactory,
    #[cfg(test)]
    search_url: Option<Url>,
}

impl DirectWebSearchTool {
    pub(crate) const fn new(http_client_factory: HttpClientFactory) -> Self {
        Self {
            http_client_factory,
            #[cfg(test)]
            search_url: None,
        }
    }

    #[cfg(test)]
    fn with_search_url(http_client_factory: HttpClientFactory, search_url: Url) -> Self {
        Self {
            http_client_factory,
            search_url: Some(search_url),
        }
    }

    fn request_url(&self, query: &str) -> Result<Url, FunctionCallError> {
        #[cfg(test)]
        if let Some(url) = self.search_url.as_ref() {
            return Ok(url.clone());
        }

        search_url(query)
    }
}

impl ToolExecutor<ToolCall> for DirectWebSearchTool {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SEARCH_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        function_spec(
            SEARCH_TOOL_NAME,
            "Search the public web. Returns titles and URLs from DuckDuckGo.",
            "query",
            "Search query.",
        )
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    fn handle(&self, call: ToolCall) -> xedoc_extension_api::ToolExecutorFuture<'_> {
        Box::pin(async move {
            let args: SearchArgs = parse_arguments(call.function_arguments()?)?;
            let url = self.request_url(&args.query)?;
            call.turn_item_emitter
                .emit_started(web_search_started_item(&call.call_id))
                .await;
            let html = fetch_body(&self.http_client_factory, url).await?;
            let output = extract_search_results(&html)?;
            call.turn_item_emitter
                .emit_completed(web_search_completed_item(&call.call_id, &args.query))
                .await;
            Ok(Box::new(SearchOutput::new(output)) as Box<dyn ToolOutput>)
        })
    }
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
}

fn function_spec(
    name: &str,
    description: &str,
    parameter: &str,
    parameter_description: &str,
) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: description.to_string(),
        strict: false,
        parameters: JsonSchema::object(
            BTreeMap::from([(
                parameter.to_string(),
                JsonSchema::string(Some(parameter_description.to_string())),
            )]),
            Some(vec![parameter.to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
        defer_loading: None,
    })
}

fn web_search_started_item(call_id: &str) -> ExtensionTurnItem {
    ExtensionTurnItem {
        item: ExtensionItem::WebSearch(WebSearchItem {
            id: call_id.to_string(),
            query: String::new(),
            action: None,
            results: None,
        }),
        legacy_events: vec![EventMsg::WebSearchBegin(WebSearchBeginEvent {
            call_id: call_id.to_string(),
        })],
    }
}

fn web_search_completed_item(call_id: &str, query: &str) -> ExtensionTurnItem {
    ExtensionTurnItem {
        item: ExtensionItem::WebSearch(WebSearchItem {
            id: call_id.to_string(),
            query: query.to_string(),
            action: Some(WebSearchAction::Search {
                query: Some(query.to_string()),
                queries: None,
            }),
            results: None,
        }),
        legacy_events: vec![EventMsg::WebSearchEnd(WebSearchEndEvent {
            call_id: call_id.to_string(),
            query: query.to_string(),
            action: CoreWebSearchAction::Search {
                query: Some(query.to_string()),
                queries: None,
            },
            results: None,
        })],
    }
}

async fn fetch_body(
    http_client_factory: &HttpClientFactory,
    url: Url,
) -> Result<String, FunctionCallError> {
    let request_url = url.to_string();
    let http_client_factory = http_client_factory.clone();
    let client = tokio::task::spawn_blocking(move || {
        http_client_factory.build_reqwest_client(
            reqwest::Client::builder()
                .redirect(Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .user_agent(USER_AGENT),
            &request_url,
            ClientRouteClass::Other,
        )
    })
    .await
    .map_err(|_| FunctionCallError::Fatal("web client task failed".to_string()))?
    .map_err(|_| FunctionCallError::Fatal("failed to build web client".to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| FunctionCallError::RespondToModel("web request failed".to_string()))?
        .error_for_status()
        .map_err(|_| {
            FunctionCallError::RespondToModel("web request returned an error status".to_string())
        })?;
    let bytes = bounded_body(response).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, FunctionCallError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            FunctionCallError::RespondToModel("web response interrupted".to_string())
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(FunctionCallError::RespondToModel(
                "web response exceeded the 1 MB limit".to_string(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn extract_search_results(html: &str) -> Result<String, FunctionCallError> {
    let links = Regex::new(
        r#"(?s)<a[^>]*class="[^"]*\bresult__a\b[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .map_err(|_| FunctionCallError::Fatal("failed to parse search results".to_string()))?;
    let results = links
        .captures_iter(html)
        .filter_map(|capture| {
            let url = search_result_url(capture.get(1)?.as_str())?;
            let title = strip_html(capture.get(2)?.as_str());
            (!title.is_empty()).then_some(format!("{title}\n{url}"))
        })
        .take(10)
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Ok("No web results found.".to_string());
    }
    Ok(truncate_output(results.join("\n\n")))
}

fn search_result_url(href: &str) -> Option<String> {
    let url = Url::parse(href)
        .or_else(|_| Url::parse(&format!("https:{href}")))
        .ok()?;
    url.query_pairs()
        .find(|(key, _)| key == "uddg")
        .map(|(_, value)| value.into_owned())
        .or_else(|| public_url(url.as_str()).ok().map(|url| url.to_string()))
}

fn search_url(query: &str) -> Result<Url, FunctionCallError> {
    if query.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "web_search requires a non-empty query".to_string(),
        ));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(FunctionCallError::RespondToModel(
            "web_search query exceeds the 4096 character limit".to_string(),
        ));
    }
    Url::parse(&format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    ))
    .map_err(|_| FunctionCallError::Fatal("failed to build search URL".to_string()))
}

fn public_url(value: &str) -> Result<Url, FunctionCallError> {
    if value.chars().count() > MAX_URL_CHARS {
        return Err(FunctionCallError::RespondToModel(
            "web_fetch URL exceeds the 8192 character limit".to_string(),
        ));
    }
    let url = Url::parse(value)
        .map_err(|_| FunctionCallError::RespondToModel("invalid web URL".to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || !public_host(&url) {
        return Err(FunctionCallError::RespondToModel(
            "web_fetch requires a public HTTP(S) URL".to_string(),
        ));
    }
    Ok(url)
}

fn public_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => {
            !host.eq_ignore_ascii_case("localhost")
                && !host.ends_with(".local")
                && !host.ends_with(".internal")
        }
        Some(Host::Ipv4(address)) => public_ip(IpAddr::V4(address)),
        Some(Host::Ipv6(address)) => public_ip(IpAddr::V6(address)),
        None => false,
    }
}

fn public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !address.is_loopback()
                && !address.is_private()
                && !address.is_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
        }
        IpAddr::V6(address) => {
            !address.is_loopback()
                && !address.is_unique_local()
                && !address.is_unicast_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
        }
    }
}

fn strip_html(value: &str) -> String {
    let scripts = Regex::new(r"(?is)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>").ok();
    let tags = Regex::new(r"(?is)<[^>]+>").ok();
    let without_scripts = scripts.as_ref().map_or_else(
        || value.to_string(),
        |regex| regex.replace_all(value, " ").into_owned(),
    );
    let without_tags = match tags {
        Some(regex) => regex.replace_all(&without_scripts, " ").into_owned(),
        None => without_scripts,
    };
    decode_html_entities(&without_tags)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn truncate_output(value: String) -> String {
    if value.chars().count() <= MAX_OUTPUT_CHARS {
        return value;
    }
    let mut output = value.chars().take(MAX_OUTPUT_CHARS).collect::<String>();
    output.push_str("\n\n[output truncated]");
    output
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, FunctionCallError> {
    if arguments.len() > MAX_ARGUMENT_BYTES {
        return Err(FunctionCallError::RespondToModel(
            "web tool arguments exceed the 8192 byte limit".to_string(),
        ));
    }
    serde_json::from_str(arguments)
        .map_err(|_| FunctionCallError::RespondToModel("invalid web tool arguments".to_string()))
}

#[cfg(test)]
#[path = "direct_tests.rs"]
mod tests;
