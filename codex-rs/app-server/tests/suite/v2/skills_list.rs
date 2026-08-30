use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml_with_chatgpt_base_url;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SkillsChangedNotification;
use codex_app_server_protocol::SkillsExtraRootsSetParams;
use codex_app_server_protocol::SkillsExtraRootsSetResponse;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_exec_server::CODEX_EXEC_SERVER_URL_ENV_VAR;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const WATCHER_TIMEOUT: Duration = Duration::from_secs(20);

fn write_skill(root: &TempDir, name: &str) -> Result<()> {
    let skill_dir = root.path().join("skills").join(name);
    std::fs::create_dir_all(&skill_dir)?;
    let content = format!("---\nname: {name}\ndescription: {name} description\n---\n\n# Body\n");
    std::fs::write(skill_dir.join("SKILL.md"), content)?;
    Ok(())
}

async fn expect_skills_changed_notification(
    mcp: &mut TestAppServer,
    timeout_duration: Duration,
) -> Result<()> {
    let notification = timeout(
        timeout_duration,
        mcp.read_stream_until_notification_message("skills/changed"),
    )
    .await??;
    let params = notification
        .params
        .context("skills/changed params must be present")?;
    let notification: SkillsChangedNotification = serde_json::from_value(params)?;
    assert_eq!(notification, SkillsChangedNotification {});
    Ok(())
}

#[tokio::test]
async fn skills_list_skips_cwd_roots_when_environment_disabled() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    write_skill(&codex_home, "home-skill")?;
    let repo_skill_dir = cwd.path().join(".codex/skills/repo-skill");
    std::fs::create_dir_all(&repo_skill_dir)?;
    std::fs::write(
        repo_skill_dir.join("SKILL.md"),
        "---\nname: repo-skill\ndescription: from repo root\n---\n\n# Body\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[(CODEX_EXEC_SERVER_URL_ENV_VAR, Some("none"))])
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(response)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].cwd, cwd.path().to_path_buf());
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "home-skill")
    );
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "repo-skill")
    );
    Ok(())
}

#[tokio::test]
async fn skills_list_accepts_relative_cwds() -> Result<()> {
    let codex_home = TempDir::new()?;
    let relative_cwd = std::path::PathBuf::from("relative-cwd");
    std::fs::create_dir_all(codex_home.path().join(&relative_cwd))?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![relative_cwd.clone()],
            force_reload: true,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(response)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].cwd, relative_cwd);
    assert_eq!(data[0].errors, Vec::new());
    Ok(())
}

#[tokio::test]
async fn skills_list_preserves_requested_cwd_order() -> Result<()> {
    let codex_home = TempDir::new()?;
    let first_cwd = TempDir::new()?;
    let second_cwd = TempDir::new()?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![
                first_cwd.path().to_path_buf(),
                second_cwd.path().to_path_buf(),
            ],
            force_reload: true,
        })
        .await?;

    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(response)?;
    assert_eq!(
        data.iter()
            .map(|entry| entry.cwd.clone())
            .collect::<Vec<_>>(),
        vec![
            first_cwd.path().to_path_buf(),
            second_cwd.path().to_path_buf(),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn skills_list_uses_cached_result_until_force_reload() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    // Seed the cwd cache before the cwd-local skill exists.
    let first_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let first_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(first_request_id)),
    )
    .await??;
    let SkillsListResponse { data: first_data } = to_response(first_response)?;
    assert_eq!(first_data.len(), 1);
    assert!(
        first_data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "late-extra-skill")
    );

    let skill_dir = cwd.path().join(".codex/skills/late-extra-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: late-extra-skill\ndescription: late skill\n---\n\n# Body\n",
    )?;

    let second_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let second_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(second_request_id)),
    )
    .await??;
    let SkillsListResponse { data: second_data } = to_response(second_response)?;
    assert_eq!(second_data.len(), 1);
    assert!(
        second_data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "late-extra-skill")
    );

    let third_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let third_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(third_request_id)),
    )
    .await??;
    let SkillsListResponse { data: third_data } = to_response(third_response)?;
    assert_eq!(third_data.len(), 1);
    assert!(
        third_data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "late-extra-skill")
    );
    Ok(())
}

