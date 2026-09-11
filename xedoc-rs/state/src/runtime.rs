use crate::GOALS_DB_FILENAME;
use crate::LOGS_DB_FILENAME;
use crate::LogEntry;
use crate::LogQuery;
use crate::LogRow;
use crate::STATE_DB_FILENAME;
use crate::SortKey;
use crate::SqliteConfig;
use crate::THREAD_HISTORY_DB_FILENAME;
use crate::ThreadMetadata;
use crate::ThreadMetadataBuilder;
use crate::ThreadsPage;
use crate::apply_rollout_item;
use crate::migrations::repair_legacy_state_migration_versions;
use crate::migrations::runtime_goals_migrator;
use crate::migrations::runtime_logs_migrator;
use crate::migrations::runtime_state_migrator;
use crate::migrations::runtime_thread_history_migrator;
use crate::model::ThreadRow;
use crate::model::anchor_from_item;
use crate::model::datetime_to_epoch_millis;
use crate::model::datetime_to_epoch_seconds;
use crate::model::epoch_millis_to_datetime;
use crate::paths::file_modified_time_utc;
use crate::telemetry::DbKind;
use crate::telemetry::DbTelemetry;
use chrono::DateTime;
use chrono::Utc;
use serde_json::Value;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqliteConnection;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::warn;
use xedoc_protocol::ThreadId;
use xedoc_protocol::protocol::RolloutItem;
use xedoc_tool_output_reduce::PayloadKind;
use xedoc_tool_output_reduce::ReducerId;
use xedoc_tool_output_reduce::ReductionLevel;
use xedoc_tool_output_reduce::ReductionRecord;
use xedoc_utils_absolute_path::AbsolutePathBuf;

mod backfill;
mod goals;
mod logs;
mod model_router;
mod recovery;
#[cfg(test)]
pub(crate) mod test_support;
mod threads;

pub use goals::GoalAccountingMode;
pub use goals::GoalAccountingOutcome;
pub use goals::GoalStore;
pub use goals::GoalUpdate;
pub use recovery::RuntimeDbBackup;
pub use recovery::backup_runtime_db_for_fresh_start;
pub use recovery::is_sqlite_corruption_error;
pub use recovery::runtime_db_path_for_corruption_error;
pub use recovery::sqlite_error_detail_is_corruption;
pub use recovery::sqlite_error_detail_is_lock;
pub use threads::ThreadFilterOptions;

