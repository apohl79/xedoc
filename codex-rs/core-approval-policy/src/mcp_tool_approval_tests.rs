use super::*;

use pretty_assertions::assert_eq;

fn annotations(
    read_only: Option<bool>,
    destructive: Option<bool>,
    open_world: Option<bool>,
) -> ToolAnnotations {
    ToolAnnotations::from_raw(
        /*title*/ None,
        read_only,
        destructive,
        /*idempotent_hint*/ None,
        open_world,
    )
}

fn approval_metadata(
    connector_id: Option<&str>,
    connector_name: Option<&str>,
    connector_description: Option<&str>,
    tool_title: Option<&str>,
    tool_description: Option<&str>,
) -> McpToolApprovalMetadata {
    McpToolApprovalMetadata {
        annotations: None,
        connector_id: connector_id.map(str::to_string),
        link_id: None,
        connector_name: connector_name.map(str::to_string),
        connector_description: connector_description.map(str::to_string),
        connected_account_email: None,
        plugin_id: None,
        tool_title: tool_title.map(str::to_string),
        tool_description: tool_description.map(str::to_string),
        mcp_app_resource_uri: None,
        codex_apps_meta: None,
        openai_file_input_optional_fields: None,
    }
}

fn prompt_options(
    allow_session_remember: bool,
    allow_persistent_approval: bool,
) -> McpToolApprovalPromptOptions {
    McpToolApprovalPromptOptions {
        allow_session_remember,
        allow_persistent_approval,
    }
}

#[test]
fn approval_required_when_read_only_false_and_destructive() {
    let annotations = annotations(Some(false), Some(true), /*open_world*/ None);
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_read_only_false_and_open_world() {
    let annotations = annotations(Some(false), /*destructive*/ None, Some(true));
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_destructive_even_if_read_only_true() {
    let annotations = annotations(Some(true), Some(true), Some(true));
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), true);
}

#[test]
fn approval_required_when_annotations_are_absent() {
    assert_eq!(requires_mcp_tool_approval(/*annotations*/ None), true);
}

#[test]
fn approval_not_required_when_read_only_and_other_hints_are_absent() {
    let annotations = annotations(
        Some(true),
        /*destructive*/ None,
        /*open_world*/ None,
    );
    assert_eq!(requires_mcp_tool_approval(Some(&annotations)), false);
}

#[test]
fn writes_mode_requires_approval_for_non_read_only_tools() {
    let annotations = annotations(Some(false), Some(false), Some(false));
    assert_eq!(
        requires_mcp_tool_approval_for_mode(Some(&annotations), AppToolApproval::Writes),
        true
    );
    assert_eq!(
        requires_mcp_tool_approval_for_mode(/*annotations*/ None, AppToolApproval::Writes),
        true
    );
}

#[test]
fn writes_mode_does_not_require_approval_for_read_only_tools() {
    let annotations = annotations(Some(true), Some(true), Some(true));
    assert_eq!(
        requires_mcp_tool_approval_for_mode(Some(&annotations), AppToolApproval::Writes),
        false
    );
}

#[test]
fn prompting_modes_do_not_allow_persistent_remember() {
    for approval_mode in [AppToolApproval::Prompt, AppToolApproval::Writes] {
        assert_eq!(
            normalize_approval_decision_for_mode(
                McpToolApprovalDecision::AcceptForSession,
                approval_mode,
            ),
            McpToolApprovalDecision::Accept
        );
        assert_eq!(
            normalize_approval_decision_for_mode(
                McpToolApprovalDecision::AcceptAndRemember,
                approval_mode,
            ),
            McpToolApprovalDecision::Accept
        );
    }
}

