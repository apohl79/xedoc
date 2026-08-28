use codex_tools::LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME;
use codex_tools::ListAvailablePluginsToInstallResult;
use codex_tools::RequestPluginInstallEntry;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

use crate::FunctionToolOutput;
use crate::ToolInvocation;
use crate::ToolOutput;
use crate::ToolPayload;
use crate::boxed_tool_output;
use codex_core_tool_specs::list_available_plugins_to_install_spec::create_list_available_plugins_to_install_tool;
use codex_tools::FunctionCallError;

const MAX_LIST_AVAILABLE_PLUGINS_TO_INSTALL_DESCRIPTION_CHARS: usize = 240;

pub struct ListAvailablePluginsToInstallHandler {
    tools: Vec<RequestPluginInstallEntry>,
}

impl ListAvailablePluginsToInstallHandler {
    pub fn new(mut tools: Vec<RequestPluginInstallEntry>) -> Self {
        tools.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        Self { tools }
    }

    fn result(&self) -> ListAvailablePluginsToInstallResult {
        ListAvailablePluginsToInstallResult {
            tools: self
                .tools
                .iter()
                .map(|tool| RequestPluginInstallEntry {
                    id: tool.id.clone(),
                    name: tool.name.clone(),
                    description: tool.description.as_ref().map(|description| {
                        truncate_to_char_boundary(
                            description,
                            MAX_LIST_AVAILABLE_PLUGINS_TO_INSTALL_DESCRIPTION_CHARS,
                        )
                        .to_string()
                    }),
                    tool_type: tool.tool_type,
                    has_skills: tool.has_skills,
                    mcp_server_names: tool.mcp_server_names.clone(),
                    app_connector_ids: tool.app_connector_ids.clone(),
                })
                .collect(),
        }
    }
}

impl<S: Send + Sync + 'static, C: Send + Sync + 'static> ToolExecutor<ToolInvocation<S, C>>
    for ListAvailablePluginsToInstallHandler
{
    fn tool_name(&self) -> ToolName {
        ToolName::plain(LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_list_available_plugins_to_install_tool()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        false
    }

    fn handle(&self, invocation: ToolInvocation<S, C>) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl ListAvailablePluginsToInstallHandler {
    async fn handle_call<S, C>(
        &self,
        invocation: ToolInvocation<S, C>,
    ) -> Result<Box<dyn ToolOutput>, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;
        match payload {
            ToolPayload::Function { .. } => {}
            _ => {
                return Err(FunctionCallError::Fatal(format!(
                    "{LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME} handler received unsupported payload"
                )));
            }
        }

        let content = serde_json::to_string(&self.result()).map_err(|err| {
            FunctionCallError::Fatal(format!(
                "failed to serialize {LIST_AVAILABLE_PLUGINS_TO_INSTALL_TOOL_NAME} response: {err}"
            ))
        })?;

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            content,
            Some(true),
        )))
    }
}

fn truncate_to_char_boundary(value: &str, max_chars: usize) -> &str {
    match value.char_indices().nth(max_chars) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_tools::DiscoverableToolType;
    use pretty_assertions::assert_eq;

    #[test]
    fn list_tool_does_not_support_parallel_calls() {
        assert!(!<ListAvailablePluginsToInstallHandler as ToolExecutor<
            ToolInvocation<(), ()>,
        >>::supports_parallel_tool_calls(
            &ListAvailablePluginsToInstallHandler::new(Vec::new())
        ));
    }

    #[test]
    fn result_truncates_candidate_descriptions() {
        let handler = ListAvailablePluginsToInstallHandler::new(vec![
            RequestPluginInstallEntry {
                id: "sample@openai-curated".to_string(),
                name: "Sample Plugin".to_string(),
                description: Some(
                    "x".repeat(MAX_LIST_AVAILABLE_PLUGINS_TO_INSTALL_DESCRIPTION_CHARS + 1),
                ),
                tool_type: DiscoverableToolType::Plugin,
                has_skills: true,
                mcp_server_names: vec!["sample-mcp".to_string()],
                app_connector_ids: vec!["connector-sample".to_string()],
            },
            RequestPluginInstallEntry {
                id: "calendar@openai-curated".to_string(),
                name: "Calendar".to_string(),
                description: Some("calendar".to_string()),
                tool_type: DiscoverableToolType::Plugin,
                has_skills: false,
                mcp_server_names: Vec::new(),
                app_connector_ids: Vec::new(),
            },
        ]);

        assert_eq!(
            handler.result(),
            ListAvailablePluginsToInstallResult {
                tools: vec![
                    RequestPluginInstallEntry {
                        id: "calendar@openai-curated".to_string(),
                        name: "Calendar".to_string(),
                        description: Some("calendar".to_string()),
                        tool_type: DiscoverableToolType::Plugin,
                        has_skills: false,
                        mcp_server_names: Vec::new(),
                        app_connector_ids: Vec::new(),
                    },
                    RequestPluginInstallEntry {
                        id: "sample@openai-curated".to_string(),
                        name: "Sample Plugin".to_string(),
                        description: Some(
                            "x".repeat(MAX_LIST_AVAILABLE_PLUGINS_TO_INSTALL_DESCRIPTION_CHARS,)
                        ),
                        tool_type: DiscoverableToolType::Plugin,
                        has_skills: true,
                        mcp_server_names: vec!["sample-mcp".to_string()],
                        app_connector_ids: vec!["connector-sample".to_string()],
                    },
                ],
            }
        );
    }
}