// "Partition" is the retained-log-content bucket we cap at 10 MiB:
// - one bucket per non-null thread_id
// - one bucket per threadless (thread_id IS NULL) non-null process_uuid
// - one bucket for threadless rows with process_uuid IS NULL
// This budget tracks each row's persisted rendered log body plus non-body
// metadata, rather than the exact sum of all persisted SQLite column bytes.
const LOG_PARTITION_SIZE_LIMIT_BYTES: i64 = 10 * 1024 * 1024;
const LOG_PARTITION_ROW_LIMIT: i64 = 1_000;
const TOOL_OUTPUT_REDUCTION_RETENTION_DAYS: i64 = 30;
const TOOL_OUTPUT_REDUCTION_ROW_LIMIT: i64 = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolOutputReductionBreakdown {
    pub dimension: String,
    pub reductions: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub retrievals: i64,
    pub reruns: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutputReductionModelBreakdown {
    pub dimension: String,
    pub reductions: i64,
    pub tokens_saved: i64,
    pub cost_saved_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutputReductionTop {
    pub call_id: String,
    pub tool_name: String,
    pub kind: String,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub tokens_saved: i64,
    pub cost_saved_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutputReductionInsights {
    pub by_kind: Vec<ToolOutputReductionBreakdown>,
    pub by_reducer: Vec<ToolOutputReductionBreakdown>,
    pub by_tool: Vec<ToolOutputReductionBreakdown>,
    pub top_reductions: Vec<ToolOutputReductionTop>,
    pub retrievals: i64,
    pub spilled: i64,
    pub by_model: Vec<ToolOutputReductionModelBreakdown>,
    pub cost_saved_usd: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutputReductionReportModel {
    pub model_slug: Option<String>,
    pub reductions: i64,
    pub tokens_saved: i64,
    pub cost_saved_usd: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutputReductionReportDay {
    pub day: i64,
    pub partial: bool,
    pub by_model: Vec<ToolOutputReductionReportModel>,
    pub reductions: i64,
    pub tokens_saved: i64,
    pub cost_saved_usd: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutputReductionReport {
    pub since_day: i64,
    pub until_day: i64,
    pub days: Vec<ToolOutputReductionReportDay>,
    pub reductions: i64,
    pub tokens_saved: i64,
    pub cost_saved_usd: f64,
}

#[derive(Clone, Copy)]
enum ReductionBreakdownDimension {
    Kind,
    Tool,
}

impl ReductionBreakdownDimension {
    fn expression(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Tool => "tool_name",
        }
    }
}

#[derive(Clone, Copy)]
struct RuntimeDbSpec {
    label: &'static str,
    filename: &'static str,
    kind: DbKind,
    open_phase: &'static str,
    migrate_phase: &'static str,
}

impl RuntimeDbSpec {
    fn path(self, xedoc_home: &Path) -> PathBuf {
        xedoc_home.join(self.filename)
    }
}

const STATE_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "state DB",
    filename: STATE_DB_FILENAME,
    kind: DbKind::State,
    open_phase: "open_state",
    migrate_phase: "migrate_state",
};

const LOGS_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "log DB",
    filename: LOGS_DB_FILENAME,
    kind: DbKind::Logs,
    open_phase: "open_logs",
    migrate_phase: "migrate_logs",
};

const GOALS_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "goals DB",
    filename: GOALS_DB_FILENAME,
    kind: DbKind::Goals,
    open_phase: "open_goals",
    migrate_phase: "migrate_goals",
};

const THREAD_HISTORY_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "thread history DB",
    filename: THREAD_HISTORY_DB_FILENAME,
    kind: DbKind::ThreadHistory,
    open_phase: "open_thread_history",
    migrate_phase: "migrate_thread_history",
};

const RUNTIME_DBS: [RuntimeDbSpec; 4] = [STATE_DB, LOGS_DB, GOALS_DB, THREAD_HISTORY_DB];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDbPath {
    pub label: &'static str,
    pub path: PathBuf,
}

#[derive(Clone)]
pub struct StateRuntime {
    xedoc_home: PathBuf,
    default_provider: String,
    pool: Arc<sqlx::SqlitePool>,
    logs_pool: Arc<sqlx::SqlitePool>,
    thread_goals: GoalStore,
    thread_updated_at_millis: Arc<AtomicI64>,
    thread_recency_at_millis: Arc<AtomicI64>,
    reduction_tx: mpsc::Sender<ReductionRecord>,
    retrieval_tx: mpsc::Sender<(String, String)>,
}

const REDUCTION_QUEUE_CAPACITY: usize = 256;

struct RuntimeReductionSink {
    tx: mpsc::Sender<ReductionRecord>,
    retrieval_tx: mpsc::Sender<(String, String)>,
}

impl xedoc_tool_output_reduce::ReductionSink for RuntimeReductionSink {
    fn try_record(&self, record: ReductionRecord) {
        let kind = payload_kind_name(record.kind);
        let level = reduction_level_name(record.level);
        let reducer = record.reducers_applied.first().map_or("none", reducer_name);
        xedoc_otel::record_tool_output_reduction(
            record.est_tokens_in,
            record.est_tokens_out,
            record.duration_us,
            &record.tool_name,
            kind,
            level,
            reducer,
        );
        let _ = self.tx.try_send(record);
    }

    fn try_record_retrieval(&self, call_id: &str, spill_path: &str) {
        xedoc_otel::record_tool_output_retrieval("shell");
        let _ = self
            .retrieval_tx
            .try_send((call_id.to_owned(), spill_path.to_owned()));
    }
}

impl StateRuntime {
    /// Return the bounded, non-blocking sink used by ingestion paths.
    pub fn reduction_sink(&self) -> Arc<dyn xedoc_tool_output_reduce::ReductionSink> {
        Arc::new(RuntimeReductionSink {
            tx: self.reduction_tx.clone(),
            retrieval_tx: self.retrieval_tx.clone(),
        })
    }

    /// Return lifetime aggregate estimated token savings and reduction count.
    ///
    /// Rows are retained in a bounded rolling window; callers must label this
    /// as lifetime-within-retention rather than as a per-session statistic.
    pub async fn tool_output_reduction_stats(&self) -> anyhow::Result<(i64, i64)> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count, COALESCE(SUM(est_tokens_in - est_tokens_out), 0) AS saved \
             FROM tool_output_reductions",
        )
        .fetch_one(self.pool.as_ref())
        .await?;
        Ok((row.try_get("count")?, row.try_get("saved")?))
    }

    /// Return reduction count and estimated savings for one thread within retention.
    pub async fn tool_output_reduction_stats_for_thread(
        &self,
        thread_id: &str,
    ) -> anyhow::Result<(i64, i64)> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count, COALESCE(SUM(est_tokens_in - est_tokens_out), 0) AS saved \
             FROM tool_output_reductions WHERE thread_id = ?",
        )
        .bind(thread_id)
        .fetch_one(self.pool.as_ref())
        .await?;
        Ok((row.try_get("count")?, row.try_get("saved")?))
    }

    /// Return bounded effectiveness breakdowns for the optimizer dashboard.
    ///
    /// Only aggregate metadata is returned; tool output bodies and spill paths are
    /// intentionally excluded. Results are capped to keep API responses bounded.
    pub async fn tool_output_reduction_insights(
        &self,
        thread_id: Option<&str>,
    ) -> anyhow::Result<ToolOutputReductionInsights> {
        let by_kind = self
            .reduction_breakdown(ReductionBreakdownDimension::Kind, thread_id)
            .await?;
        let by_reducer = self.reduction_breakdown_by_reducer(thread_id).await?;
        let by_tool = self
            .reduction_breakdown(ReductionBreakdownDimension::Tool, thread_id)
            .await?;
        let mut model_query = QueryBuilder::<Sqlite>::new(
            "SELECT COALESCE(model_slug, 'unknown') AS dimension, COUNT(*) AS reductions, \
             COALESCE(SUM(est_tokens_in - est_tokens_out), 0) AS tokens_saved, \
             SUM(CASE WHEN input_price_per_1m IS NULL THEN NULL \
                 ELSE (est_tokens_in - est_tokens_out) * input_price_per_1m / 1000000.0 END) \
                 AS cost_saved_usd FROM tool_output_reductions",
        );
        if let Some(thread_id) = thread_id {
            model_query.push(" WHERE thread_id = ").push_bind(thread_id);
        }
        model_query.push(" GROUP BY model_slug ORDER BY reductions DESC LIMIT 20");
        let by_model = model_query
            .build()
            .fetch_all(self.pool.as_ref())
            .await?
            .into_iter()
            .map(|row| {
                Ok(ToolOutputReductionModelBreakdown {
                    dimension: row.try_get("dimension")?,
                    reductions: row.try_get("reductions")?,
                    tokens_saved: row.try_get("tokens_saved")?,
                    cost_saved_usd: row.try_get("cost_saved_usd")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut cost_query = QueryBuilder::<Sqlite>::new(
            "SELECT COALESCE(SUM(CASE WHEN input_price_per_1m IS NULL THEN 0.0 \
             ELSE (est_tokens_in - est_tokens_out) * input_price_per_1m / 1000000.0 END), 0.0) \
             AS cost_saved_usd FROM tool_output_reductions",
        );
        if let Some(thread_id) = thread_id {
            cost_query.push(" WHERE thread_id = ").push_bind(thread_id);
        }
        let cost_saved_usd = cost_query
            .build()
            .fetch_one(self.pool.as_ref())
            .await?
            .try_get("cost_saved_usd")?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT call_id, tool_name, kind, bytes_in, bytes_out, \
             (est_tokens_in - est_tokens_out) AS tokens_saved, \
             CASE WHEN input_price_per_1m IS NULL THEN NULL \
                  ELSE (est_tokens_in - est_tokens_out) * input_price_per_1m / 1000000.0 END \
                  AS cost_saved_usd \
             FROM tool_output_reductions",
        );
        if let Some(thread_id) = thread_id {
            query.push(" WHERE thread_id = ").push_bind(thread_id);
        }
        query.push(" ORDER BY (est_tokens_in - est_tokens_out) DESC LIMIT 10");
        let rows = query.build().fetch_all(self.pool.as_ref()).await?;
        let top_reductions = rows
            .into_iter()
            .map(|row| {
                Ok(ToolOutputReductionTop {
                    call_id: row.try_get("call_id")?,
                    tool_name: row.try_get("tool_name")?,
                    kind: row.try_get("kind")?,
                    bytes_in: row.try_get("bytes_in")?,
                    bytes_out: row.try_get("bytes_out")?,
                    tokens_saved: row.try_get("tokens_saved")?,
                    cost_saved_usd: row.try_get("cost_saved_usd")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut count_query = sqlx::query(
            "SELECT COUNT(DISTINCT r.call_id) AS count FROM tool_output_retrievals r \
                 JOIN tool_output_reductions d ON d.call_id = r.call_id AND d.spilled = 1",
        );
        if let Some(thread_id) = thread_id {
            count_query = sqlx::query(
                "SELECT COUNT(DISTINCT r.call_id) AS count FROM tool_output_retrievals r \
                 JOIN tool_output_reductions d ON d.call_id = r.call_id AND d.spilled = 1 \
                 WHERE d.thread_id = ?",
            )
            .bind(thread_id);
        }
        let retrievals = count_query
            .fetch_one(self.pool.as_ref())
            .await?
            .try_get("count")?;
        let mut spilled_query =
            sqlx::query("SELECT COUNT(*) AS count FROM tool_output_reductions WHERE spilled = 1");
        if let Some(thread_id) = thread_id {
            spilled_query = sqlx::query(
                "SELECT COUNT(*) AS count FROM tool_output_reductions \
                 WHERE spilled = 1 AND thread_id = ?",
            )
            .bind(thread_id);
        }
        let spilled = spilled_query
            .fetch_one(self.pool.as_ref())
            .await?
            .try_get("count")?;
        Ok(ToolOutputReductionInsights {
            by_kind,
            by_reducer,
            by_tool,
            top_reductions,
            retrievals,
            spilled,
            by_model,
            cost_saved_usd,
        })
    }

    async fn reduction_breakdown(
        &self,
        dimension: ReductionBreakdownDimension,
        thread_id: Option<&str>,
    ) -> anyhow::Result<Vec<ToolOutputReductionBreakdown>> {
        let expression = dimension.expression();
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH turn_groups AS ( \
                SELECT thread_id, turn_id, \
                       ROW_NUMBER() OVER ( \
                           PARTITION BY thread_id \
                           ORDER BY first_recorded_at, first_rowid, turn_id \
                       ) AS turn_ordinal \
                FROM ( \
                    SELECT thread_id, turn_id, MIN(recorded_at) AS first_recorded_at, \
                           MIN(rowid) AS first_rowid \
                    FROM tool_output_reductions \
                    WHERE thread_id IS NOT NULL AND turn_id IS NOT NULL \
                    GROUP BY thread_id, turn_id \
                ) \
            ), rerun_calls AS ( \
                SELECT DISTINCT d.call_id \
                FROM tool_output_reductions d \
                JOIN turn_groups d_turn ON d_turn.thread_id = d.thread_id \
                    AND d_turn.turn_id = d.turn_id \
                JOIN tool_output_reductions r2 ON r2.thread_id = d.thread_id \
                    AND r2.command_hash = d.command_hash \
                JOIN turn_groups r2_turn ON r2_turn.thread_id = r2.thread_id \
                    AND r2_turn.turn_id = r2.turn_id \
                WHERE d.command_hash IS NOT NULL \
                    AND d.thread_id IS NOT NULL AND d.turn_id IS NOT NULL \
                    AND r2.turn_id IS NOT NULL \
                    AND r2_turn.turn_ordinal > d_turn.turn_ordinal \
                    AND r2_turn.turn_ordinal <= d_turn.turn_ordinal + 2 \
            ) SELECT ",
        );
        query
            .push(expression)
            .push(
                " AS dimension, COUNT(*) AS reductions, \
                 SUM(bytes_in) AS bytes_in, SUM(bytes_out) AS bytes_out, \
                 COUNT(DISTINCT CASE WHEN d.spilled = 1 AND EXISTS (SELECT 1 FROM tool_output_retrievals r \
                  WHERE r.call_id = d.call_id) THEN d.call_id END) AS retrievals, \
                 COUNT(DISTINCT CASE WHEN EXISTS ( \
                  SELECT 1 FROM rerun_calls WHERE call_id = d.call_id \
                 ) THEN d.call_id END) AS reruns \
                 FROM tool_output_reductions d",
            );
        if let Some(thread_id) = thread_id {
            query.push(" WHERE thread_id = ").push_bind(thread_id);
        }
        query
            .push(" GROUP BY ")
            .push(expression)
            .push(" ORDER BY reductions DESC LIMIT 20");
        let rows = query.build().fetch_all(self.pool.as_ref()).await?;
        rows.into_iter()
            .map(|row| {
                Ok(ToolOutputReductionBreakdown {
                    dimension: row.try_get("dimension")?,
                    reductions: row.try_get("reductions")?,
                    bytes_in: row.try_get("bytes_in")?,
                    bytes_out: row.try_get("bytes_out")?,
                    retrievals: row.try_get("retrievals")?,
                    reruns: row.try_get("reruns")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }

    async fn reduction_breakdown_by_reducer(
        &self,
        thread_id: Option<&str>,
    ) -> anyhow::Result<Vec<ToolOutputReductionBreakdown>> {
        let rerun_calls = self.rerun_call_ids(thread_id).await?;
        let query = sqlx::query(
            "SELECT call_id, reducers_applied, bytes_in, bytes_out \
             FROM tool_output_reductions \
             WHERE (? IS NULL OR thread_id = ?) ORDER BY recorded_at DESC LIMIT 10000",
        )
        .bind(thread_id)
        .bind(thread_id);
        let rows = query.fetch_all(self.pool.as_ref()).await?;
        let retrieval_rows = sqlx::query(
            "SELECT DISTINCT r.call_id FROM tool_output_retrievals r \
             JOIN tool_output_reductions d ON d.call_id = r.call_id \
             WHERE (? IS NULL OR d.thread_id = ?)",
        )
        .bind(thread_id)
        .bind(thread_id)
        .fetch_all(self.pool.as_ref())
        .await?;
        let retrieved_calls = retrieval_rows
            .into_iter()
            .map(|row| row.try_get::<String, _>("call_id"))
            .collect::<Result<HashSet<_>, _>>()?;
        let mut totals: BTreeMap<String, (i64, i64, i64, i64, HashSet<String>)> = BTreeMap::new();
        for row in rows {
            let call_id: String = row.try_get("call_id")?;
            let reducers: String = row.try_get("reducers_applied")?;
            let bytes_in: i64 = row.try_get("bytes_in")?;
            let bytes_out: i64 = row.try_get("bytes_out")?;
            let names = if reducers.is_empty() {
                vec!["none"]
            } else {
                reducers.split(',').collect()
            };
            let reducer_count = names.len() as i64;
            let bytes_in_share = bytes_in / reducer_count;
            let bytes_in_remainder = bytes_in % reducer_count;
            let bytes_out_share = bytes_out / reducer_count;
            let bytes_out_remainder = bytes_out % reducer_count;
            for (index, name) in names.into_iter().enumerate() {
                let entry = totals
                    .entry(name.to_owned())
                    .or_insert_with(|| (0, 0, 0, 0, HashSet::new()));
                entry.0 += 1;
                entry.1 += bytes_in_share
                    + if (index as i64) < bytes_in_remainder {
                        1
                    } else {
                        0
                    };
                entry.2 += bytes_out_share
                    + if (index as i64) < bytes_out_remainder {
                        1
                    } else {
                        0
                    };
                if rerun_calls.contains(&call_id) {
                    entry.3 += 1;
                }
                entry.4.insert(call_id.clone());
            }
        }
        totals
            .into_iter()
            .map(
                |(dimension, (reductions, bytes_in, bytes_out, reruns, calls))| {
                    Ok(ToolOutputReductionBreakdown {
                        dimension,
                        reductions,
                        bytes_in,
                        bytes_out,
                        retrievals: calls.intersection(&retrieved_calls).count() as i64,
                        reruns,
                    })
                },
            )
            .collect::<anyhow::Result<Vec<_>>>()
    }

    async fn rerun_call_ids(&self, thread_id: Option<&str>) -> anyhow::Result<HashSet<String>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH turn_groups AS ( \
                SELECT thread_id, turn_id, \
                       ROW_NUMBER() OVER ( \
                           PARTITION BY thread_id \
                           ORDER BY first_recorded_at, first_rowid, turn_id \
                       ) AS turn_ordinal \
                FROM ( \
                    SELECT thread_id, turn_id, MIN(recorded_at) AS first_recorded_at, \
                           MIN(rowid) AS first_rowid \
                    FROM tool_output_reductions \
                    WHERE thread_id IS NOT NULL AND turn_id IS NOT NULL \
                    GROUP BY thread_id, turn_id \
                ) \
            ) SELECT DISTINCT d.call_id \
              FROM tool_output_reductions d \
              JOIN turn_groups d_turn ON d_turn.thread_id = d.thread_id \
                  AND d_turn.turn_id = d.turn_id \
              JOIN tool_output_reductions r2 ON r2.thread_id = d.thread_id \
                  AND r2.command_hash = d.command_hash \
              JOIN turn_groups r2_turn ON r2_turn.thread_id = r2.thread_id \
                  AND r2_turn.turn_id = r2.turn_id \
              WHERE d.command_hash IS NOT NULL \
                  AND d.thread_id IS NOT NULL AND d.turn_id IS NOT NULL \
                  AND r2.turn_id IS NOT NULL \
                  AND r2_turn.turn_ordinal > d_turn.turn_ordinal \
                  AND r2_turn.turn_ordinal <= d_turn.turn_ordinal + 2",
        );
        if let Some(thread_id) = thread_id {
            query.push(" AND d.thread_id = ").push_bind(thread_id);
        }
        let rows = query.build().fetch_all(self.pool.as_ref()).await?;
        rows.into_iter()
            .map(|row| row.try_get::<String, _>("call_id"))
            .collect::<Result<HashSet<_>, _>>()
            .map_err(Into::into)
    }

    /// Fold complete UTC days into the durable daily rollup.
    pub async fn fold_tool_output_reductions_into_daily(&self) -> anyhow::Result<()> {
        let today = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let mut tx = self.pool.begin().await?;
        let last: i64 = sqlx::query_scalar(
            "SELECT last_folded_day FROM tool_output_reduction_rollup_state WHERE id = 0",
        )
        .fetch_one(&mut *tx)
        .await?;
        let first: Option<i64> = sqlx::query_scalar(
            "SELECT MIN((recorded_at / 86400) * 86400) FROM tool_output_reductions",
        )
        .fetch_one(&mut *tx)
        .await?;
        let mut day = if last == 0 {
            first.unwrap_or(today)
        } else {
            last + 86400
        };
        while day < today {
            let rows = sqlx::query(
                "SELECT model_slug, COUNT(*) AS reductions, \
                 COALESCE(SUM(est_tokens_in - est_tokens_out), 0) AS tokens_saved, \
                 COALESCE(SUM(CASE WHEN input_price_per_1m IS NULL THEN 0.0 \
                    ELSE (est_tokens_in - est_tokens_out) * input_price_per_1m / 1000000.0 END), 0.0) AS cost_saved_usd \
                 FROM tool_output_reductions WHERE recorded_at >= ? AND recorded_at < ? GROUP BY model_slug",
            )
            .bind(day)
            .bind(day + 86400)
            .fetch_all(&mut *tx)
            .await?;
            for row in rows {
                sqlx::query(
                    "INSERT INTO tool_output_reduction_daily \
                     (day, model_slug, reductions, tokens_saved, cost_saved_usd) VALUES (?, ?, ?, ?, ?) \
                     ON CONFLICT(day, model_slug) DO UPDATE SET reductions = excluded.reductions, \
                     tokens_saved = excluded.tokens_saved, cost_saved_usd = excluded.cost_saved_usd",
                )
                .bind(day)
                .bind(row.try_get::<Option<String>, _>("model_slug")?)
                .bind(row.try_get::<i64, _>("reductions")?)
                .bind(row.try_get::<i64, _>("tokens_saved")?)
                .bind(row.try_get::<f64, _>("cost_saved_usd")?)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "UPDATE tool_output_reduction_rollup_state SET last_folded_day = ? WHERE id = 0",
            )
            .bind(day)
            .execute(&mut *tx)
            .await?;
            day += 86400;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Return a bounded daily report, including today's raw partial day.
    pub async fn tool_output_reduction_report(
        &self,
        since_day: i64,
        until_day: i64,
        model_filter: Option<&str>,
    ) -> anyhow::Result<ToolOutputReductionReport> {
        let since_day = since_day.max(0);
        let until_day = until_day.max(since_day).min(since_day + 366 * 86400);
        self.fold_tool_output_reductions_into_daily().await?;
        let today = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let mut days = Vec::new();
        let mut day = since_day;
        while day <= until_day {
            let partial = day >= today;
            let rows = if partial {
                (if let Some(model) = model_filter {
                    sqlx::query(
                        "SELECT model_slug, COUNT(*) AS reductions, COALESCE(SUM(est_tokens_in - est_tokens_out), 0) AS tokens_saved, \
                         COALESCE(SUM(CASE WHEN input_price_per_1m IS NULL THEN 0.0 ELSE (est_tokens_in - est_tokens_out) * input_price_per_1m / 1000000.0 END), 0.0) AS cost_saved_usd \
                         FROM tool_output_reductions WHERE recorded_at >= ? AND recorded_at < ? AND model_slug = ? GROUP BY model_slug",
                    ).bind(day).bind(day + 86400).bind(model)
                } else {
                    sqlx::query(
                        "SELECT model_slug, COUNT(*) AS reductions, COALESCE(SUM(est_tokens_in - est_tokens_out), 0) AS tokens_saved, \
                         COALESCE(SUM(CASE WHEN input_price_per_1m IS NULL THEN 0.0 ELSE (est_tokens_in - est_tokens_out) * input_price_per_1m / 1000000.0 END), 0.0) AS cost_saved_usd \
                         FROM tool_output_reductions WHERE recorded_at >= ? AND recorded_at < ? GROUP BY model_slug",
                    ).bind(day).bind(day + 86400)
                })
                .fetch_all(self.pool.as_ref())
                .await?
            } else {
                let mut query = sqlx::query(
                    "SELECT model_slug, reductions, tokens_saved, cost_saved_usd FROM tool_output_reduction_daily WHERE day = ?",
                ).bind(day);
                if let Some(model) = model_filter {
                    query = sqlx::query(
                        "SELECT model_slug, reductions, tokens_saved, cost_saved_usd FROM tool_output_reduction_daily WHERE day = ? AND model_slug = ?",
                    ).bind(day).bind(model);
                }
                query.fetch_all(self.pool.as_ref()).await?
            };
            let mut by_model = Vec::new();
            for row in rows {
                by_model.push(ToolOutputReductionReportModel {
                    model_slug: row.try_get("model_slug")?,
                    reductions: row.try_get("reductions")?,
                    tokens_saved: row.try_get("tokens_saved")?,
                    cost_saved_usd: row.try_get("cost_saved_usd")?,
                });
            }
            by_model.sort_by_key(|item| item.model_slug.clone());
            let reductions = by_model.iter().map(|item| item.reductions).sum();
            let tokens_saved = by_model.iter().map(|item| item.tokens_saved).sum();
            let cost_saved_usd = by_model.iter().map(|item| item.cost_saved_usd).sum();
            days.push(ToolOutputReductionReportDay {
                day,
                partial,
                by_model,
                reductions,
                tokens_saved,
                cost_saved_usd,
            });
            day += 86400;
        }
        Ok(ToolOutputReductionReport {
            since_day,
            until_day,
            reductions: days.iter().map(|d| d.reductions).sum(),
            tokens_saved: days.iter().map(|d| d.tokens_saved).sum(),
            cost_saved_usd: days.iter().map(|d| d.cost_saved_usd).sum(),
            days,
        })
    }

    /// Remove durable report data explicitly; reset-stats intentionally leaves it intact.
    pub async fn reset_tool_output_reduction_report(&self) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM tool_output_reduction_daily")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE tool_output_reduction_rollup_state SET last_folded_day = 0 WHERE id = 0",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Clear persisted tool-output reduction records without affecting other telemetry.
    pub async fn reset_tool_output_reduction_stats(&self) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM tool_output_retrievals")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM tool_output_reductions")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Record that a spilled tool output was read back by a shell command.
    pub async fn record_tool_output_retrieval(
        &self,
        call_id: &str,
        spill_path: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO tool_output_retrievals (call_id, spill_path, retrieved_at) VALUES (?, ?, ?)",
        )
        .bind(call_id)
        .bind(spill_path)
        .bind(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64),
        )
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Initialize the state runtime using the provided Xedoc home and default provider.
    ///
    /// This opens (and migrates) the SQLite databases under `xedoc_home`.
    /// Logs and paginated thread history live in dedicated files to reduce
    /// lock contention with the rest of the state store.
    pub async fn init(xedoc_home: PathBuf, default_provider: String) -> anyhow::Result<Arc<Self>> {
        Self::init_inner(
            xedoc_home,
            default_provider,
            /*telemetry_override*/ None,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn init_with_telemetry_for_tests(
        xedoc_home: PathBuf,
        default_provider: String,
        telemetry_override: &dyn DbTelemetry,
    ) -> anyhow::Result<Arc<Self>> {
        Self::init_inner(xedoc_home, default_provider, Some(telemetry_override)).await
    }

    async fn init_inner(
        xedoc_home: PathBuf,
        default_provider: String,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<Arc<Self>> {
        let sqlite = SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(xedoc_home.clone())?);
        tokio::fs::create_dir_all(&xedoc_home).await?;
        let state_migrator = runtime_state_migrator();
        let logs_migrator = runtime_logs_migrator();
        let goals_migrator = runtime_goals_migrator();
        let state_path = STATE_DB.path(xedoc_home.as_path());
        let logs_path = LOGS_DB.path(xedoc_home.as_path());
        let goals_path = GOALS_DB.path(xedoc_home.as_path());
        let pool = match open_state_sqlite(
            &sqlite,
            &state_path,
            &state_migrator,
            telemetry_override,
        )
        .await
        {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open state db at {}: {err}", state_path.display());
                return Err(err);
            }
        };
        let logs_pool =
            match open_logs_sqlite(&sqlite, &logs_path, &logs_migrator, telemetry_override).await {
                Ok(db) => Arc::new(db),
                Err(err) => {
                    warn!("failed to open logs db at {}: {err}", logs_path.display());
                    close_sqlite_pools(&[pool.as_ref()]).await;
                    return Err(err);
                }
            };
        let goals_pool = match open_goals_sqlite(
            &sqlite,
            &goals_path,
            &goals_migrator,
            telemetry_override,
        )
        .await
        {
            Ok(db) => Arc::new(db),
            Err(err) => {
                warn!("failed to open goals db at {}: {err}", goals_path.display());
                close_sqlite_pools(&[pool.as_ref(), logs_pool.as_ref()]).await;
                return Err(err);
            }
        };
        let started = Instant::now();
        let backfill_state_result = ensure_backfill_state_row_in_pool(pool.as_ref()).await;
        crate::telemetry::record_init_result(
            telemetry_override,
            DbKind::State,
            "ensure_backfill_state",
            started.elapsed(),
            &backfill_state_result,
        );
        if let Err(err) = backfill_state_result {
            close_sqlite_pools(&[pool.as_ref(), logs_pool.as_ref(), goals_pool.as_ref()]).await;
            return Err(err);
        }
        let started = Instant::now();
        let thread_timestamp_millis_result: anyhow::Result<(Option<i64>, Option<i64>)> =
            sqlx::query_as(
                "SELECT MAX(threads.updated_at_ms), MAX(threads.recency_at_ms) FROM threads",
            )
            .fetch_one(pool.as_ref())
            .await
            .map_err(anyhow::Error::from);
        crate::telemetry::record_init_result(
            telemetry_override,
            DbKind::State,
            "post_init_query",
            started.elapsed(),
            &thread_timestamp_millis_result,
        );
        let (thread_updated_at_millis, thread_recency_at_millis) =
            match thread_timestamp_millis_result {
                Ok(value) => value,
                Err(err) => {
                    close_sqlite_pools(&[pool.as_ref(), logs_pool.as_ref(), goals_pool.as_ref()])
                        .await;
                    return Err(err);
                }
            };
        let thread_updated_at_millis = thread_updated_at_millis.unwrap_or(0);
        let thread_recency_at_millis = thread_recency_at_millis.unwrap_or(0);
        let (reduction_tx, mut reduction_rx) = mpsc::channel(REDUCTION_QUEUE_CAPACITY);
        let (retrieval_tx, mut retrieval_rx) =
            mpsc::channel::<(String, String)>(REDUCTION_QUEUE_CAPACITY);
        let reduction_pool = Arc::clone(&pool);
        let retrieval_pool = Arc::clone(&pool);
        tokio::spawn(async move {
            while let Some(record) = reduction_rx.recv().await {
                if let Err(err) =
                    persist_tool_output_reduction(reduction_pool.as_ref(), &record).await
                {
                    warn!("failed to persist tool-output reduction: {err}");
                }
            }
        });
        tokio::spawn(async move {
            while let Some((call_id, spill_path)) = retrieval_rx.recv().await {
                if let Err(err) =
                    persist_tool_output_retrieval(retrieval_pool.as_ref(), &call_id, &spill_path)
                        .await
                {
                    warn!("failed to persist tool-output retrieval: {err}");
                }
            }
        });
        let runtime = Arc::new(Self {
            thread_goals: GoalStore::new(Arc::clone(&goals_pool)),
            pool,
            logs_pool,
            xedoc_home,
            default_provider,
            thread_updated_at_millis: Arc::new(AtomicI64::new(thread_updated_at_millis)),
            thread_recency_at_millis: Arc::new(AtomicI64::new(thread_recency_at_millis)),
            reduction_tx,
            retrieval_tx,
        });
        if let Err(err) = runtime.run_logs_startup_maintenance().await {
            warn!(
                "failed to run startup maintenance for logs db at {}: {err}",
                logs_path.display(),
            );
        }
        Ok(runtime)
    }

    /// Return the configured Xedoc home directory for this runtime.
    pub fn xedoc_home(&self) -> &Path {
        self.xedoc_home.as_path()
    }

    pub fn thread_goals(&self) -> &GoalStore {
        &self.thread_goals
    }

    /// Close all SQLite pools and wait for outstanding pool workers to exit.
    pub async fn close(&self) {
        self.thread_goals.close().await;
        self.logs_pool.close().await;
        self.pool.close().await;
    }
}

async fn persist_tool_output_reduction(
    pool: &SqlitePool,
    record: &ReductionRecord,
) -> anyhow::Result<()> {
    let kind = payload_kind_name(record.kind);
    let level = reduction_level_name(record.level);
    let reducers_applied = record
        .reducers_applied
        .iter()
        .map(reducer_name)
        .collect::<Vec<_>>()
        .join(",");
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO tool_output_reductions \
         (thread_id, turn_id, call_id, command_hash, tool_name, kind, level, reducers_applied, \
         bytes_in, bytes_out, est_tokens_in, est_tokens_out, duration_us, spilled, spill_path, recorded_at, \
         model_slug, input_price_per_1m) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.thread_id.as_deref())
    .bind(record.turn_id.as_deref())
    .bind(&record.call_id)
    .bind(record.command_hash.as_deref())
    .bind(&record.tool_name)
    .bind(kind)
    .bind(level)
    .bind(reducers_applied)
    .bind(record.bytes_in as i64)
    .bind(record.bytes_out as i64)
    .bind(record.est_tokens_in)
    .bind(record.est_tokens_out)
    .bind(record.duration_us as i64)
    .bind(record.spilled)
    .bind(record.spill_path.as_deref())
    .bind(record.recorded_at)
    .bind(record.model_slug.as_deref())
    .bind(record.input_price_per_1m)
    .execute(&mut *tx)
    .await?;

    let recorded_day = record.recorded_at.div_euclid(86400) * 86400;
    let last_folded_day: i64 = sqlx::query_scalar(
        "SELECT last_folded_day FROM tool_output_reduction_rollup_state WHERE id = 0",
    )
    .fetch_one(&mut *tx)
    .await?;
    if recorded_day <= last_folded_day {
        sqlx::query("DELETE FROM tool_output_reduction_daily WHERE day = ?")
            .bind(recorded_day)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO tool_output_reduction_daily \
             (day, model_slug, reductions, tokens_saved, cost_saved_usd) \
             SELECT ?, model_slug, COUNT(*), \
                    COALESCE(SUM(est_tokens_in - est_tokens_out), 0), \
                    COALESCE(SUM(CASE WHEN input_price_per_1m IS NULL THEN 0.0 \
                        ELSE (est_tokens_in - est_tokens_out) * input_price_per_1m / 1000000.0 END), 0.0) \
             FROM tool_output_reductions \
             WHERE recorded_at >= ? AND recorded_at < ? \
             GROUP BY model_slug",
        )
        .bind(recorded_day)
        .bind(recorded_day)
        .bind(recorded_day + 86400)
        .execute(&mut *tx)
        .await?;
    }

    // Keep dashboard history bounded even when users never invoke reset-stats.
    // The age cutoff handles dormant databases; the row cap handles high-volume
    // sessions while retaining the newest records.
    let cutoff = record
        .recorded_at
        .saturating_sub(TOOL_OUTPUT_REDUCTION_RETENTION_DAYS * 24 * 60 * 60);
    sqlx::query(
        "DELETE FROM tool_output_reductions
         WHERE recorded_at < ?
            OR id NOT IN (
                SELECT id FROM tool_output_reductions
                ORDER BY recorded_at DESC, id DESC
                LIMIT ?
            )",
    )
    .bind(cutoff)
    .bind(TOOL_OUTPUT_REDUCTION_ROW_LIMIT)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM tool_output_retrievals
         WHERE NOT EXISTS (
             SELECT 1 FROM tool_output_reductions d WHERE d.call_id = tool_output_retrievals.call_id
         )",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn persist_tool_output_retrieval(
    pool: &SqlitePool,
    call_id: &str,
    spill_path: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO tool_output_retrievals (call_id, spill_path, retrieved_at) VALUES (?, ?, ?)",
    )
    .bind(call_id)
    .bind(spill_path)
    .bind(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64),
    )
    .execute(pool)
    .await?;
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
        .saturating_sub(TOOL_OUTPUT_REDUCTION_RETENTION_DAYS * 24 * 60 * 60);
    sqlx::query(
        "DELETE FROM tool_output_retrievals
         WHERE retrieved_at < ?
            OR id NOT IN (
                SELECT id FROM tool_output_retrievals
                ORDER BY retrieved_at DESC, id DESC
                LIMIT ?
            )",
    )
    .bind(cutoff)
    .bind(TOOL_OUTPUT_REDUCTION_ROW_LIMIT)
    .execute(pool)
    .await?;
    Ok(())
}

fn payload_kind_name(kind: PayloadKind) -> &'static str {
    match kind {
        PayloadKind::Json => "json",
        PayloadKind::Log => "log",
        PayloadKind::Diff => "diff",
        PayloadKind::Table => "table",
        PayloadKind::Code => "code",
        PayloadKind::Prose => "prose",
        PayloadKind::Binaryish => "binaryish",
    }
}

fn reduction_level_name(level: ReductionLevel) -> &'static str {
    match level {
        ReductionLevel::Off => "off",
        ReductionLevel::Conservative => "conservative",
        ReductionLevel::Balanced => "balanced",
        ReductionLevel::Aggressive => "aggressive",
    }
}

fn reducer_name(reducer: &ReducerId) -> &'static str {
    match reducer {
        ReducerId::Normalize => "normalize",
        ReducerId::Dedup => "dedup",
        ReducerId::Json => "json",
        ReducerId::Log => "log",
        ReducerId::Diff => "diff-display-only",
        ReducerId::Budget => "budget",
    }
}

async fn close_sqlite_pools(pools: &[&SqlitePool]) {
    for pool in pools {
        pool.close().await;
    }
}

async fn open_state_sqlite(
    sqlite: &SqliteConfig,
    path: &Path,
    migrator: &Migrator,
    telemetry_override: Option<&dyn DbTelemetry>,
) -> anyhow::Result<SqlitePool> {
    // New state DBs should use incremental auto-vacuum, but retrofitting an
    // existing DB requires a full VACUUM. Do not attempt that during process
    // startup: it is maintenance work that can contend with foreground writers.
    open_sqlite(sqlite, path, migrator, STATE_DB, telemetry_override).await
}

async fn open_logs_sqlite(
    sqlite: &SqliteConfig,
    path: &Path,
    migrator: &Migrator,
    telemetry_override: Option<&dyn DbTelemetry>,
) -> anyhow::Result<SqlitePool> {
    open_sqlite(sqlite, path, migrator, LOGS_DB, telemetry_override).await
}

async fn open_goals_sqlite(
    sqlite: &SqliteConfig,
    path: &Path,
    migrator: &Migrator,
    telemetry_override: Option<&dyn DbTelemetry>,
) -> anyhow::Result<SqlitePool> {
    open_sqlite(sqlite, path, migrator, GOALS_DB, telemetry_override).await
}

/// Open and migrate the rebuildable paginated thread-history database.
pub async fn open_thread_history_db(sqlite_home: &Path) -> anyhow::Result<SqlitePool> {
    let sqlite = SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(sqlite_home)?);
    let migrator = runtime_thread_history_migrator();
    open_sqlite(
        &sqlite,
        thread_history_db_path(sqlite_home).as_path(),
        &migrator,
        THREAD_HISTORY_DB,
        /*telemetry_override*/ None,
    )
    .await
}

