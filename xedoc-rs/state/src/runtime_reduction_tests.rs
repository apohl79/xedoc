use super::payload_kind_name;
use super::reducer_name;
use super::reduction_level_name;
use crate::SqliteConfig;
use crate::state_db_path;
use pretty_assertions::assert_eq;
use sqlx::Row;
use xedoc_tool_output_reduce::PayloadKind;
use xedoc_tool_output_reduce::ReducerId;
use xedoc_tool_output_reduce::ReductionLevel;
use xedoc_tool_output_reduce::ReductionRecord;
use xedoc_utils_absolute_path::test_support::PathExt;

#[test]
fn reduction_labels_are_stable_for_persistence_and_metrics() {
    assert_eq!(payload_kind_name(PayloadKind::Json), "json");
    assert_eq!(reduction_level_name(ReductionLevel::Balanced), "balanced");
    assert_eq!(reducer_name(&ReducerId::Dedup), "dedup");
}

#[tokio::test]
async fn reduction_record_round_trips_through_persistence() {
    let xedoc_home = super::test_support::unique_temp_dir();
    let runtime = super::StateRuntime::init(xedoc_home.clone(), "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    let read_pool = SqliteConfig::new_for_testing(xedoc_home.as_path().abs())
        .open_read_only_pool(&state_db_path(&xedoc_home))
        .await
        .expect("state database should open for reading");
    let expected = ReductionRecord {
        thread_id: Some("thread-round-trip".to_owned()),
        turn_id: Some("turn-round-trip".to_owned()),
        call_id: "call-round-trip".to_owned(),
        command_hash: Some("command-hash-round-trip".to_owned()),
        tool_name: "shell".to_owned(),
        kind: PayloadKind::Json,
        level: ReductionLevel::Aggressive,
        reducers_applied: vec![ReducerId::Normalize, ReducerId::Json, ReducerId::Dedup],
        bytes_in: 12_345,
        bytes_out: 6_789,
        est_tokens_in: 3_210,
        est_tokens_out: 1_234,
        duration_us: 987_654,
        spilled: true,
        spill_path: Some("/tmp/round-trip.txt".to_owned()),
        recorded_at: 1_700_000_123,
    };
    runtime.reduction_sink().try_record(expected.clone());

    let row = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(row) = sqlx::query(
                "SELECT thread_id, turn_id, call_id, command_hash, tool_name, kind, level, \
                 reducers_applied, bytes_in, bytes_out, est_tokens_in, est_tokens_out, duration_us, \
                 spilled, spill_path, recorded_at FROM tool_output_reductions \
                 WHERE call_id = ?",
            )
            .bind(&expected.call_id)
            .fetch_optional(&read_pool)
            .await
            .expect("reduction row query should succeed")
            {
                break row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("reduction row should be persisted");
    let actual = ReductionRecord {
        thread_id: row.try_get("thread_id").expect("thread_id column"),
        turn_id: row.try_get("turn_id").expect("turn_id column"),
        call_id: row.try_get("call_id").expect("call_id column"),
        command_hash: row.try_get("command_hash").expect("command_hash column"),
        tool_name: row.try_get("tool_name").expect("tool_name column"),
        kind: match row
            .try_get::<String, _>("kind")
            .expect("kind column")
            .as_str()
        {
            "json" => PayloadKind::Json,
            kind => panic!("unexpected persisted payload kind: {kind}"),
        },
        level: match row
            .try_get::<String, _>("level")
            .expect("level column")
            .as_str()
        {
            "aggressive" => ReductionLevel::Aggressive,
            level => panic!("unexpected persisted reduction level: {level}"),
        },
        reducers_applied: row
            .try_get::<String, _>("reducers_applied")
            .expect("reducers_applied column")
            .split(',')
            .map(|reducer| match reducer {
                "normalize" => ReducerId::Normalize,
                "json" => ReducerId::Json,
                "dedup" => ReducerId::Dedup,
                reducer => panic!("unexpected persisted reducer: {reducer}"),
            })
            .collect(),
        bytes_in: row.try_get::<i64, _>("bytes_in").expect("bytes_in column") as u64,
        bytes_out: row
            .try_get::<i64, _>("bytes_out")
            .expect("bytes_out column") as u64,
        est_tokens_in: row.try_get("est_tokens_in").expect("est_tokens_in column"),
        est_tokens_out: row
            .try_get("est_tokens_out")
            .expect("est_tokens_out column"),
        duration_us: row
            .try_get::<i64, _>("duration_us")
            .expect("duration_us column") as u64,
        spilled: row.try_get("spilled").expect("spilled column"),
        spill_path: row.try_get("spill_path").expect("spill_path column"),
        recorded_at: row.try_get("recorded_at").expect("recorded_at column"),
    };
    assert_eq!(actual, expected);

    read_pool.close().await;
    runtime.close().await;
    let _ = tokio::fs::remove_dir_all(xedoc_home).await;
}

#[tokio::test]
async fn reduction_insights_aggregate_attribution_retrieval_and_thread_scope() {
    let xedoc_home = super::test_support::unique_temp_dir();
    let runtime = super::StateRuntime::init(xedoc_home.clone(), "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    let sink = runtime.reduction_sink();

    let record = |thread_id: &str,
                  call_id: &str,
                  command_hash: Option<&str>,
                  tool_name: &str,
                  kind: PayloadKind,
                  reducers_applied: Vec<ReducerId>,
                  bytes_in: u64,
                  bytes_out: u64,
                  est_tokens_in: i64,
                  est_tokens_out: i64,
                  spilled: bool| ReductionRecord {
        thread_id: Some(thread_id.to_string()),
        turn_id: Some(format!("turn-{call_id}")),
        call_id: call_id.to_string(),
        command_hash: command_hash.map(str::to_owned),
        tool_name: tool_name.to_string(),
        kind,
        level: ReductionLevel::Balanced,
        reducers_applied,
        bytes_in,
        bytes_out,
        est_tokens_in,
        est_tokens_out,
        duration_us: 10,
        spilled,
        spill_path: spilled.then(|| format!("/tmp/{call_id}.txt")),
        recorded_at: 1,
    };

    sink.try_record(record(
        "thread-a",
        "call-json-large",
        Some("same-command"),
        "shell",
        PayloadKind::Json,
        vec![ReducerId::Json, ReducerId::Dedup],
        1_000,
        400,
        250,
        100,
        true,
    ));
    sink.try_record(record(
        "thread-a",
        "call-json-small",
        Some("same-command"),
        "shell",
        PayloadKind::Json,
        vec![ReducerId::Json],
        800,
        600,
        200,
        150,
        false,
    ));
    sink.try_record(record(
        "thread-a",
        "call-log",
        None,
        "mcp",
        PayloadKind::Log,
        vec![ReducerId::Log, ReducerId::Budget],
        600,
        300,
        150,
        75,
        true,
    ));
    sink.try_record(record(
        "thread-b",
        "call-prose",
        Some("same-command"),
        "apply_patch",
        PayloadKind::Prose,
        vec![ReducerId::Normalize],
        400,
        200,
        100,
        50,
        false,
    ));
    runtime
        .record_tool_output_retrieval("call-json-large", "/tmp/call-json-large.txt")
        .await
        .expect("retrieval should persist");
    runtime
        .record_tool_output_retrieval("call-json-large", "/tmp/call-json-large.txt")
        .await
        .expect("duplicate retrieval should persist");

    // The reduction sink is deliberately asynchronous; polling the public
    // aggregate API makes this test independent of implementation details.
    let insights = loop {
        let insights = runtime
            .tool_output_reduction_insights(None)
            .await
            .expect("insights query should succeed");
        if insights.top_reductions.len() == 4 {
            break insights;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };

    let mut all = insights.clone();
    all.by_kind.sort_by(|a, b| a.dimension.cmp(&b.dimension));
    all.by_reducer.sort_by(|a, b| a.dimension.cmp(&b.dimension));
    all.by_tool.sort_by(|a, b| a.dimension.cmp(&b.dimension));
    all.top_reductions.sort_by(|a, b| a.call_id.cmp(&b.call_id));
    assert_eq!(
        all,
        super::ToolOutputReductionInsights {
            by_kind: vec![
                super::ToolOutputReductionBreakdown {
                    dimension: "json".to_string(),
                    reductions: 2,
                    bytes_in: 1_800,
                    bytes_out: 1_000,
                    retrievals: 1,
                    reruns: 1,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "log".to_string(),
                    reductions: 1,
                    bytes_in: 600,
                    bytes_out: 300,
                    retrievals: 0,
                    reruns: 0,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "prose".to_string(),
                    reductions: 1,
                    bytes_in: 400,
                    bytes_out: 200,
                    retrievals: 0,
                    reruns: 0,
                },
            ],
            by_reducer: vec![
                super::ToolOutputReductionBreakdown {
                    dimension: "budget".to_string(),
                    reductions: 1,
                    bytes_in: 300,
                    bytes_out: 150,
                    retrievals: 0,
                    reruns: 0,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "dedup".to_string(),
                    reductions: 1,
                    bytes_in: 500,
                    bytes_out: 200,
                    retrievals: 1,
                    reruns: 1,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "json".to_string(),
                    reductions: 2,
                    bytes_in: 1_300,
                    bytes_out: 800,
                    retrievals: 1,
                    reruns: 1,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "log".to_string(),
                    reductions: 1,
                    bytes_in: 300,
                    bytes_out: 150,
                    retrievals: 0,
                    reruns: 0,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "normalize".to_string(),
                    reductions: 1,
                    bytes_in: 400,
                    bytes_out: 200,
                    retrievals: 0,
                    reruns: 0,
                },
            ],
            by_tool: vec![
                super::ToolOutputReductionBreakdown {
                    dimension: "apply_patch".to_string(),
                    reductions: 1,
                    bytes_in: 400,
                    bytes_out: 200,
                    retrievals: 0,
                    reruns: 0,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "mcp".to_string(),
                    reductions: 1,
                    bytes_in: 600,
                    bytes_out: 300,
                    retrievals: 0,
                    reruns: 0,
                },
                super::ToolOutputReductionBreakdown {
                    dimension: "shell".to_string(),
                    reductions: 2,
                    bytes_in: 1_800,
                    bytes_out: 1_000,
                    retrievals: 1,
                    reruns: 1,
                },
            ],
            top_reductions: vec![
                super::ToolOutputReductionTop {
                    call_id: "call-json-large".to_string(),
                    tool_name: "shell".to_string(),
                    kind: "json".to_string(),
                    bytes_in: 1_000,
                    bytes_out: 400,
                    tokens_saved: 150,
                },
                super::ToolOutputReductionTop {
                    call_id: "call-json-small".to_string(),
                    tool_name: "shell".to_string(),
                    kind: "json".to_string(),
                    bytes_in: 800,
                    bytes_out: 600,
                    tokens_saved: 50,
                },
                super::ToolOutputReductionTop {
                    call_id: "call-log".to_string(),
                    tool_name: "mcp".to_string(),
                    kind: "log".to_string(),
                    bytes_in: 600,
                    bytes_out: 300,
                    tokens_saved: 75,
                },
                super::ToolOutputReductionTop {
                    call_id: "call-prose".to_string(),
                    tool_name: "apply_patch".to_string(),
                    kind: "prose".to_string(),
                    bytes_in: 400,
                    bytes_out: 200,
                    tokens_saved: 50,
                },
            ],
            retrievals: 1,
            spilled: 2,
        }
    );

    let thread_a = runtime
        .tool_output_reduction_insights(Some("thread-a"))
        .await
        .expect("thread-scoped insights should succeed");
    assert_eq!(thread_a.retrievals, 1);
    assert_eq!(thread_a.spilled, 2);
    assert!(
        thread_a
            .top_reductions
            .iter()
            .all(|item| item.call_id != "call-prose")
    );

    runtime
        .reset_tool_output_reduction_stats()
        .await
        .expect("reset should succeed");
    assert_eq!(
        runtime
            .tool_output_reduction_insights(None)
            .await
            .expect("empty insights query should succeed"),
        super::ToolOutputReductionInsights {
            by_kind: Vec::new(),
            by_reducer: Vec::new(),
            by_tool: Vec::new(),
            top_reductions: Vec::new(),
            retrievals: 0,
            spilled: 0,
        }
    );
    runtime.close().await;
    let _ = tokio::fs::remove_dir_all(xedoc_home).await;
}

#[tokio::test]
async fn reruns_are_limited_to_two_turns_and_thread_scoped() {
    let xedoc_home = super::test_support::unique_temp_dir();
    let runtime = super::StateRuntime::init(xedoc_home, "test-provider".to_string())
        .await
        .expect("state runtime should initialize");
    let sink = runtime.reduction_sink();
    let record = |thread_id: &str, turn_id: &str, call_id: &str| ReductionRecord {
        thread_id: Some(thread_id.to_owned()),
        turn_id: Some(turn_id.to_owned()),
        call_id: call_id.to_owned(),
        command_hash: Some("same-command".to_owned()),
        tool_name: "shell".to_owned(),
        kind: PayloadKind::Log,
        level: ReductionLevel::Balanced,
        reducers_applied: vec![ReducerId::Log],
        bytes_in: 100,
        bytes_out: 50,
        est_tokens_in: 25,
        est_tokens_out: 12,
        duration_us: 1,
        spilled: false,
        spill_path: None,
        recorded_at: 1,
    };
    sink.try_record(record("thread-a", "turn-1", "call-1"));
    sink.try_record(record("thread-a", "turn-2", "call-2"));
    sink.try_record(record("thread-a", "turn-3", "call-3"));
    sink.try_record(record("thread-a", "turn-4", "call-4"));
    sink.try_record(record("thread-b", "turn-2", "call-other-thread"));

    let insights = loop {
        let insights = runtime
            .tool_output_reduction_insights(None)
            .await
            .expect("insights query should succeed");
        if insights
            .by_kind
            .first()
            .is_some_and(|item| item.reductions == 5)
        {
            break insights;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    let log = insights
        .by_kind
        .iter()
        .find(|item| item.dimension == "log")
        .expect("log breakdown should exist");
    assert_eq!(log.reruns, 3);
}