#[tokio::test]
async fn skills_extra_roots_set_updates_process_runtime_roots() -> Result<()> {
    let codex_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    let extra_root = TempDir::new()?;
    let extra_skills_root = extra_root.path().join("skills");
    let skill_dir = extra_skills_root.join("runtime-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: runtime-skill\ndescription: runtime skill\n---\n\n# Body\n",
    )?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let set_request_id = mcp
        .send_skills_extra_roots_set_request(SkillsExtraRootsSetParams {
            extra_roots: vec![AbsolutePathBuf::from_absolute_path(&extra_skills_root)?],
        })
        .await?;
    let set_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(set_request_id)),
    )
    .await??;
    let _: SkillsExtraRootsSetResponse = to_response(set_response)?;
    expect_skills_changed_notification(&mut mcp, DEFAULT_TIMEOUT).await?;

    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let skills_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(skills_request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(skills_response)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "runtime-skill")
    );

    let missing_root = extra_root.path().join("missing-skills");
    let reset_request_id = mcp
        .send_skills_extra_roots_set_request(SkillsExtraRootsSetParams {
            extra_roots: vec![AbsolutePathBuf::from_absolute_path(&missing_root)?],
        })
        .await?;
    let reset_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(reset_request_id)),
    )
    .await??;
    let _: SkillsExtraRootsSetResponse = to_response(reset_response)?;
    expect_skills_changed_notification(&mut mcp, DEFAULT_TIMEOUT).await?;

    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let skills_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(skills_request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(skills_response)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "runtime-skill")
    );

    let clear_request_id = mcp
        .send_skills_extra_roots_set_request(SkillsExtraRootsSetParams {
            extra_roots: Vec::new(),
        })
        .await?;
    let clear_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(clear_request_id)),
    )
    .await??;
    let _: SkillsExtraRootsSetResponse = to_response(clear_response)?;
    expect_skills_changed_notification(&mut mcp, DEFAULT_TIMEOUT).await?;
    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let skills_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(skills_request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(skills_response)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "runtime-skill")
    );

    drop(mcp);
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;
    let skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![cwd.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let skills_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(skills_request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(skills_response)?;
    assert_eq!(data.len(), 1);
    assert_eq!(data[0].errors, Vec::new());
    assert!(
        data[0]
            .skills
            .iter()
            .all(|skill| skill.name != "runtime-skill")
    );
    Ok(())
}

#[tokio::test]
async fn skills_changed_notification_is_emitted_after_skill_change() -> Result<()> {
    // TODO(anp): Remove after skill watching can bridge host-local storage into remote exec.
    skip_if_remote!(
        Ok(()),
        "host-local skill changes are not visible to remote executors"
    );

    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml_with_chatgpt_base_url(
        codex_home.path(),
        &server.uri(),
        &server.uri(),
    )?;
    write_skill(&codex_home, "demo")?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;
    let initial_skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![codex_home.path().to_path_buf()],
            force_reload: true,
        })
        .await?;
    let initial_skills_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(initial_skills_request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(initial_skills_response)?;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| { skill.name == "demo" && skill.description == "demo description" })
    );

    let thread_start_request_id = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: None,
            model_provider: None,
            allow_provider_model_fallback: false,
            service_tier: None,
            cwd: None,
            runtime_workspace_roots: None,
            approval_policy: None,
            sandbox: None,
            permissions: None,
            config: None,
            service_name: None,
            base_instructions: None,
            developer_instructions: None,
            personality: None,
            multi_agent_mode: None,
            ephemeral: None,
            history_mode: None,
            session_start_source: None,
            thread_source: None,
            dynamic_tools: None,
            environments: None,
            selected_capability_roots: None,
            mock_experimental_field: None,
            experimental_raw_events: false,
        })
        .await?;
    let _: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_start_request_id)),
    )
    .await??;

    let skill_path = codex_home
        .path()
        .join("skills")
        .join("demo")
        .join("SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: demo\ndescription: updated\n---\n\n# Updated\n",
    )?;

    let notification = timeout(
        WATCHER_TIMEOUT,
        mcp.read_stream_until_notification_message("skills/changed"),
    )
    .await??;
    let params = notification
        .params
        .context("skills/changed params must be present")?;
    let notification: SkillsChangedNotification = serde_json::from_value(params)?;

    assert_eq!(notification, SkillsChangedNotification {});
    let updated_skills_request_id = mcp
        .send_skills_list_request(SkillsListParams {
            cwds: vec![codex_home.path().to_path_buf()],
            force_reload: false,
        })
        .await?;
    let updated_skills_response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(updated_skills_request_id)),
    )
    .await??;
    let SkillsListResponse { data } = to_response(updated_skills_response)?;
    assert_eq!(data.len(), 1);
    assert!(
        data[0]
            .skills
            .iter()
            .any(|skill| skill.name == "demo" && skill.description == "updated")
    );
    Ok(())
}