async fn open_sqlite(
    sqlite: &SqliteConfig,
    path: &Path,
    migrator: &Migrator,
    spec: RuntimeDbSpec,
    telemetry_override: Option<&dyn DbTelemetry>,
) -> anyhow::Result<SqlitePool> {
    let started = Instant::now();
    let pool_result = sqlite
        .open_read_write_pool(path)
        .await
        .map_err(anyhow::Error::from);
    crate::telemetry::record_init_result(
        telemetry_override,
        spec.kind,
        spec.open_phase,
        started.elapsed(),
        &pool_result,
    );
    let pool = pool_result
        .map_err(|source| recovery::RuntimeDbInitError::new(spec.label, "open", path, source))?;
    let started = Instant::now();
    let migrate_result = async {
        if matches!(spec.kind, DbKind::State) {
            repair_legacy_state_migration_versions(&pool, migrator).await?;
        }
        migrator.run(&pool).await.map_err(anyhow::Error::from)
    }
    .await;
    crate::telemetry::record_init_result(
        telemetry_override,
        spec.kind,
        spec.migrate_phase,
        started.elapsed(),
        &migrate_result,
    );
    if let Err(source) = migrate_result {
        pool.close().await;
        return Err(recovery::RuntimeDbInitError::new(spec.label, "migrate", path, source).into());
    }
    Ok(pool)
}

