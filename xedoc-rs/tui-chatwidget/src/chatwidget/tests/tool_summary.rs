use super::*;
use xedoc_config::types::ToolCallRenderingMode;

fn optimized_chat(chat: &mut ChatWidget) {
    chat.config.tui_tool_call_rendering = ToolCallRenderingMode::Optimized;
}

fn assert_mode_change_notice(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    expected: ToolCallRenderingMode,
) -> Vec<String> {
    let mut saw_notice = false;
    let mut saw_event = false;
    let mut other_history = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) => {
                let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
                if rendered.contains("Tool rendering mode:") {
                    saw_notice = true;
                } else {
                    other_history.push(rendered);
                }
            }
            AppEvent::ToolCallRenderingModeChanged { mode } => {
                assert_eq!(mode, expected);
                saw_event = true;
            }
            other => panic!("unexpected mode-change event: {other:?}"),
        }
    }
    assert!(saw_notice);
    assert!(saw_event);
    other_history
}

#[tokio::test]
async fn normal_mode_keeps_detailed_command_output() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.tui_tool_call_rendering = ToolCallRenderingMode::Normal;
    chat.on_task_started();

    let begin = begin_exec(&mut chat, "normal-call", "printf detailed");
    end_exec(
        &mut chat,
        begin,
        "detailed output\n",
        "",
        /*exit_code*/ 0,
    );

    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert_chatwidget_snapshot!("tool_summary_normal_mode_detailed_output", rendered);
}

#[tokio::test]
async fn optimized_mode_keeps_assistant_tail_and_shows_two_row_summary() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();
    chat.handle_streaming_delta("| Step | Owner |\n".to_string());
    assert!(chat.active_cell_is_stream_tail());
    let assistant_tail = active_blob(&chat);
    handle_agent_reasoning_delta(&mut chat, "**Thinking about the next step**");

    let _begin = begin_exec(&mut chat, "optimized-call", "printf hidden");

    assert_eq!(active_blob(&chat), assistant_tail);
    assert_eq!(
        chat.status_state.current_status.header,
        "Thinking about the next step"
    );
    let summary = chat
        .tool_call_summary
        .as_ref()
        .expect("optimized tool summary should be active")
        .cell()
        .display_lines(/*width*/ 80)
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_chatwidget_snapshot!("tool_summary_optimized_live", summary);
}

#[tokio::test]
async fn optimized_mode_does_not_persist_a_summary_without_following_output() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();

    let first = begin_exec(&mut chat, "success-call", "printf success");
    end_exec(
        &mut chat,
        first,
        "success output\n",
        "",
        /*exit_code*/ 0,
    );
    let second = begin_exec(&mut chat, "failed-call", "printf failure");
    end_exec(
        &mut chat,
        second,
        "failure output\n",
        "",
        /*exit_code*/ 1,
    );
    handle_turn_completed(&mut chat, "turn-1", /*duration_ms*/ None);

    assert!(drain_insert_history(&mut rx).is_empty());
}

#[tokio::test]
async fn optimized_mode_inserts_a_persistent_summary_before_following_output() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();

    let command = begin_exec(&mut chat, "command", "printf summary");
    end_exec(&mut chat, command, "", "", /*exit_code*/ 0);
    chat.on_web_search_begin("search".to_string());
    chat.on_web_search_end(
        "search".to_string(),
        "ratatui".to_string(),
        xedoc_app_server_protocol::WebSearchAction::Search {
            query: Some("ratatui".to_string()),
            queries: None,
        },
    );

    chat.handle_streaming_delta("The work is complete.".to_string());

    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(rendered.contains("1 web search performed."));
    assert!(!rendered.contains("Calls:"));
    assert!(!rendered.contains("summary"));
    assert!(chat.tool_call_summary.is_some());

    chat.stream_controller = None;
    chat.on_web_search_begin("second-search".to_string());
    chat.on_web_search_end(
        "second-search".to_string(),
        "ratatui themes".to_string(),
        xedoc_app_server_protocol::WebSearchAction::Search {
            query: Some("ratatui themes".to_string()),
            queries: None,
        },
    );
    chat.handle_streaming_delta("A second output block.".to_string());

    let second_rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(second_rendered.contains("1 web search performed."));
    assert!(!second_rendered.contains("2 web searches performed."));
}

#[tokio::test]
async fn optimized_mode_without_tools_commits_no_summary() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();
    handle_turn_completed(&mut chat, "turn-empty", /*duration_ms*/ None);

    assert!(chat.tool_call_summary.is_none());
    assert!(
        drain_insert_history(&mut rx)
            .iter()
            .flat_map(|lines| lines.iter())
            .all(|line| !line.to_string().contains("Tool:"))
    );
}

