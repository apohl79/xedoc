use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;

const FIRST_PROMPT: &str = "spawn the first worker";
const FIRST_TASK: &str = "first worker task";
const SECOND_TASK: &str = "second worker task";
const MULTI_AGENT_V2_NAMESPACE: &str = "multi_agent_v2";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body)
        .is_ok_and(|body| body.to_string().contains(text))
}

fn has_function_call_output(request: &wiremock::Request, call_id: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| {
        body.get("input")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(serde_json::Value::as_str)
                        == Some("function_call_output")
                        && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
                })
            })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_nested_spawn_checks_shared_active_execution_capacity() -> Result<()> {
    let server = start_mock_server().await;
    let first_args = serde_json::to_string(&json!({
        "message": FIRST_TASK,
        "task_name": "first",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, FIRST_PROMPT),
        sse(vec![
            ev_response_created("first-response"),
            ev_function_call_with_namespace(
                "first-call",
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &first_args,
            ),
            ev_completed("first-response"),
        ]),
    )
    .await;
    let second_args = serde_json::to_string(&json!({
        "message": SECOND_TASK,
        "task_name": "second",
    }))?;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, FIRST_TASK) && !has_function_call_output(request, "first-call")
        },
        sse(vec![
            ev_response_created("first-worker-response"),
            ev_function_call_with_namespace(
                "second-call",
                MULTI_AGENT_V2_NAMESPACE,
                "spawn_agent",
                &second_args,
            ),
            ev_completed("first-worker-response"),
        ]),
    )
    .await;
    let second_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "second-call"),
        sse(vec![
            ev_response_created("second-followup-response"),
            ev_assistant_message("second-followup-message", "blocked"),
            ev_completed("second-followup-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| has_function_call_output(request, "first-call"),
        sse(vec![
            ev_response_created("first-followup-response"),
            ev_assistant_message("first-followup-message", "spawned"),
            ev_completed("first-followup-response"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_model("koffing").with_config(|config| {
        config
            .features
            .enable(Feature::Collab)
            .expect("test config should allow feature update");
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("test config should allow feature update");
        config.multi_agent_v2.max_concurrent_threads_per_session = 2;
    });
    let test = builder.build(&server).await?;
    test.submit_turn(FIRST_PROMPT).await?;

    let second_output = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(output) = second_followup.function_call_output_text("second-call") {
                return output;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(
        second_output,
        "collab spawn failed: agent thread limit reached"
    );
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_leaf_spawns_keep_v2_capacity_and_only_disable_spawning() -> Result<()> {
    const ROOT_PROMPT: &str = "spawn the capacity workers";
    const SUBAGENT_HINT: &str = "Delegating child guidance.";
    const MODE_HINT: &str = "Delegating mode guidance.";
    let spawns = [
        (
            "spawn-delegating-explicit",
            "delegating_explicit",
            "delegating explicit task",
            Some(true),
        ),
        (
            "spawn-delegating-default",
            "delegating_default",
            "delegating default task",
            None,
        ),
        (
            "spawn-delegating-2",
            "delegating_2",
            "delegating task 2",
            None,
        ),
        (
            "spawn-delegating-3",
            "delegating_3",
            "delegating task 3",
            None,
        ),
        ("spawn-leaf-0", "leaf_0", "leaf task 0", Some(false)),
        ("spawn-leaf-1", "leaf_1", "leaf task 1", Some(false)),
        ("spawn-leaf-2", "leaf_2", "leaf task 2", Some(false)),
    ];
    let server = start_mock_server().await;
    let mut root_events = vec![ev_response_created("root-spawn-response")];
    for (call_id, task_name, task, allow_delegation) in spawns {
        let mut args = json!({
            "message": task,
            "task_name": task_name,
            "fork_turns": "none",
        });
        if let Some(allow_delegation) = allow_delegation {
            args["allow_delegation"] = json!(allow_delegation);
        }
        root_events.push(ev_function_call_with_namespace(
            call_id,
            MULTI_AGENT_V2_NAMESPACE,
            "spawn_agent",
            &args.to_string(),
        ));
    }
    root_events.push(ev_completed("root-spawn-response"));
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, ROOT_PROMPT),
        sse(root_events),
    )
    .await;

    let mut child_responses = Vec::new();
    for (index, (call_id, _, task, _)) in spawns.into_iter().enumerate() {
        let call_id = call_id.to_string();
        let task = task.to_string();
        let response_id = format!("child-response-{index}");
        child_responses.push(
            mount_sse_once_match(
                &server,
                move |request: &wiremock::Request| {
                    body_contains(request, &task) && !has_function_call_output(request, &call_id)
                },
                sse(vec![
                    ev_response_created(&response_id),
                    ev_assistant_message(&format!("child-message-{index}"), "done"),
                    ev_completed(&response_id),
                ]),
            )
            .await,
        );
    }
    let call_ids = spawns.map(|(call_id, _, _, _)| call_id);
    let root_followup = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            call_ids
                .iter()
                .all(|call_id| has_function_call_output(request, call_id))
        },
        sse(vec![
            ev_response_created("root-followup-response"),
            ev_assistant_message("root-followup-message", "spawned"),
            ev_completed("root-followup-response"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_model("koffing").with_config(|config| {
        config
            .features
            .enable(Feature::Collab)
            .expect("test config should allow feature update");
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("test config should allow feature update");
        config.agent_max_threads = Some(6);
        config.multi_agent_v2.max_concurrent_threads_per_session = 8;
        config.multi_agent_v2.subagent_usage_hint_text = Some(SUBAGENT_HINT.to_string());
        config.multi_agent_v2.multi_agent_mode_hint_text = Some(MODE_HINT.to_string());
    });
    let test = builder.build_with_auto_env(&server).await?;
    test.submit_turn(ROOT_PROMPT).await?;

    let spawned_agent_paths = spawns
        .iter()
        .map(|(call_id, _, _, _)| {
            let output = root_followup
                .function_call_output_text(call_id)
                .expect("spawn_agent output");
            serde_json::from_str::<Value>(&output)
                .expect("spawn_agent output should be JSON")
                .get("task_name")
                .and_then(Value::as_str)
                .expect("spawn_agent output should include task_name")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        spawned_agent_paths,
        spawns
            .map(|(_, task_name, _, _)| format!("/root/{task_name}"))
            .to_vec()
    );

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if child_responses
                .iter()
                .all(|response| response.last_request().is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let child_delegation_state = [0, 1, 6].map(|index| {
        let request = child_responses[index]
            .requests()
            .into_iter()
            .next()
            .expect("child request");
        (
            request
                .tool_by_name(MULTI_AGENT_V2_NAMESPACE, "spawn_agent")
                .is_some(),
            request
                .tool_by_name(MULTI_AGENT_V2_NAMESPACE, "send_message")
                .is_some(),
            request.body_contains_text(SUBAGENT_HINT),
            request.body_contains_text(MODE_HINT),
        )
    });
    assert_eq!(
        child_delegation_state,
        [
            (true, true, true, true),
            (true, true, true, true),
            (false, true, true, true),
        ]
    );

    Ok(())
}
