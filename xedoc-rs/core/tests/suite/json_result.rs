#![cfg(not(target_os = "windows"))]

use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_xedoc::TestXedoc;
use core_test_support::test_xedoc::local_selections;
use core_test_support::test_xedoc::test_xedoc;
use core_test_support::test_xedoc::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use responses::ev_assistant_message;
use responses::ev_completed;
use responses::sse;
use responses::start_mock_server;
use xedoc_protocol::models::PermissionProfile;
use xedoc_protocol::protocol::AskForApproval;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::Op;
use xedoc_protocol::user_input::UserInput;

const SCHEMA: &str = r#"
{
    "type": "object",
    "properties": {
        "explanation": { "type": "string" },
        "final_answer": { "type": "string" }
    },
    "required": ["explanation", "final_answer"],
    "additionalProperties": false
}
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xedoc_returns_json_result_for_gpt5() -> anyhow::Result<()> {
    xedoc_returns_json_result("gpt-5.4".to_string()).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xedoc_returns_json_result_for_gpt5_xedoc() -> anyhow::Result<()> {
    xedoc_returns_json_result("gpt-5.4".to_string()).await
}

async fn xedoc_returns_json_result(model: String) -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;

    let sse1 = sse(vec![
        ev_assistant_message(
            "m2",
            r#"{"explanation": "explanation", "final_answer": "final_answer"}"#,
        ),
        ev_completed("r1"),
    ]);

    let expected_schema: serde_json::Value = serde_json::from_str(SCHEMA)?;
    let match_json_text_param = move |req: &wiremock::Request| {
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let Some(text) = body.get("text") else {
            return false;
        };
        let Some(format) = text.get("format") else {
            return false;
        };

        format.get("name") == Some(&serde_json::Value::String("codex_output_schema".into()))
            && format.get("type") == Some(&serde_json::Value::String("json_schema".into()))
            && format.get("strict") == Some(&serde_json::Value::Bool(true))
            && format.get("schema") == Some(&expected_schema)
    };
    responses::mount_sse_once_match(&server, match_json_text_param, sse1).await;

    let TestXedoc { xedoc, config, .. } = test_xedoc().build(&server).await?;
    let cwd = config.cwd.clone();
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, cwd.as_path());

    // 1) Normal user input – should hit server once.
    xedoc
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello world".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: Some(serde_json::from_str(SCHEMA)?),
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: xedoc_protocol::protocol::ThreadSettingsOverrides {
                environments: Some(local_selections(cwd)),
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(xedoc_protocol::config_types::CollaborationMode {
                    mode: xedoc_protocol::config_types::ModeKind::Default,
                    settings: xedoc_protocol::config_types::Settings {
                        model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            },
        })
        .await?;

    let message = wait_for_event(&xedoc, |ev| matches!(ev, EventMsg::AgentMessage(_))).await;
    if let EventMsg::AgentMessage(message) = message {
        let json: serde_json::Value = serde_json::from_str(&message.message)?;
        assert_eq!(
            json.get("explanation"),
            Some(&serde_json::Value::String("explanation".into()))
        );
        assert_eq!(
            json.get("final_answer"),
            Some(&serde_json::Value::String("final_answer".into()))
        );
    } else {
        anyhow::bail!("expected agent message event");
    }

    Ok(())
}
