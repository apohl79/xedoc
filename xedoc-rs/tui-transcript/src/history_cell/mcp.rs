//! MCP tool-call, inventory, and output history cells.

use super::*;
use crate::city_lights::CityLightsStylize;

fn mcp_auth_status_label(status: McpAuthStatus) -> &'static str {
    match status {
        McpAuthStatus::Unsupported => "Unsupported",
        McpAuthStatus::NotLoggedIn => "Not logged in",
        McpAuthStatus::BearerToken => "Bearer token",
        McpAuthStatus::OAuth => "OAuth",
    }
}
#[derive(Debug)]
pub struct McpToolCallCell {
    call_id: String,
    invocation: McpInvocation,
    start_time: Instant,
    duration: Option<Duration>,
    result: Option<Result<xedoc_protocol::mcp::CallToolResult, String>>,
    animations_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct McpInvocation {
    pub server: String,
    pub tool: String,
    pub arguments: Option<serde_json::Value>,
}

impl McpToolCallCell {
    pub fn new(call_id: String, invocation: McpInvocation, animations_enabled: bool) -> Self {
        Self {
            call_id,
            invocation,
            start_time: Instant::now(),
            duration: None,
            result: None,
            animations_enabled,
        }
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn complete(
        &mut self,
        duration: Duration,
        result: Result<xedoc_protocol::mcp::CallToolResult, String>,
    ) -> Option<Box<dyn HistoryCell>> {
        let image_cell = try_new_completed_mcp_tool_call_with_image_output(&result)
            .map(|cell| Box::new(cell) as Box<dyn HistoryCell>);
        self.duration = Some(duration);
        self.result = Some(result);
        image_cell
    }

    fn success(&self) -> Option<bool> {
        match self.result.as_ref() {
            Some(Ok(result)) => Some(!result.is_error.unwrap_or(false)),
            Some(Err(_)) => Some(false),
            None => None,
        }
    }

    pub fn mark_failed(&mut self) {
        let elapsed = self.start_time.elapsed();
        self.duration = Some(elapsed);
        self.result = Some(Err("interrupted".to_string()));
    }

    fn render_content_block(block: &serde_json::Value, width: usize) -> String {
        let content = match serde_json::from_value::<rmcp::model::Content>(block.clone()) {
            Ok(content) => content,
            Err(_) => {
                return format_and_truncate_tool_result(
                    &block.to_string(),
                    TOOL_CALL_MAX_LINES,
                    width,
                );
            }
        };

        match content.raw {
            rmcp::model::RawContent::Text(text) => {
                format_and_truncate_tool_result(&text.text, TOOL_CALL_MAX_LINES, width)
            }
            rmcp::model::RawContent::Image(_) => "<image content>".to_string(),
            rmcp::model::RawContent::Audio(_) => "<audio content>".to_string(),
            rmcp::model::RawContent::Resource(resource) => {
                let uri = match resource.resource {
                    rmcp::model::ResourceContents::TextResourceContents { uri, .. } => uri,
                    rmcp::model::ResourceContents::BlobResourceContents { uri, .. } => uri,
                };
                format!("embedded resource: {uri}")
            }
            rmcp::model::RawContent::ResourceLink(link) => format!("link: {}", link.uri),
        }
    }
}

impl HistoryCell for McpToolCallCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let status = self.success();
        let bullet = match status {
            Some(true) => "•".cl_green().bold(),
            Some(false) => "•".cl_red().bold(),
            None => activity_indicator(
                Some(self.start_time),
                MotionMode::from_animations_enabled(self.animations_enabled),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "•".dim()),
        };
        let header_text = if status.is_some() {
            "Called"
        } else {
            "Calling"
        };

        let invocation_line = line_to_static(&format_mcp_invocation(self.invocation.clone()));
        let mut compact_spans = vec![bullet.clone(), " ".into(), header_text.bold(), " ".into()];
        let mut compact_header = Line::from(compact_spans.clone());
        let reserved = compact_header.width();

        let inline_invocation =
            invocation_line.width() <= (width as usize).saturating_sub(reserved);

        if inline_invocation {
            compact_header.extend(invocation_line.spans.clone());
            lines.push(compact_header);
        } else {
            compact_spans.pop(); // drop trailing space for standalone header
            lines.push(Line::from(compact_spans));

            let opts = RtOptions::new((width as usize).saturating_sub(4))
                .initial_indent("".into())
                .subsequent_indent("    ".into());
            let wrapped = adaptive_wrap_line(&invocation_line, opts);
            let body_lines: Vec<Line<'static>> = wrapped.iter().map(line_to_static).collect();
            lines.extend(prefix_lines(body_lines, "  └ ".dim(), "    ".into()));
        }

        let mut detail_lines: Vec<Line<'static>> = Vec::new();
        // Reserve four columns for the tree prefix ("  └ "/"    ") and ensure the wrapper still has at least one cell to work with.
        let detail_wrap_width = (width as usize).saturating_sub(4).max(1);

