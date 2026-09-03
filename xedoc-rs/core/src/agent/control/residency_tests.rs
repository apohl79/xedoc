use crate::ThreadManager;
use crate::agent::AgentControl;
use crate::config::Config;
use crate::config_test_support::test_config;
use crate::thread_manager::ThreadManagerState;
use crate::xedoc_thread::XedocThread;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use xedoc_features::Feature;
use xedoc_login::XedocAuth;
use xedoc_protocol::ThreadId;
use xedoc_protocol::error::XedocErr;
use xedoc_protocol::protocol::EventMsg;
use xedoc_protocol::protocol::SessionSource;
use xedoc_protocol::protocol::SubAgentSource;
use xedoc_protocol::protocol::ThreadSource;
use xedoc_protocol::protocol::TurnAbortReason;
use xedoc_protocol::protocol::TurnAbortedEvent;
use xedoc_protocol::protocol::TurnCompleteEvent;

#[tokio::test]
async fn residency_slot_reservation_ignores_stale_removed_v2_agent() {
    let mut config = test_config().await;
    let _ = config.features.enable(Feature::MultiAgentV2);
    config.multi_agent_v2.max_loaded_threads_per_session = 1;
    let temp_home = tempfile::tempdir().expect("create temp home");
    config.xedoc_home = temp_home.path().to_path_buf().try_into().unwrap();
    config.cwd = temp_home.path().to_path_buf().try_into().unwrap();
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        XedocAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.xedoc_home.to_path_buf(),
        Arc::new(xedoc_exec_server::EnvironmentManager::default_for_tests()),
    );
    let root = manager
        .start_thread(config.clone())
        .await
        .expect("start root thread");
    let control = manager.agent_control();
    let state = control.upgrade().expect("thread manager should be live");

    let first_slot = control
        .reserve_v2_residency_slot(&state, &config, /*protected_thread_id*/ None)
        .await
        .expect("first resident slot");
    let first =
        spawn_v2_subagent(&control, &state, config.clone(), root.thread_id, "worker-1").await;
    first_slot.commit(first.thread_id);
    assert!(manager.remove_thread(&first.thread_id).await.is_some());

    let second_slot = control
        .reserve_v2_residency_slot(&state, &config, /*protected_thread_id*/ None)
        .await
        .expect("stale resident should not consume session capacity");
    match manager.get_thread(first.thread_id).await {
        Err(XedocErr::ThreadNotFound(thread_id)) => assert_eq!(thread_id, first.thread_id),
        Err(err) => panic!("expected evicted thread to be missing, got {err:?}"),
        Ok(_) => panic!("expected evicted thread to be missing"),
    }
    let second = spawn_v2_subagent(&control, &state, config, root.thread_id, "worker-2").await;
    second_slot.commit(second.thread_id);

    assert!(manager.get_thread(root.thread_id).await.is_ok());
    assert!(manager.get_thread(second.thread_id).await.is_ok());
}

#[tokio::test]
async fn interrupted_v2_agent_is_lost_after_residency_eviction() {
    let mut config = test_config().await;
    let _ = config.features.enable(Feature::MultiAgentV2);
    config.multi_agent_v2.max_loaded_threads_per_session = 1;
    let temp_home = tempfile::tempdir().expect("create temp home");
    config.xedoc_home = temp_home.path().to_path_buf().try_into().unwrap();
    config.cwd = temp_home.path().to_path_buf().try_into().unwrap();
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        XedocAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.xedoc_home.to_path_buf(),
        Arc::new(xedoc_exec_server::EnvironmentManager::default_for_tests()),
    );
    let root = manager
        .start_thread(config.clone())
        .await
        .expect("start root thread");
    let control = manager.agent_control();
    let state = control.upgrade().expect("thread manager should be live");

    let first_slot = control
        .reserve_v2_residency_slot(&state, &config, /*protected_thread_id*/ None)
        .await
        .expect("first resident slot");
    let first =
        spawn_v2_subagent(&control, &state, config.clone(), root.thread_id, "worker-1").await;
    first_slot.commit(first.thread_id);
    first
        .thread
        .inject_user_message_without_turn("resident history".to_string())
        .await;
    assert!(
        !first
            .thread
            .session
            .clone_history()
            .await
            .raw_items()
            .is_empty()
    );
    mark_thread_interrupted(first.thread.as_ref()).await;

    let second_slot = control
        .reserve_v2_residency_slot(&state, &config, /*protected_thread_id*/ None)
        .await
        .expect("second resident slot should evict the first interrupted idle agent");
    match manager.get_thread(first.thread_id).await {
        Err(XedocErr::ThreadNotFound(thread_id)) => assert_eq!(thread_id, first.thread_id),
        Err(err) => panic!("expected evicted thread to be missing, got {err:?}"),
        Ok(_) => panic!("expected evicted thread to be missing"),
    }
    assert!(
        first
            .thread
            .session
            .clone_history()
            .await
            .raw_items()
            .is_empty(),
        "unload should clear resident history even while another thread Arc is retained"
    );
    let second =
        spawn_v2_subagent(&control, &state, config.clone(), root.thread_id, "worker-2").await;
    second_slot.commit(second.thread_id);
    mark_thread_completed(second.thread.as_ref()).await;

    let err = control
        .ensure_v2_agent_loaded(config, first.thread_id)
        .await
        .expect_err("evicted interrupted agent should stay lost");
    match err {
        XedocErr::ThreadNotFound(thread_id) => assert_eq!(thread_id, first.thread_id),
        err => panic!("expected ThreadNotFound, got {err:?}"),
    }

    assert!(manager.get_thread(root.thread_id).await.is_ok());
    assert!(manager.get_thread(second.thread_id).await.is_ok());
    match manager.get_thread(first.thread_id).await {
        Err(XedocErr::ThreadNotFound(thread_id)) => assert_eq!(thread_id, first.thread_id),
        Err(err) => panic!("expected evicted thread to be missing, got {err:?}"),
        Ok(_) => panic!("expected evicted thread to be missing"),
    }
}