#[tokio::test]
async fn optimized_mode_resets_summary_between_turns() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();
    let first = begin_exec(&mut chat, "first-turn-call", "printf first");
    end_exec(&mut chat, first, "", "", /*exit_code*/ 0);
    handle_turn_completed(&mut chat, "turn-1", /*duration_ms*/ None);
    let interrupted = drain_insert_history(&mut rx);
    assert!(
        interrupted
            .iter()
            .flat_map(|lines| lines.iter())
            .all(|line| !line.to_string().contains("Calls:"))
    );

    chat.on_task_started();
    let second = begin_exec(&mut chat, "second-turn-call", "printf-second");
    assert_eq!(
        chat.tool_call_summary
            .as_ref()
            .expect("second-turn summary")
            .cell()
            .display_label(),
        "bash -lc printf-second"
    );
    end_exec(&mut chat, second, "", "", /*exit_code*/ 0);
    handle_turn_completed(&mut chat, "turn-2", /*duration_ms*/ None);
    let failed = drain_insert_history(&mut rx);
    assert!(
        failed
            .iter()
            .flat_map(|lines| lines.iter())
            .all(|line| !line.to_string().contains("Calls:"))
    );
}

#[tokio::test]
async fn optimized_mode_tracks_current_and_last_overlapping_calls() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();

    let first = begin_exec(&mut chat, "call-a", "printf-A");
    let second = begin_exec(&mut chat, "call-b", "printf-B");
    let current_label = chat
        .tool_call_summary
        .as_ref()
        .expect("overlapping-call summary")
        .cell()
        .display_label()
        .to_string();
    assert_eq!(current_label, "bash -lc printf-B");

    end_exec(&mut chat, second, "", "", /*exit_code*/ 0);
    let current_label = chat
        .tool_call_summary
        .as_ref()
        .expect("overlapping-call summary")
        .cell()
        .display_label()
        .to_string();
    assert_eq!(current_label, "bash -lc printf-A");

    end_exec(&mut chat, first, "", "", /*exit_code*/ 0);
    let current_label = chat
        .tool_call_summary
        .as_ref()
        .expect("overlapping-call summary")
        .cell()
        .display_label()
        .to_string();
    assert_eq!(current_label, "bash -lc printf-A");
}

#[tokio::test]
async fn optimized_mode_aggregates_web_search_without_query_cell() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();

    chat.on_web_search_begin("search-call".to_string());
    chat.on_web_search_end(
        "search-call".to_string(),
        "private query".to_string(),
        xedoc_app_server_protocol::WebSearchAction::Search {
            query: Some("private query".to_string()),
            queries: None,
        },
    );
    handle_turn_completed(&mut chat, "turn-search", /*duration_ms*/ None);

    let failed = drain_insert_history(&mut rx);
    assert!(
        failed
            .iter()
            .flat_map(|lines| lines.iter())
            .all(|line| !line.to_string().contains("Calls:"))
    );
}

#[tokio::test]
async fn optimized_tool_summary_is_hidden_when_the_turn_is_not_running() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.show_welcome_banner = false;
    optimized_chat(&mut chat);
    chat.on_task_started();
    chat.on_web_search_begin("search-call".to_string());
    chat.on_web_search_end(
        "search-call".to_string(),
        "private query".to_string(),
        xedoc_app_server_protocol::WebSearchAction::Search {
            query: Some("private query".to_string()),
            queries: None,
        },
    );
    assert!(chat.tool_call_summary.is_some());

    chat.turn_lifecycle.finish();
    chat.update_task_running_state();
    assert!(!chat.bottom_pane.is_task_running());

    let width = 80;
    let height = chat.desired_height(width);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .expect("create terminal");
    terminal
        .draw(|frame| chat.render(frame.area(), frame.buffer_mut()))
        .expect("render inactive tool summary");
    let rendered = normalized_backend_snapshot(terminal.backend());
    assert!(!rendered.contains("Searched the web"));
    assert_chatwidget_snapshot!(
        "optimized_tool_summary_hidden_without_active_turn",
        rendered
    );
}