#[test]
fn approval_elicitation_request_uses_message_override_and_preserves_tool_params_keys() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        CODEX_APPS_MCP_SERVER_NAME,
        "create_event",
        Some("Calendar"),
        prompt_options(
            /*allow_session_remember*/ true, /*allow_persistent_approval*/ true,
        ),
        Some("Allow Calendar to create an event?"),
    );

    let request = build_mcp_tool_approval_elicitation_request(McpToolApprovalElicitationRequest {
        server: CODEX_APPS_MCP_SERVER_NAME,
        metadata: Some(&approval_metadata(
            Some("calendar"),
            Some("Calendar"),
            Some("Manage events and schedules."),
            Some("Create Event"),
            Some("Create a calendar event."),
        )),
        tool_params: Some(&serde_json::json!({
            "calendar_id": "primary",
            "title": "Roadmap review",
        })),
        tool_params_display: Some(&[
            RenderedMcpToolApprovalParam {
                name: "calendar_id".to_string(),
                value: serde_json::json!("primary"),
                display_name: "Calendar".to_string(),
            },
            RenderedMcpToolApprovalParam {
                name: "title".to_string(),
                value: serde_json::json!("Roadmap review"),
                display_name: "Title".to_string(),
            },
        ]),
        question,
        message_override: Some("Allow Calendar to create an event?"),
        prompt_options: prompt_options(
            /*allow_session_remember*/ true, /*allow_persistent_approval*/ true,
        ),
    });

    assert_eq!(
        request,
        ElicitationRequest::Form {
            meta: Some(serde_json::json!({
                MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
                MCP_TOOL_APPROVAL_PERSIST_KEY: [
                    MCP_TOOL_APPROVAL_PERSIST_SESSION,
                    MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
                ],
                MCP_TOOL_APPROVAL_SOURCE_KEY: MCP_TOOL_APPROVAL_SOURCE_CONNECTOR,
                MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY: "calendar",
                MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY: "Calendar",
                MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY: "Manage events and schedules.",
                MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Create Event",
                MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Create a calendar event.",
                MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                    "calendar_id": "primary",
                    "title": "Roadmap review",
                },
                MCP_TOOL_APPROVAL_TOOL_PARAMS_DISPLAY_KEY: [
                    {
                        "name": "calendar_id",
                        "value": "primary",
                        "display_name": "Calendar",
                    },
                    {
                        "name": "title",
                        "value": "Roadmap review",
                        "display_name": "Title",
                    },
                ],
            })),
            message: "Allow Calendar to create an event?".to_string(),
            requested_schema: serde_json::json!({
                "type": "object",
                "properties": {},
            }),
        }
    );
}

#[test]
fn custom_mcp_tool_question_mentions_server_name() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        "custom_server",
        "run_action",
        /*connector_name*/ None,
        prompt_options(
            /*allow_session_remember*/ false, /*allow_persistent_approval*/ false,
        ),
        /*question_override*/ None,
    );

    assert_eq!(question.header, "Approve app tool call?");
    assert_eq!(
        question.question,
        "Allow the custom_server MCP server to run tool \"run_action\"?"
    );
    assert!(
        !question
            .options
            .expect("options")
            .into_iter()
            .map(|option| option.label)
            .any(|label| label == MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER)
    );
}

#[test]
fn codex_apps_tool_question_uses_fallback_app_label() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        CODEX_APPS_MCP_SERVER_NAME,
        "run_action",
        /*connector_name*/ None,
        prompt_options(
            /*allow_session_remember*/ true, /*allow_persistent_approval*/ true,
        ),
        /*question_override*/ None,
    );

    assert_eq!(
        question.question,
        "Allow this app to run tool \"run_action\"?"
    );
}

#[test]
fn trusted_codex_apps_tool_question_offers_always_allow() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        CODEX_APPS_MCP_SERVER_NAME,
        "run_action",
        Some("Calendar"),
        prompt_options(
            /*allow_session_remember*/ true, /*allow_persistent_approval*/ true,
        ),
        /*question_override*/ None,
    );
    let options = question.options.expect("options");

    assert!(options.iter().any(|option| {
        option.label == MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION
            && option.description == "Run the tool and remember this choice for this session."
    }));
    assert!(options.iter().any(|option| {
        option.label == MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER
            && option.description == "Run the tool and remember this choice for future tool calls."
    }));
    assert_eq!(
        options
            .into_iter()
            .map(|option| option.label)
            .collect::<Vec<_>>(),
        vec![
            MCP_TOOL_APPROVAL_ACCEPT.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER.to_string(),
            MCP_TOOL_APPROVAL_CANCEL.to_string(),
        ]
    );
}

#[test]
fn codex_apps_tool_question_without_elicitation_omits_always_allow() {
    let session_key = McpToolApprovalKey {
        server: CODEX_APPS_MCP_SERVER_NAME.to_string(),
        connector_id: Some("calendar".to_string()),
        tool_name: "run_action".to_string(),
    };
    let persistent_key = session_key.clone();
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        CODEX_APPS_MCP_SERVER_NAME,
        "run_action",
        Some("Calendar"),
        mcp_tool_approval_prompt_options(
            Some(&session_key),
            Some(&persistent_key),
            /*tool_call_mcp_elicitation_enabled*/ false,
        ),
        /*question_override*/ None,
    );

    assert_eq!(
        question
            .options
            .expect("options")
            .into_iter()
            .map(|option| option.label)
            .collect::<Vec<_>>(),
        vec![
            MCP_TOOL_APPROVAL_ACCEPT.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
            MCP_TOOL_APPROVAL_CANCEL.to_string(),
        ]
    );
}