#[tokio::test]
async fn residency_limit_is_scoped_to_each_root_session() {
    let mut config = test_config().await;
    let _ = config.features.enable(Feature::MultiAgentV2);
    config.multi_agent_v2.max_loaded_threads_per_session = 1;
    let temp_home = tempfile::tempdir().expect("create temp home");
    config.xedoc_home = temp_home.path().to_path_buf().try_into().unwrap();
    config.cwd = temp_home.path().to_path_buf().try_into().unwrap();
    let manager = ThreadManager::with_models_provider_and_home_for_tests(
        XedocAuth::from_api_key("dummy"),
        config.model_provider.clone(),
        config.xedoc_home.to_path_buf(),
        Arc::new(xedoc_exec_server::EnvironmentManager::default_for_tests()),
    );
    let first_root = manager
        .start_thread(config.clone())
        .await
        .expect("start first root thread");
    let second_root = manager
        .start_thread(config.clone())
        .await
        .expect("start second root thread");
    let first_control = first_root.thread.session.services.agent_control.clone();
    let second_control = second_root.thread.session.services.agent_control.clone();
    let state = first_control
        .upgrade()
        .expect("thread manager should be live");

    let first_slot = first_control
        .reserve_v2_residency_slot(&state, &config, /*protected_thread_id*/ None)
        .await
        .expect("first session resident slot");
    let first_child = spawn_v2_subagent(
        &first_control,
        &state,
        config.clone(),
        first_root.thread_id,
        "first-worker",
    )
    .await;
    first_slot.commit(first_child.thread_id);
    mark_thread_completed(first_child.thread.as_ref()).await;

    let second_slot = second_control
        .reserve_v2_residency_slot(&state, &config, /*protected_thread_id*/ None)
        .await
        .expect("second session should have an independent resident slot");
    let second_child = spawn_v2_subagent(
        &second_control,
        &state,
        config,
        second_root.thread_id,
        "second-worker",
    )
    .await;
    second_slot.commit(second_child.thread_id);

    assert!(manager.get_thread(first_child.thread_id).await.is_ok());
    assert!(manager.get_thread(second_child.thread_id).await.is_ok());
}

async fn spawn_v2_subagent(
    control: &AgentControl,
    state: &Arc<ThreadManagerState>,
    config: Config,
    parent_thread_id: ThreadId,
    label: &str,
) -> crate::thread_manager::NewThread {
    state
        .spawn_new_thread_with_source(
            config,
            control.clone(),
            SessionSource::SubAgent(SubAgentSource::Other(label.to_string())),
            /*history_mode*/ None,
            Some(parent_thread_id),
            /*forked_from_thread_id*/ None,
            Some(ThreadSource::Subagent),
            /*metrics_service_name*/ None,
            /*inherited_environments*/ None,
            /*inherited_exec_policy*/ None,
            /*environments*/ None,
        )
        .await
        .expect("spawn v2 subagent")
}

async fn mark_thread_completed(thread: &XedocThread) {
    let turn = thread.session.new_default_turn().await;
    thread
        .session
        .send_event(
            turn.as_ref(),
            EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: turn.sub_id.clone(),
                started_at: None,
                last_agent_message: Some("done".to_string()),
                error: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            }),
        )
        .await;
    clear_active_turn(thread).await;
}

async fn mark_thread_interrupted(thread: &XedocThread) {
    let turn = thread.session.new_default_turn().await;
    thread
        .session
        .send_event(
            turn.as_ref(),
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id: Some(turn.sub_id.clone()),
                started_at: None,
                reason: TurnAbortReason::Interrupted,
                completed_at: None,
                duration_ms: None,
            }),
        )
        .await;
    clear_active_turn(thread).await;
}

async fn clear_active_turn(thread: &XedocThread) {
    // The fixture has no task runner to clear the turn after the terminal event.
    *thread.session.active_turn.lock().await = None;
}