        if let Some(result) = &self.result {
            match result {
                Ok(xedoc_protocol::mcp::CallToolResult { content, .. }) => {
                    if !content.is_empty() {
                        for block in content {
                            let text = Self::render_content_block(block, detail_wrap_width);
                            for segment in text.split('\n') {
                                let line = Line::from(segment.to_string().dim());
                                let wrapped = adaptive_wrap_line(
                                    &line,
                                    RtOptions::new(detail_wrap_width)
                                        .initial_indent("".into())
                                        .subsequent_indent("    ".into()),
                                );
                                detail_lines.extend(wrapped.iter().map(line_to_static));
                            }
                        }
                    }
                }
                Err(err) => {
                    let err_text = format_and_truncate_tool_result(
                        &format!("Error: {err}"),
                        TOOL_CALL_MAX_LINES,
                        width as usize,
                    );
                    let err_line = Line::from(err_text.dim());
                    let wrapped = adaptive_wrap_line(
                        &err_line,
                        RtOptions::new(detail_wrap_width)
                            .initial_indent("".into())
                            .subsequent_indent("    ".into()),
                    );
                    detail_lines.extend(wrapped.iter().map(line_to_static));
                }
            }
        }

        if !detail_lines.is_empty() {
            let initial_prefix: Span<'static> = if inline_invocation {
                "  └ ".dim()
            } else {
                "    ".into()
            };
            lines.extend(prefix_lines(detail_lines, initial_prefix, "    ".into()));
        }

        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let header_text = if self.success().is_some() {
            "Called"
        } else {
            "Calling"
        };
        let mut lines = vec![Line::from(format!(
            "{header_text} {}",
            format_mcp_invocation(self.invocation.clone())
        ))];

        if let Some(result) = &self.result {
            match result {
                Ok(xedoc_protocol::mcp::CallToolResult { content, .. }) => {
                    for block in content {
                        let text = Self::render_content_block(block, RAW_TOOL_OUTPUT_WIDTH);
                        lines.extend(raw_lines_from_source(&text));
                    }
                }
                Err(err) => lines.push(Line::from(format!("Error: {err}"))),
            }
        }

        lines
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled || self.result.is_some() {
            return None;
        }
        Some((self.start_time.elapsed().as_millis() / 50) as u64)
    }
}

pub fn new_active_mcp_tool_call(
    call_id: String,
    invocation: McpInvocation,
    animations_enabled: bool,
) -> McpToolCallCell {
    McpToolCallCell::new(call_id, invocation, animations_enabled)
}
fn try_new_completed_mcp_tool_call_with_image_output(
    result: &Result<xedoc_protocol::mcp::CallToolResult, String>,
) -> Option<ImageOutputCell> {
    new_mcp_image_output(&result.as_ref().ok()?.content)
}

/// Retains bounded MCP image payloads for GUI previews without decoding them in the TUI.
pub fn new_mcp_image_output(content: &[serde_json::Value]) -> Option<ImageOutputCell> {
    const MAX_IMAGES: usize = 32;
    const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;
    const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

    let mut images = Vec::new();
    let mut total_bytes = 0;
    for block in content {
        if block.get("type").and_then(serde_json::Value::as_str) != Some("image") {
            continue;
        }
        let Some(data) = block.get("data").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if data.len() > MAX_IMAGE_BYTES || data.len() > MAX_TOTAL_BYTES - total_bytes {
            images.push(String::new());
        } else {
            total_bytes += data.len();
            images.push(data.to_owned());
        }
        if images.len() == MAX_IMAGES {
            break;
        }
    }
    if images.is_empty() {
        return None;
    }
    Some(ImageOutputCell {
        images,
        fallback: PlainHistoryCell::new(vec!["tool result (image output)".into()]),
    })
}
/// Render a summary of configured MCP servers from the current `Config`.
pub fn empty_mcp_output() -> PlainHistoryCell {
    let lines: Vec<Line<'static>> = vec![
        "/mcp".cl_magenta().into(),
        "".into(),
        vec!["🔌  ".into(), "MCP Tools".bold()].into(),
        "".into(),
        "  • No MCP servers configured.".italic().into(),
        Line::from(vec![
            "    See the ".into(),
            crate::terminal_hyperlinks::osc8_hyperlink(
                "https://developers.openai.com/codex/mcp",
                "MCP docs",
            )
            .underlined(),
            " to configure them.".into(),
        ])
        .style(Style::default().add_modifier(Modifier::DIM)),
    ];

    PlainHistoryCell::new(lines)
}