#[tokio::test]
async fn normal_to_optimized_mode_switch_applies_on_next_turn() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.tui_tool_call_rendering = ToolCallRenderingMode::Normal;
    chat.on_task_started();

    let first = begin_exec(&mut chat, "normal-call", "printf normal");
    chat.dispatch_command_with_args(
        SlashCommand::ToolRendering,
        "optimized".to_string(),
        Vec::new(),
    );
    let flushed = assert_mode_change_notice(&mut rx, ToolCallRenderingMode::Optimized);
    assert_eq!(flushed.len(), 1);
    end_exec(
        &mut chat,
        first,
        "normal output\n",
        "",
        /*exit_code*/ 0,
    );
    let detailed = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(detailed.contains("normal output"));
    assert!(!detailed.contains("Calls:"));
    handle_turn_completed(&mut chat, "turn-normal", /*duration_ms*/ None);
    drain_insert_history(&mut rx);

    chat.on_task_started();
    let second = begin_exec(&mut chat, "optimized-call", "printf optimized");
    assert!(chat.tool_call_summary.is_some());
    end_exec(
        &mut chat,
        second,
        "optimized output\n",
        "",
        /*exit_code*/ 0,
    );
    handle_turn_completed(&mut chat, "turn-optimized", /*duration_ms*/ None);
    assert!(drain_insert_history(&mut rx).is_empty());
}

#[tokio::test]
async fn optimized_to_normal_mode_switch_preserves_active_turn_summary() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.tui_tool_call_rendering = ToolCallRenderingMode::Optimized;
    chat.on_task_started();

    let first = begin_exec(&mut chat, "optimized-call", "printf optimized");
    assert!(chat.tool_call_summary.is_some());
    chat.dispatch_command_with_args(
        SlashCommand::ToolRendering,
        "normal".to_string(),
        Vec::new(),
    );
    let flushed = assert_mode_change_notice(&mut rx, ToolCallRenderingMode::Normal);
    assert!(flushed.is_empty());
    end_exec(
        &mut chat,
        first,
        "optimized output\n",
        "",
        /*exit_code*/ 0,
    );
    handle_turn_completed(&mut chat, "turn-optimized", /*duration_ms*/ None);
    assert!(drain_insert_history(&mut rx).is_empty());

    chat.on_task_started();
    let second = begin_exec(&mut chat, "normal-call", "printf normal");
    assert!(chat.tool_call_summary.is_none());
    end_exec(
        &mut chat,
        second,
        "normal output\n",
        "",
        /*exit_code*/ 0,
    );
    let detailed = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(detailed.contains("normal output"));
    assert!(!detailed.contains("Calls:"));
}

#[tokio::test]
async fn optimized_mode_marks_in_progress_calls_failed_on_interrupt_and_failure() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();
    let _ = begin_exec(&mut chat, "interrupted-call", "sleep 1");
    handle_turn_interrupted(&mut chat, "turn-interrupted");

    let interrupted = drain_insert_history(&mut rx);
    assert!(
        interrupted
            .iter()
            .flat_map(|lines| lines.iter())
            .all(|line| !line.to_string().contains("Calls:"))
    );

    chat.on_task_started();
    let _ = begin_exec(&mut chat, "failed-call", "sleep 1");
    handle_error(&mut chat, "turn failed", None);
    let failed = drain_insert_history(&mut rx);
    assert!(
        failed
            .iter()
            .flat_map(|lines| lines.iter())
            .all(|line| !line.to_string().contains("Calls:"))
    );
}

#[tokio::test]
async fn optimized_mode_keeps_user_shell_detailed() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);
    chat.on_task_started();

    let begin = begin_exec_with_source(
        &mut chat,
        "user-shell-call",
        "printf user-shell",
        ExecCommandSource::UserShell,
    );
    end_exec(
        &mut chat,
        begin,
        "user shell output\n",
        "",
        /*exit_code*/ 0,
    );

    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<String>();
    assert!(chat.tool_call_summary.is_none());
    assert_chatwidget_snapshot!("tool_summary_user_shell_detailed", rendered);
}

#[tokio::test]
async fn optimized_replay_omits_command_only_tool_summary() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    optimized_chat(&mut chat);

    let mut turn = app_server_turn(
        "replay-turn",
        AppServerTurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    let replay_cwd = chat.config.cwd.clone();
    let command = |id: &str, status, output| AppServerThreadItem::CommandExecution {
        id: id.to_string(),
        command: "bash -lc 'printf replay'".to_string(),
        cwd: replay_cwd.clone().into(),
        process_id: None,
        source: ExecCommandSource::Agent,
        status,
        command_actions: Vec::new(),
        aggregated_output: output,
        exit_code: Some(0),
        duration_ms: Some(1),
    };
    turn.items = vec![
        command(
            "replayed-first",
            AppServerCommandExecutionStatus::Completed,
            Some("replayed output".to_string()),
        ),
        command(
            "replayed-second",
            AppServerCommandExecutionStatus::Completed,
            Some("replayed output".to_string()),
        ),
    ];

    chat.replay_thread_turns(vec![turn], ReplayKind::ThreadSnapshot);

    assert!(drain_insert_history(&mut rx).is_empty());
}