#[test]
fn custom_mcp_tool_question_offers_session_remember_and_always_allow() {
    let question = build_mcp_tool_approval_question(
        "q".to_string(),
        "custom_server",
        "run_action",
        /*connector_name*/ None,
        prompt_options(
            /*allow_session_remember*/ true, /*allow_persistent_approval*/ true,
        ),
        /*question_override*/ None,
    );

    assert_eq!(
        question
            .options
            .expect("options")
            .into_iter()
            .map(|option| option.label)
            .collect::<Vec<_>>(),
        vec![
            MCP_TOOL_APPROVAL_ACCEPT.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_FOR_SESSION.to_string(),
            MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER.to_string(),
            MCP_TOOL_APPROVAL_CANCEL.to_string(),
        ]
    );
}

#[test]
fn custom_servers_support_session_and_persistent_approval() {
    let invocation = McpInvocation {
        server: "custom_server".to_string(),
        tool: "run_action".to_string(),
        arguments: None,
    };
    let expected = McpToolApprovalKey {
        server: "custom_server".to_string(),
        connector_id: None,
        tool_name: "run_action".to_string(),
    };

    assert_eq!(
        session_mcp_tool_approval_key(&invocation, /*metadata*/ None, AppToolApproval::Auto),
        Some(expected.clone())
    );
    assert_eq!(
        persistent_mcp_tool_approval_key(
            &invocation,
            /*metadata*/ None,
            AppToolApproval::Auto
        ),
        Some(expected)
    );
}

#[test]
fn codex_apps_connectors_support_persistent_approval() {
    let invocation = McpInvocation {
        server: CODEX_APPS_MCP_SERVER_NAME.to_string(),
        tool: "calendar/list_events".to_string(),
        arguments: None,
    };
    let metadata = approval_metadata(
        Some("calendar"),
        Some("Calendar"),
        /*connector_description*/ None,
        /*tool_title*/ None,
        /*tool_description*/ None,
    );
    let expected = McpToolApprovalKey {
        server: CODEX_APPS_MCP_SERVER_NAME.to_string(),
        connector_id: Some("calendar".to_string()),
        tool_name: "calendar/list_events".to_string(),
    };

    assert_eq!(
        session_mcp_tool_approval_key(&invocation, Some(&metadata), AppToolApproval::Auto),
        Some(expected.clone())
    );
    assert_eq!(
        persistent_mcp_tool_approval_key(&invocation, Some(&metadata), AppToolApproval::Auto),
        Some(expected)
    );
}

#[test]
fn accepted_elicitation_content_converts_to_request_user_input_response() {
    let response = request_user_input_response_from_elicitation_content(Some(serde_json::json!(
        {
            "approval": MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER,
        }
    )));

    assert_eq!(
        response,
        Some(RequestUserInputResponse {
            answers: std::collections::HashMap::from([(
                "approval".to_string(),
                RequestUserInputAnswer {
                    answers: vec![MCP_TOOL_APPROVAL_ACCEPT_AND_REMEMBER.to_string()],
                },
            )]),
        })
    );
}

#[test]
fn approval_elicitation_meta_marks_tool_approvals() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            "custom_server",
            /*metadata*/ None,
            /*tool_params*/ None,
            /*tool_params_display*/ None,
            prompt_options(
                /*allow_session_remember*/ false, /*allow_persistent_approval*/ false
            ),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
        }))
    );
}

#[test]
fn approval_elicitation_meta_merges_session_and_always_persist_for_custom_servers() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            "custom_server",
            Some(&approval_metadata(
                /*connector_id*/ None,
                /*connector_name*/ None,
                /*connector_description*/ None,
                Some("Run Action"),
                Some("Runs the selected action."),
            )),
            Some(&serde_json::json!({"id": 1})),
            /*tool_params_display*/ None,
            prompt_options(
                /*allow_session_remember*/ true, /*allow_persistent_approval*/ true
            ),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
            MCP_TOOL_APPROVAL_PERSIST_KEY: [
                MCP_TOOL_APPROVAL_PERSIST_SESSION,
                MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
            ],
            MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Run Action",
            MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Runs the selected action.",
            MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                "id": 1,
            },
        }))
    );
}

