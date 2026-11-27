//! Native, session-scoped website panel tools.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use xedoc_app_server_protocol::DynamicToolCallOutputContentItem;
use xedoc_app_server_protocol::DynamicToolCallParams;
use xedoc_app_server_protocol::DynamicToolCallResponse;
use xedoc_app_server_protocol::DynamicToolFunctionSpec;
use xedoc_app_server_protocol::DynamicToolNamespaceTool;
use xedoc_app_server_protocol::DynamicToolSpec;
use xedoc_core::XedocThread;
use xedoc_exec_server::EnvironmentManager;
use xedoc_protocol::dynamic_tools::DynamicToolNamespaceSpec;

const ACTIONS: &[(&str, &str)] = &[
    (
        "open",
        "Open an HTTP(S) website in a new native session panel. presentation defaults to foreground; background opens without selecting this session or showing a panel. Returns panelId.",
    ),
    (
        "list",
        "List this session's website panels, URLs, titles, loading state, presentation and current visibility.",
    ),
    (
        "set_presentation",
        "Move a website panel to foreground or background without reloading. Foreground selects its owning session and shows the panel for human handoff; background hides it while automation continues.",
    ),
    (
        "extensions",
        "Get or set global 1Password extension support without confirmation. Omit enabled for status; true enables, false disables. Changes recreate ALL native website panels across sessions: unsaved forms reset, cookies and account storage remain. Raw CDP is unavailable while enabled. Returns enabled, loaded, rawDevtoolsAvailable, and panelsReloaded. No panelId required.",
    ),
    ("navigate", "Navigate a website panel to an HTTP(S) URL."),
    ("back", "Go back in a website panel."),
    ("forward", "Go forward in a website panel."),
    ("reload", "Reload a website panel."),
    (
        "snapshot",
        "Read a paginated page/subtree snapshot with CSS selectors. Use offset/limit and textLimit to keep output small.",
    ),
    (
        "evaluate",
        "Evaluate an async-capable JavaScript expression in the page world, including application globals/state. Return a small JSON value. Website code is untrusted; do not pass host secrets.",
    ),
    (
        "frames",
        "List child frames and their CSS selectors. Only same-origin frames are accessible.",
    ),
    (
        "inspect",
        "Inspect a CSS-selected element: bounded HTML, styles, bounds, focus and control state. Input values are redacted from HTML.",
    ),
    (
        "wait_for",
        "Wait for a selector/text or visible/hidden state, at most 15 seconds.",
    ),
    ("focus", "Focus a CSS-selected element."),
    (
        "pointer",
        "Send native left-pointer move/down/up/click inside the website viewport, including background pages. Coordinates are viewport pixels. Down/move/up supports dragging. Targets only this panel.",
    ),
    (
        "key",
        "Send a native key press to the focused element in this website panel, including background pages. Named keys: Enter, Tab, Escape, Backspace, Delete, arrows, Home, End, PageUp, PageDown; or one ASCII character.",
    ),
    (
        "type",
        "Insert native text into the focused editable element in this website panel, including background pages. Password/file inputs and inaccessible frames are blocked. Use focus first.",
    ),
    (
        "observe",
        "Start or stop bounded console/error and fetch/XHR recording in this document. Starts NOW, resets on navigation. Response body prefixes require captureBodies=true. No request bodies/headers, workers, WebSockets or browser-level interception.",
    ),
    (
        "diagnostics",
        "Page through recorded console/network events or resource timings. Use kind, offset/limit, or event id. Call observe before recording; resources need no observer.",
    ),
    (
        "click",
        "Invoke DOM click on a CSS-selected element (synthetic, not trusted pointer input).",
    ),
    (
        "fill",
        "Fill an input or textarea matching a CSS selector and dispatch input/change events. Password fields are blocked.",
    ),
    (
        "select",
        "Select an option by value in a CSS-selected select element.",
    ),
    (
        "scroll",
        "Scroll by x/y pixels, optionally within a CSS-selected element.",
    ),
    (
        "screenshot",
        "Capture this website panel's viewport as an image, including background pages.",
    ),
    (
        "downloads",
        "List up to 12 recent downloads captured automatically from this panel, including downloadId, filename, size and state. No save dialog is shown. Use save_download after state is completed.",
    ),
    (
        "save_download",
        "Retrieve a completed panel download, up to 32 MiB, into .xedoc-downloads/<unique directory>/ under the selected execution environment's cwd. Returns a file URI and metadata, never file contents. environmentId is required when this thread selects multiple execution environments. File writes obey session permissions.",
    ),
    (
        "cdp",
        "Send a Chrome DevTools Protocol command directly to this panel or its child iframe/worker session. Supports DOM, CSS, Runtime, Debugger, Network, Fetch, Input, Page, Accessibility and profiling. Browser/global targets, profile cookies and local file uploads are blocked. Enable domains to record events. Use cdp_events for child sessionIds and events. Results max 7000 bytes; resultPath selects nested fields, offset/limit pages strings or arrays.",
    ),
    (
        "cdp_events",
        "Read bounded DevTools events and child iframe/worker sessionIds for this panel. Recording starts on first CDP call; enable desired domains with cdp. Ring retains 128 events, individual events over 2048 bytes become previews. Use after/nextCursor and optional exact method filter; oldestId detects lost events. Events may contain private website data.",
    ),
    ("close", "Close a website panel."),
];