/// Build the `/mcp` history cell from app-server `McpServerStatus` responses.
///
/// The server list comes directly from the app-server status response, sorted
/// alphabetically. The TUI deliberately does not enrich these rows from
/// client-local config because the app-server owns the remote MCP state.
///
/// This mirrors the layout of [`new_mcp_tools_output`] but sources data from
/// the paginated RPC response rather than the in-process `McpManager`. The
/// `detail` flag controls whether resources and resource templates are rendered.
pub fn new_mcp_tools_output_from_statuses(
    statuses: &[McpServerStatus],
    detail: McpServerStatusDetail,
) -> PlainHistoryCell {
    let mut lines: Vec<Line<'static>> = vec![
        "/mcp".cl_magenta().into(),
        "".into(),
        vec!["🔌  ".into(), "MCP Tools".bold()].into(),
        "".into(),
    ];

    let mut statuses = statuses.iter().collect::<Vec<_>>();
    statuses.sort_by(|a, b| a.name.cmp(&b.name));

    let has_any_tools = statuses.iter().any(|status| !status.tools.is_empty());
    if !has_any_tools {
        lines.push("  • No MCP tools available.".italic().into());
        lines.push("".into());
    }

    for status in statuses {
        let header: Vec<Span<'static>> = vec!["  • ".into(), status.name.clone().into()];

        lines.push(header.into());
        let auth_status = match status.auth_status {
            xedoc_app_server_protocol::McpAuthStatus::Unsupported => McpAuthStatus::Unsupported,
            xedoc_app_server_protocol::McpAuthStatus::NotLoggedIn => McpAuthStatus::NotLoggedIn,
            xedoc_app_server_protocol::McpAuthStatus::BearerToken => McpAuthStatus::BearerToken,
            xedoc_app_server_protocol::McpAuthStatus::OAuth => McpAuthStatus::OAuth,
        };
        lines.push(
            vec![
                "    • Auth: ".into(),
                mcp_auth_status_label(auth_status).into(),
            ]
            .into(),
        );

        let mut names = status.tools.keys().cloned().collect::<Vec<_>>();
        names.sort();
        if names.is_empty() {
            lines.push("    • Tools: (none)".into());
        } else {
            lines.push(vec!["    • Tools: ".into(), names.join(", ").into()].into());
        }

        if matches!(detail, McpServerStatusDetail::Full) {
            let server_resources = status.resources.clone();
            if server_resources.is_empty() {
                lines.push("    • Resources: (none)".into());
            } else {
                let mut spans: Vec<Span<'static>> = vec!["    • Resources: ".into()];

                for (idx, resource) in server_resources.iter().enumerate() {
                    if idx > 0 {
                        spans.push(", ".into());
                    }

                    let label = resource.title.as_ref().unwrap_or(&resource.name);
                    spans.push(label.clone().into());
                    spans.push(" ".into());
                    spans.push(format!("({})", resource.uri).dim());
                }

                lines.push(spans.into());
            }

            let server_templates = status.resource_templates.clone();
            if server_templates.is_empty() {
                lines.push("    • Resource templates: (none)".into());
            } else {
                let mut spans: Vec<Span<'static>> = vec!["    • Resource templates: ".into()];

                for (idx, template) in server_templates.iter().enumerate() {
                    if idx > 0 {
                        spans.push(", ".into());
                    }

                    let label = template.title.as_ref().unwrap_or(&template.name);
                    spans.push(label.clone().into());
                    spans.push(" ".into());
                    spans.push(format!("({})", template.uri_template).dim());
                }

                lines.push(spans.into());
            }
        }

        lines.push(Line::from(""));
    }

    PlainHistoryCell { lines }
}
/// A transient history cell that shows an animated spinner while the MCP
/// inventory RPC is in flight.
///
/// Inserted as the `active_cell` by `ChatWidget::add_mcp_output()` and removed
/// once the fetch completes. The app removes committed copies from transcript
/// history, while `ChatWidget::clear_mcp_inventory_loading()` only clears the
/// in-flight `active_cell`.
#[derive(Debug)]
pub struct McpInventoryLoadingCell {
    start_time: Instant,
    animations_enabled: bool,
}

impl McpInventoryLoadingCell {
    pub fn new(animations_enabled: bool) -> Self {
        Self {
            start_time: Instant::now(),
            animations_enabled,
        }
    }
}

impl HistoryCell for McpInventoryLoadingCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        vec![
            vec![
                activity_indicator(
                    Some(self.start_time),
                    MotionMode::from_animations_enabled(self.animations_enabled),
                    ReducedMotionIndicator::StaticBullet,
                )
                .unwrap_or_else(|| "•".dim()),
                " ".into(),
                "Loading MCP inventory".bold(),
                "…".dim(),
            ]
            .into(),
        ]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![Line::from("Loading MCP inventory...")]
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled {
            return None;
        }
        Some((self.start_time.elapsed().as_millis() / 50) as u64)
    }
}

/// Convenience constructor for [`McpInventoryLoadingCell`].
pub fn new_mcp_inventory_loading(animations_enabled: bool) -> McpInventoryLoadingCell {
    McpInventoryLoadingCell::new(animations_enabled)
}
fn format_mcp_invocation<'a>(invocation: McpInvocation) -> Line<'a> {
    let args_str = invocation
        .arguments
        .as_ref()
        .map(|v: &serde_json::Value| {
            // Use compact form to keep things short but readable.
            serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
        })
        .unwrap_or_default();

    let invocation_spans = vec![
        invocation.server.clone().cl_cyan(),
        ".".into(),
        invocation.tool.cl_cyan(),
        "(".into(),
        args_str.dim(),
        ")".into(),
    ];
    invocation_spans.into()
}