#[test]
fn approval_elicitation_meta_includes_connector_source_for_codex_apps() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            CODEX_APPS_MCP_SERVER_NAME,
            Some(&approval_metadata(
                Some("calendar"),
                Some("Calendar"),
                Some("Manage events and schedules."),
                Some("Run Action"),
                Some("Runs the selected action."),
            )),
            Some(&serde_json::json!({
                "calendar_id": "primary",
            })),
            /*tool_params_display*/ None,
            prompt_options(
                /*allow_session_remember*/ false, /*allow_persistent_approval*/ false
            ),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
            MCP_TOOL_APPROVAL_SOURCE_KEY: MCP_TOOL_APPROVAL_SOURCE_CONNECTOR,
            MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY: "calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY: "Calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY: "Manage events and schedules.",
            MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Run Action",
            MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Runs the selected action.",
            MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                "calendar_id": "primary",
            },
        }))
    );
}

#[test]
fn approval_elicitation_meta_merges_session_and_always_persist_with_connector_source() {
    assert_eq!(
        build_mcp_tool_approval_elicitation_meta(
            CODEX_APPS_MCP_SERVER_NAME,
            Some(&approval_metadata(
                Some("calendar"),
                Some("Calendar"),
                Some("Manage events and schedules."),
                Some("Run Action"),
                Some("Runs the selected action."),
            )),
            Some(&serde_json::json!({
                "calendar_id": "primary",
            })),
            /*tool_params_display*/ None,
            prompt_options(
                /*allow_session_remember*/ true, /*allow_persistent_approval*/ true
            ),
        ),
        Some(serde_json::json!({
            MCP_TOOL_APPROVAL_KIND_KEY: MCP_TOOL_APPROVAL_KIND_MCP_TOOL_CALL,
            MCP_TOOL_APPROVAL_PERSIST_KEY: [
                MCP_TOOL_APPROVAL_PERSIST_SESSION,
                MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
            ],
            MCP_TOOL_APPROVAL_SOURCE_KEY: MCP_TOOL_APPROVAL_SOURCE_CONNECTOR,
            MCP_TOOL_APPROVAL_CONNECTOR_ID_KEY: "calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_NAME_KEY: "Calendar",
            MCP_TOOL_APPROVAL_CONNECTOR_DESCRIPTION_KEY: "Manage events and schedules.",
            MCP_TOOL_APPROVAL_TOOL_TITLE_KEY: "Run Action",
            MCP_TOOL_APPROVAL_TOOL_DESCRIPTION_KEY: "Runs the selected action.",
            MCP_TOOL_APPROVAL_TOOL_PARAMS_KEY: {
                "calendar_id": "primary",
            },
        }))
    );
}

#[test]
fn declined_elicitation_response_stays_decline() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Decline,
            content: Some(serde_json::json!({
                "approval": MCP_TOOL_APPROVAL_ACCEPT,
            })),
            meta: None,
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::Decline { message: None });
}

#[test]
fn synthetic_decline_request_user_input_response_stays_decline() {
    let response = parse_mcp_tool_approval_response(
        Some(RequestUserInputResponse {
            answers: HashMap::from([(
                "approval".to_string(),
                RequestUserInputAnswer {
                    answers: vec![MCP_TOOL_APPROVAL_DECLINE_SYNTHETIC.to_string()],
                },
            )]),
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::Decline { message: None });
}

#[test]
fn accepted_elicitation_response_uses_always_persist_meta() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: None,
            meta: Some(serde_json::json!({
                MCP_TOOL_APPROVAL_PERSIST_KEY: MCP_TOOL_APPROVAL_PERSIST_ALWAYS,
            })),
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::AcceptAndRemember);
}

#[test]
fn accepted_elicitation_response_uses_session_persist_meta() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: None,
            meta: Some(serde_json::json!({
                MCP_TOOL_APPROVAL_PERSIST_KEY: MCP_TOOL_APPROVAL_PERSIST_SESSION,
            })),
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::AcceptForSession);
}

#[test]
fn accepted_elicitation_without_content_defaults_to_accept() {
    let response = parse_mcp_tool_approval_elicitation_response(
        Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: None,
            meta: None,
        }),
        "approval",
    );

    assert_eq!(response, McpToolApprovalDecision::Accept);
}