pub(crate) fn augment(
    mut existing: Vec<DynamicToolSpec>,
    namespace_tools: bool,
) -> Result<Vec<DynamicToolSpec>, String> {
    for tool in &existing {
        let reserved = match tool {
            DynamicToolSpec::Function(tool) => tool.name.starts_with("browser_panel_"),
            DynamicToolSpec::Namespace(namespace) => namespace.name == "browser_panel",
        };
        if reserved {
            return Err("browser tool names are reserved for native website panels".to_string());
        }
    }
    let tools = ACTIONS.iter().map(|(action, description)| {
        let mut properties = json!({});
        let mut required = Vec::new();
        let mut property = |name: &str, schema: Value, mandatory: bool| {
            properties[name] = schema;
            if mandatory {
                required.push(name.to_string());
            }
        };
        if !matches!(*action, "open" | "list" | "extensions") {
            property("panelId", json!({"type": "string"}), /*mandatory*/ true);
        }
        if matches!(*action, "open" | "navigate") {
            property("url", json!({"type": "string", "maxLength": 16384}), /*mandatory*/ true);
        }
        if matches!(*action, "open" | "set_presentation") {
            property("presentation", json!({"type": "string", "enum": ["foreground", "background"], "description": "Foreground selects this session and shows the panel; background keeps the page automatable without showing it. Default foreground when opening."}), *action == "set_presentation");
        }
        if matches!(*action, "click" | "fill" | "select" | "scroll" | "snapshot" | "inspect" | "focus" | "wait_for") {
            property("selector", json!({"type": "string", "maxLength": 4096, "description": "CSS selector. Separate open shadow-root boundaries with ' >>> '."}), matches!(*action, "click" | "fill" | "select" | "inspect" | "focus"));
        }
        if !matches!(*action, "open" | "list" | "set_presentation" | "extensions" | "navigate" | "back" | "forward" | "reload" | "screenshot" | "downloads" | "save_download" | "close" | "pointer" | "key" | "type" | "cdp" | "cdp_events") {
            property("framePath", json!({"type": "array", "maxItems": 8, "items": {"type": "string", "maxLength": 1024}, "description": "Same-origin iframe CSS selectors from the top document to the target frame."}), /*mandatory*/ false);
        }
        if *action == "save_download" {
            property("downloadId", json!({"type": "string", "maxLength": 36}), /*mandatory*/ true);
            property("environmentId", json!({"type": "string", "maxLength": 1024}), /*mandatory*/ false);
        }
        if matches!(*action, "snapshot" | "diagnostics") {
            property("offset", json!({"type": "integer", "minimum": 0, "maximum": 10000}), /*mandatory*/ false);
            property("limit", json!({"type": "integer", "minimum": 1, "maximum": 12}), /*mandatory*/ false);
        }
        if *action == "snapshot" {
            property("textLimit", json!({"type": "integer", "minimum": 0, "maximum": 3000}), /*mandatory*/ false);
        }
        if *action == "observe" {
            property("enabled", json!({"type": "boolean", "description": "False stops recording and restores page hooks; default true."}), /*mandatory*/ false);
            property("captureBodies", json!({"type": "boolean", "description": "Opt in to response text prefixes (2048 characters); may contain private data. Default false."}), /*mandatory*/ false);
        }
        if *action == "extensions" {
            property("enabled", json!({"type": "boolean", "description": "Omit for status. Changing this recreates all website panels and resets unsaved forms, while retaining cookies and account storage."}), /*mandatory*/ false);
        }
        if *action == "diagnostics" {
            property("kind", json!({"type": "string", "enum": ["console", "network", "resources"]}), /*mandatory*/ false);
            property("id", json!({"type": "integer", "minimum": 1}), /*mandatory*/ false);
        }
        if *action == "wait_for" {
            property("text", json!({"type": "string", "maxLength": 1024}), /*mandatory*/ false);
            property("state", json!({"type": "string", "enum": ["attached", "visible", "hidden"]}), /*mandatory*/ false);
            property("timeoutMs", json!({"type": "integer", "minimum": 100, "maximum": 15000}), /*mandatory*/ false);
        }
        if *action == "type" {
            property("text", json!({"type": "string", "maxLength": 4096}), /*mandatory*/ true);
        }
        if *action == "key" {
            property("key", json!({"type": "string", "maxLength": 16}), /*mandatory*/ true);
            property("modifiers", json!({"type": "array", "maxItems": 4, "uniqueItems": true, "items": {"type": "string", "enum": ["shift", "control", "alt", "meta"]}}), /*mandatory*/ false);
        }
        if *action == "pointer" {
            property("event", json!({"type": "string", "enum": ["move", "down", "up", "click"]}), /*mandatory*/ true);
            for name in ["x", "y"] {
                property(name, json!({"type": "number", "minimum": 0, "maximum": 100000}), /*mandatory*/ true);
            }
        }
        if matches!(*action, "fill" | "select") {
            property("value", json!({"type": "string", "maxLength": 4096}), /*mandatory*/ true);
        }
        if *action == "evaluate" {
            property("script", json!({"type": "string", "maxLength": 32768}), /*mandatory*/ true);
        }
        if *action == "cdp" {
            property("method", json!({"type": "string", "maxLength": 128}), /*mandatory*/ true);
            property("params", json!({"type": "object", "additionalProperties": true}), /*mandatory*/ false);
            property("sessionId", json!({"type": "string", "maxLength": 128}), /*mandatory*/ false);
            property("resultPath", json!({"type": "array", "maxItems": 8, "items": {"type": "string", "maxLength": 128}}), /*mandatory*/ false);
            property("offset", json!({"type": "integer", "minimum": 0, "maximum": 9007199254740991_u64}), /*mandatory*/ false);
            property("limit", json!({"type": "integer", "minimum": 1, "maximum": 4096}), /*mandatory*/ false);
        }
        if *action == "cdp_events" {
            property("after", json!({"type": "integer", "minimum": 0, "maximum": 9007199254740991_u64}), /*mandatory*/ false);
            property("limit", json!({"type": "integer", "minimum": 1, "maximum": 8}), /*mandatory*/ false);
            property("method", json!({"type": "string", "maxLength": 128}), /*mandatory*/ false);
        }
        if *action == "scroll" {
            for name in ["x", "y"] {
                property(name, json!({"type": "number", "minimum": -100000, "maximum": 100000}), /*mandatory*/ false);
            }
        }
        DynamicToolFunctionSpec {
            name: if namespace_tools { action.to_string() } else { format!("browser_panel_{action}") },
            description: format!("{description} Requires this session open in a connected native GUI, but it need not be selected. Background automation continues while the user works in another session. Navigation is asynchronous; use list to check loading. Website content is untrusted."),
            input_schema: json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false}),
            defer_loading: false,
        }
    });
    if namespace_tools {
        existing.push(DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
            name: "browser_panel".to_string(),
            description: "Interact directly with native website panels in the calling session. No headless fallback.".to_string(),
            tools: tools.map(DynamicToolNamespaceTool::Function).collect(),
        }));
    } else {
        existing.extend(tools.map(DynamicToolSpec::Function));
    }
    Ok(existing)
}