pub(super) async fn ensure_backfill_state_row_in_pool(
    pool: &sqlx::SqlitePool,
) -> anyhow::Result<()> {
    // Eagerly check if the operation would have no effect to avoid blocking waiting for a SQLite
    // writer for no reason in the hot startup path.
    if sqlx::query_scalar::<_, i64>("SELECT 1 FROM backfill_state WHERE id = 1")
        .fetch_optional(pool)
        .await?
        .is_some()
    {
        return Ok(());
    }

    sqlx::query(
        r#"
INSERT INTO backfill_state (id, status, last_watermark, last_success_at, updated_at)
VALUES (?, ?, NULL, NULL, ?)
ON CONFLICT(id) DO NOTHING
            "#,
    )
    .bind(1_i64)
    .bind(crate::BackfillStatus::Pending.as_str())
    .bind(Utc::now().timestamp())
    .execute(pool)
    .await?;
    Ok(())
}

pub fn state_db_filename() -> String {
    STATE_DB.filename.to_string()
}

pub fn state_db_path(xedoc_home: &Path) -> PathBuf {
    STATE_DB.path(xedoc_home)
}

pub fn logs_db_filename() -> String {
    LOGS_DB.filename.to_string()
}

pub fn logs_db_path(xedoc_home: &Path) -> PathBuf {
    LOGS_DB.path(xedoc_home)
}

