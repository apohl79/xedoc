//! Durable input identities and receipts, separate from rebuildable state databases.

use crate::SqliteConfig;
use sqlx::FromRow;
use sqlx::QueryBuilder;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::OnceLock;
use uuid::Uuid;
use xedoc_utils_absolute_path::AbsolutePathBuf;

static PROCESS_OWNER: OnceLock<String> = OnceLock::new();

/// A durable submission identity and its latest delivery evidence.
#[derive(Clone, Debug, FromRow)]
pub struct DeliveryRecord {
    pub scope: String,
    pub key: String,
    pub payload: String,
    pub owner: String,
    pub response: Option<String>,
    pub status: String,
    pub item_id: Option<String>,
    pub turn_id: Option<String>,
}

/// Stores prompt-delivery evidence without automatic database replacement or record expiry.
pub struct PromptDeliveryStore {
    pool: SqlitePool,
    owner: &'static str,
}

impl PromptDeliveryStore {
    /// Opens the ledger, propagating corruption and other persistence failures.
    pub async fn open(xedoc_home: &Path) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(xedoc_home).await?;
        let sqlite =
            SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(xedoc_home.to_path_buf())?);
        let pool = sqlite
            .open_read_write_pool(&xedoc_home.join("prompt_delivery_1.sqlite"))
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS prompt_deliveries (
                scope TEXT NOT NULL,
                key TEXT NOT NULL,
                payload TEXT NOT NULL,
                owner TEXT NOT NULL,
                response TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                item_id TEXT,
                turn_id TEXT,
                PRIMARY KEY (scope, key)
            )",
        )
        .execute(&pool)
        .await?;
        Ok(Self {
            pool,
            owner: PROCESS_OWNER.get_or_init(|| Uuid::now_v7().to_string()),
        })
    }

    /// Claims a key once; callers must compare the returned payload for conflicts.
    pub async fn claim(
        &self,
        scope: &str,
        key: &str,
        payload: &str,
    ) -> anyhow::Result<(bool, DeliveryRecord)> {
        let inserted = sqlx::query(
            "INSERT INTO prompt_deliveries (scope, key, payload, owner)
             VALUES (?, ?, ?, ?) ON CONFLICT (scope, key) DO NOTHING",
        )
        .bind(scope)
        .bind(key)
        .bind(payload)
        .bind(self.owner)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1;
        let record = self
            .read(scope, key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("claimed delivery record is unavailable"))?;
        Ok((inserted, record))
    }

    /// Reads evidence, treating another process's pending input as ambiguous.
    pub async fn read(&self, scope: &str, key: &str) -> anyhow::Result<Option<DeliveryRecord>> {
        Ok(sqlx::query_as(
            "SELECT scope, key, payload, owner, response,
                    CASE WHEN status = 'pending' AND owner != ? THEN 'unknown'
                         ELSE status END AS status,
                    item_id, turn_id
             FROM prompt_deliveries WHERE scope = ? AND key = ?",
        )
        .bind(self.owner)
        .bind(scope)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Saves the first RPC outcome without ever downgrading received evidence.
    pub async fn finish(
        &self,
        scope: &str,
        key: &str,
        response: &str,
        turn_id: Option<&str>,
        status: Option<&str>,
    ) -> anyhow::Result<()> {
        let updated = sqlx::query(
            "UPDATE prompt_deliveries
             SET response = COALESCE(response, ?), turn_id = COALESCE(turn_id, ?),
                 status = CASE WHEN status = 'received' THEN status ELSE COALESCE(?, status) END
             WHERE scope = ? AND key = ?",
        )
        .bind(response)
        .bind(turn_id)
        .bind(status)
        .bind(scope)
        .bind(key)
        .execute(&self.pool)
        .await?
        .rows_affected();
        anyhow::ensure!(updated == 1, "delivery record is unavailable");
        Ok(())
    }

    /// Associates one submission with its exact history item and turn.
    pub async fn associate(
        &self,
        scope: &str,
        key: &str,
        item_id: &str,
        turn_id: &str,
    ) -> anyhow::Result<()> {
        let updated = sqlx::query(
            "UPDATE prompt_deliveries SET item_id = ?, turn_id = ?
             WHERE scope = ? AND key = ?
               AND (item_id IS NULL OR (item_id = ? AND turn_id = ?))",
        )
        .bind(item_id)
        .bind(turn_id)
        .bind(scope)
        .bind(key)
        .bind(item_id)
        .bind(turn_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        anyhow::ensure!(
            updated == 1,
            "delivery history association is unavailable or conflicts"
        );
        Ok(())
    }

    /// Marks input rejected only after the caller has atomically removed it from the queue.
    pub async fn reject_canceled(&self, scope: &str, key: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE prompt_deliveries SET status = 'rejected'
             WHERE scope = ? AND key = ? AND status = 'pending' AND item_id IS NULL",
        )
        .bind(scope)
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persists ambiguity before dispatch and returns the included submission keys.
    pub async fn mark_dispatched(
        &self,
        scope: &str,
        item_ids: &[String],
    ) -> anyhow::Result<Vec<String>> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "UPDATE prompt_deliveries SET status = 'unknown' WHERE scope = ",
        );
        query
            .push_bind(scope)
            .push(" AND status IN ('pending', 'unknown') AND item_id IN (");
        let mut items = query.separated(", ");
        for item_id in item_ids {
            items.push_bind(item_id);
        }
        items.push_unseparated(") RETURNING key");
        Ok(query
            .build_query_scalar::<String>()
            .fetch_all(&self.pool)
            .await?)
    }

    /// Records provider receipt once, excluding definitely rejected submissions.
    pub async fn receive(
        &self,
        scope: &str,
        keys: &[String],
        turn_id: &str,
    ) -> anyhow::Result<Vec<String>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "UPDATE prompt_deliveries SET status = 'received', turn_id = ",
        );
        query
            .push_bind(turn_id)
            .push(" WHERE scope = ")
            .push_bind(scope)
            .push(" AND status NOT IN ('received', 'rejected') AND key IN (");
        let mut submissions = query.separated(", ");
        for key in keys {
            submissions.push_bind(key);
        }
        submissions.push_unseparated(") RETURNING key");
        Ok(query
            .build_query_scalar::<String>()
            .fetch_all(&self.pool)
            .await?)
    }
}