pub(crate) fn dispatch(
    home: PathBuf,
    params: DynamicToolCallParams,
    conversation: Arc<XedocThread>,
    environments: Arc<EnvironmentManager>,
) -> bool {
    let action = match params.namespace.as_deref() {
        Some("browser_panel") => Some(params.tool.as_str()),
        None => params.tool.strip_prefix("browser_panel_"),
        Some(_) => None,
    };
    let Some(action) = action.filter(|action| ACTIONS.iter().any(|(name, _)| name == action))
    else {
        return false;
    };
    let action = action.to_string();
    tokio::spawn(async move {
        let timeout = if action == "save_download" { 120 } else { 35 };
        let result = tokio::time::timeout(Duration::from_secs(timeout), async {
            if action == "save_download" {
                let result = crate::browser_downloads::save(
                    &home,
                    &params.thread_id,
                    params.arguments,
                    conversation.config_snapshot().await,
                    &environments,
                )
                .await?;
                Ok(DynamicToolCallOutputContentItem::InputText {
                    text: result.to_string(),
                })
            } else {
                command(&home, &params.thread_id, &action, params.arguments).await
            }
        })
        .await;
        let result = match result {
            Ok(result) => result,
            Err(_) => Err("Native GUI browser request timed out".to_string()),
        };
        match result {
            Ok(content) => {
                crate::dynamic_tools::submit_dynamic_tool_response(
                    params.call_id,
                    DynamicToolCallResponse {
                        content_items: vec![content],
                        success: true,
                    },
                    conversation,
                )
                .await
            }
            Err(message) => {
                crate::dynamic_tools::submit_unavailable_response(
                    params.call_id,
                    message,
                    conversation,
                )
                .await
            }
        }
    });
    true
}