pub fn goals_db_filename() -> String {
    GOALS_DB.filename.to_string()
}

pub fn goals_db_path(xedoc_home: &Path) -> PathBuf {
    GOALS_DB.path(xedoc_home)
}

pub fn thread_history_db_filename() -> String {
    THREAD_HISTORY_DB.filename.to_string()
}

pub fn thread_history_db_path(xedoc_home: &Path) -> PathBuf {
    THREAD_HISTORY_DB.path(xedoc_home)
}

pub fn runtime_db_paths(xedoc_home: &Path) -> Vec<RuntimeDbPath> {
    RUNTIME_DBS
        .iter()
        .map(|spec| RuntimeDbPath {
            label: spec.label,
            path: spec.path(xedoc_home),
        })
        .collect()
}

/// Run SQLite's built-in integrity check against an existing database file.
pub async fn sqlite_integrity_check(path: &Path) -> anyhow::Result<Vec<String>> {
    let sqlite =
        SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(path.parent().unwrap_or(path))?);
    let pool = sqlite.open_read_only_pool(path).await?;
    let rows = sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
        .fetch_all(&pool)
        .await?;
    pool.close().await;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::StateRuntime;
    use super::open_state_sqlite;
    use super::runtime_state_migrator;
    use super::sqlite_integrity_check;
    use super::state_db_path;
    use super::test_support::unique_temp_dir;
    use crate::DB_INIT_METRIC;
    use crate::DbTelemetry;
    use crate::migrations::STATE_MIGRATOR;
    use pretty_assertions::assert_eq;
    use sqlx::SqlitePool;
    use sqlx::migrate::MigrateError;
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::Mutex;
    use xedoc_utils_absolute_path::test_support::PathExt;

    #[derive(Default)]
    struct TestTelemetry {
        counters: Mutex<Vec<MetricEvent>>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct MetricEvent {
        name: String,
        tags: BTreeMap<String, String>,
    }

    impl TestTelemetry {
        fn counters(&self) -> Vec<MetricEvent> {
            self.counters
                .lock()
                .expect("telemetry lock")
                .iter()
                .map(|event| MetricEvent {
                    name: event.name.clone(),
                    tags: event.tags.clone(),
                })
                .collect()
        }
    }

    impl DbTelemetry for TestTelemetry {
        fn counter(&self, name: &str, _inc: i64, tags: &[(&str, &str)]) {
            self.counters
                .lock()
                .expect("telemetry lock")
                .push(MetricEvent {
                    name: name.to_string(),
                    tags: tags_to_map(tags),
                });
        }

        fn record_duration(
            &self,
            _name: &str,
            _duration: std::time::Duration,
            _tags: &[(&str, &str)],
        ) {
        }
    }

    fn tags_to_map(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    async fn open_db_pool(path: &Path) -> SqlitePool {
        crate::SqliteConfig::new_for_testing(path.parent().unwrap_or(path).abs())
            .open_read_write_pool(path)
            .await
            .expect("open sqlite pool")
    }

    #[tokio::test]
    async fn sqlite_integrity_check_reports_ok_for_valid_db() {
        let xedoc_home = unique_temp_dir();
        tokio::fs::create_dir_all(&xedoc_home)
            .await
            .expect("create xedoc home");
        let path = state_db_path(xedoc_home.as_path());
        let pool = crate::SqliteConfig::new_for_testing(xedoc_home.as_path().abs())
            .open_read_write_pool(&path)
            .await
            .expect("open sqlite db");
        sqlx::query("CREATE TABLE sample (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("create sample table");
        pool.close().await;

        let result = sqlite_integrity_check(&path)
            .await
            .expect("integrity check should run");

        assert_eq!(result, vec!["ok".to_string()]);
        let _ = tokio::fs::remove_dir_all(xedoc_home).await;
    }

    #[tokio::test]
    async fn open_state_sqlite_tolerates_newer_applied_migrations() {
        let xedoc_home = unique_temp_dir();
        tokio::fs::create_dir_all(&xedoc_home)
            .await
            .expect("create xedoc home");
        let state_path = state_db_path(xedoc_home.as_path());
        let pool = crate::SqliteConfig::new_for_testing(xedoc_home.as_path().abs())
            .open_read_write_pool(&state_path)
            .await
            .expect("open state db");
        STATE_MIGRATOR
            .run(&pool)
            .await
            .expect("apply current state schema");
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(9_999_i64)
        .bind("future migration")
        .bind(true)
        .bind(vec![1_u8, 2, 3, 4])
        .bind(1_i64)
        .execute(&pool)
        .await
        .expect("insert future migration record");
        pool.close().await;

        let strict_pool = open_db_pool(state_path.as_path()).await;
        let strict_err = STATE_MIGRATOR
            .run(&strict_pool)
            .await
            .expect_err("strict migrator should reject newer applied migrations");
        assert!(matches!(strict_err, MigrateError::VersionMissing(9_999)));
        strict_pool.close().await;

        let tolerant_migrator = runtime_state_migrator();
        let tolerant_pool = open_state_sqlite(
            &crate::SqliteConfig::new_for_testing(xedoc_home.as_path().abs()),
            state_path.as_path(),
            &tolerant_migrator,
            /*telemetry_override*/ None,
        )
        .await
        .expect("runtime migrator should tolerate newer applied migrations");
        tolerant_pool.close().await;

        let _ = tokio::fs::remove_dir_all(xedoc_home).await;
    }

    #[tokio::test]
    async fn init_records_successful_sqlite_init_phases_to_explicit_telemetry() {
        let xedoc_home = unique_temp_dir();
        let telemetry = TestTelemetry::default();

        let runtime = StateRuntime::init_with_telemetry_for_tests(
            xedoc_home.clone(),
            "test-provider".to_string(),
            &telemetry,
        )
        .await
        .expect("state runtime should initialize");

        let phases = telemetry
            .counters()
            .into_iter()
            .filter(|event| event.name == DB_INIT_METRIC)
            .filter(|event| event.tags.get("status").map(String::as_str) == Some("success"))
            .filter_map(|event| event.tags.get("phase").cloned())
            .collect::<BTreeSet<_>>();
        let expected = [
            "open_state",
            "migrate_state",
            "open_logs",
            "migrate_logs",
            "open_goals",
            "migrate_goals",
            "ensure_backfill_state",
            "post_init_query",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
        assert_eq!(phases, expected);

        runtime.close().await;
        let _ = tokio::fs::remove_dir_all(xedoc_home).await;
    }
}

#[cfg(test)]
#[path = "runtime_reduction_tests.rs"]
mod reduction_tests;