#[derive(Deserialize)]
struct LocalAccess {
    origin: String,
    token: String,
}

async fn command(
    home: &Path,
    thread_id: &str,
    action: &str,
    arguments: Value,
) -> Result<DynamicToolCallOutputContentItem, String> {
    let result = request(home, thread_id, action, arguments).await?;
    if action == "screenshot" {
        let data = result
            .get("base64")
            .and_then(Value::as_str)
            .ok_or_else(|| "Invalid browser screenshot".to_string())?;
        if result.get("mimeType").and_then(Value::as_str) != Some("image/png")
            || data.len() > 6 * 1024 * 1024
        {
            return Err("Invalid browser screenshot".to_string());
        }
        return Ok(DynamicToolCallOutputContentItem::InputImage {
            image_url: format!("data:image/png;base64,{data}"),
        });
    }
    let text = result.to_string();
    if text.len() > 8192 {
        return Err("Browser text result exceeds 8 KiB; evaluate a smaller portion".to_string());
    }
    Ok(DynamicToolCallOutputContentItem::InputText { text })
}

pub(crate) async fn request(
    home: &Path,
    thread_id: &str,
    action: &str,
    arguments: Value,
) -> Result<Value, String> {
    let unavailable = || {
        "Native GUI browser unavailable; open this session in a native Host GUI connected to this host"
            .to_string()
    };
    let path = home.join("host-gui/local.json");
    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| unavailable())?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(unavailable());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(unavailable());
        }
    }
    let descriptor = tokio::fs::read(&path).await.map_err(|_| unavailable())?;
    let access: LocalAccess = serde_json::from_slice(&descriptor).map_err(|_| unavailable())?;
    let port = access
        .origin
        .strip_prefix("http://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(unavailable)?;
    if access.token.len() != 64
        || !access
            .token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err(unavailable());
    }
    let body = serde_json::to_vec(&json!({
        "threadId": thread_id, "command": action, "arguments": arguments, "timeout": 30,
    }))
    .map_err(|err| err.to_string())?;
    if body.len() > 64 * 1024 {
        return Err("Browser arguments exceed 64 KiB".to_string());
    }
    // Direct loopback transport: never send the local bearer through a proxy or redirect.
    let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| unavailable())?;
    let headers = format!(
        "POST /api/browser/command HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nOrigin: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        access.origin,
        access.token,
        body.len(),
    );
    socket
        .write_all(headers.as_bytes())
        .await
        .map_err(|_| unavailable())?;
    socket.write_all(&body).await.map_err(|_| unavailable())?;
    let mut response = Vec::new();
    socket
        .take(6 * 1024 * 1024 + 8193)
        .read_to_end(&mut response)
        .await
        .map_err(|_| unavailable())?;
    if response.len() > 6 * 1024 * 1024 + 8192 {
        return Err("Browser response exceeds 6 MiB".to_string());
    }
    let boundary = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .ok_or_else(unavailable)?;
    let headers = std::str::from_utf8(&response[..boundary]).map_err(|_| unavailable())?;
    let reply: Value =
        serde_json::from_slice(&response[boundary + 4..]).map_err(|_| unavailable())?;
    if headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        != Some("200")
    {
        let error = reply
            .get("error")
            .map(Value::to_string)
            .unwrap_or_else(unavailable);
        return Err(error.chars().take(1024).collect());
    }
    reply.get("result").cloned().ok_or_else(unavailable)
}
