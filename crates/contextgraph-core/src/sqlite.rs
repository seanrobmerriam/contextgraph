//! SQLite-backed `CommitStore` + `RefStore`, in WAL mode. Commit blobs and
//! metadata are stored as JSON text columns; parent/child edges are indexed
//! in both directions so ancestry walks (`log`, `bisect`) don't need to
//! deserialize blobs just to traverse the graph.

use crate::commit::{Commit, CommitId};
use crate::error::{GraphError, Result};
use crate::store::{CommitStore, RefStore};
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

/// A single SQLite-backed store implementing both `CommitStore` and
/// `RefStore`, sharing one connection pool so commit + branch-move
/// operations can be composed into a single transaction by callers.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Opens (creating if needed) a SQLite database at `path` and runs
    /// migrations. Enables WAL mode for concurrent readers.
    ///
    /// Validates the path up front so callers get one clear error message
    /// (e.g. "parent directory does not exist") instead of an opaque SQLite
    /// error code, which is otherwise all that surfaces from `open` when
    /// the containing directory is missing.
    pub async fn open(path: &str) -> Result<Self> {
        Self::validate_path(path)?;
        let options = SqliteConnectOptions::from_str(path)
            .map_err(|e| GraphError::Storage(e.to_string()))?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// In-memory SQLite, for tests that want SQLite semantics without a
    /// filesystem temp file.
    pub async fn open_in_memory() -> Result<Self> {
        Self::open(":memory:").await
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Rejects paths whose containing directory doesn't exist, with a
    /// message naming the exact missing directory. In-memory targets
    /// (`:memory:` or a `file:...mode=memory...` URI) always pass.
    fn validate_path(path: &str) -> Result<()> {
        if path == ":memory:" || (path.starts_with("file:") && path.contains("mode=memory")) {
            return Ok(());
        }
        let parent = std::path::Path::new(path).parent();
        if let Some(parent) = parent {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                return Err(GraphError::Storage(format!(
                    "cannot open database at '{path}': directory '{}' does not exist",
                    parent.display()
                )));
            }
        }
        Ok(())
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS commits (
                id TEXT PRIMARY KEY,
                parent_ids TEXT NOT NULL,
                author TEXT NOT NULL,
                delta TEXT NOT NULL,
                metadata TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::Storage(e.to_string()))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS edges (
                parent_id TEXT NOT NULL,
                child_id TEXT NOT NULL,
                PRIMARY KEY (parent_id, child_id)
            );
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::Storage(e.to_string()))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS edges_child_idx ON edges(child_id);")
            .execute(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS branches (
                name TEXT PRIMARY KEY,
                commit_id TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::Storage(e.to_string()))?;

        Ok(())
    }

    fn parent_ids_to_json(ids: &[CommitId]) -> String {
        let hexes: Vec<String> = ids.iter().map(|i| i.to_hex()).collect();
        serde_json::to_string(&hexes).expect("hex string vec always serializes")
    }

    fn parent_ids_from_json(s: &str) -> Result<Vec<CommitId>> {
        let hexes: Vec<String> = serde_json::from_str(s)?;
        hexes.into_iter().map(|h| CommitId::from_str(&h)).collect()
    }

    fn row_to_commit(
        id: String,
        parent_ids: String,
        author: String,
        delta: String,
        metadata: String,
    ) -> Result<Commit> {
        Ok(Commit {
            id: CommitId::from_str(&id)?,
            parent_ids: Self::parent_ids_from_json(&parent_ids)?,
            author: serde_json::from_str(&author)?,
            delta: serde_json::from_str(&delta)?,
            metadata: serde_json::from_str(&metadata)?,
        })
    }
}

#[async_trait]
impl CommitStore for SqliteStore {
    async fn put(&self, commit: Commit) -> Result<CommitId> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;

        let existing = sqlx::query("SELECT id FROM commits WHERE id = ?")
            .bind(commit.id.to_hex())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        if existing.is_some() {
            tx.commit()
                .await
                .map_err(|e| GraphError::Storage(e.to_string()))?;
            return Ok(commit.id);
        }

        for parent in &commit.parent_ids {
            let found = sqlx::query("SELECT id FROM commits WHERE id = ?")
                .bind(parent.to_hex())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| GraphError::Storage(e.to_string()))?;
            if found.is_none() {
                return Err(GraphError::ParentNotFound(*parent));
            }
        }

        let author_json = serde_json::to_string(&commit.author)?;
        let delta_json = serde_json::to_string(&commit.delta)?;
        let metadata_json = serde_json::to_string(&commit.metadata)?;
        let parent_ids_json = Self::parent_ids_to_json(&commit.parent_ids);

        sqlx::query(
            "INSERT INTO commits (id, parent_ids, author, delta, metadata) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(commit.id.to_hex())
        .bind(&parent_ids_json)
        .bind(&author_json)
        .bind(&delta_json)
        .bind(&metadata_json)
        .execute(&mut *tx)
        .await
        .map_err(|e| GraphError::Storage(e.to_string()))?;

        for parent in &commit.parent_ids {
            sqlx::query("INSERT OR IGNORE INTO edges (parent_id, child_id) VALUES (?, ?)")
                .bind(parent.to_hex())
                .bind(commit.id.to_hex())
                .execute(&mut *tx)
                .await
                .map_err(|e| GraphError::Storage(e.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(commit.id)
    }

    async fn get(&self, id: &CommitId) -> Result<Option<Commit>> {
        let row =
            sqlx::query("SELECT id, parent_ids, author, delta, metadata FROM commits WHERE id = ?")
                .bind(id.to_hex())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| GraphError::Storage(e.to_string()))?;

        match row {
            None => Ok(None),
            Some(row) => Ok(Some(Self::row_to_commit(
                row.get("id"),
                row.get("parent_ids"),
                row.get("author"),
                row.get("delta"),
                row.get("metadata"),
            )?)),
        }
    }

    async fn contains(&self, id: &CommitId) -> Result<bool> {
        let row = sqlx::query("SELECT 1 FROM commits WHERE id = ?")
            .bind(id.to_hex())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(row.is_some())
    }

    async fn children(&self, id: &CommitId) -> Result<Vec<CommitId>> {
        let rows = sqlx::query("SELECT child_id FROM edges WHERE parent_id = ?")
            .bind(id.to_hex())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| CommitId::from_str(r.get::<String, _>("child_id").as_str()))
            .collect()
    }

    async fn len(&self) -> Result<usize> {
        let row = sqlx::query("SELECT COUNT(*) as c FROM commits")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        let c: i64 = row.get("c");
        Ok(c as usize)
    }

    async fn all_ids(&self) -> Result<Vec<CommitId>> {
        let rows = sqlx::query("SELECT id FROM commits")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| CommitId::from_str(r.get::<String, _>("id").as_str()))
            .collect()
    }

    async fn remove_many(&self, ids: &[CommitId]) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        for id in ids {
            sqlx::query("DELETE FROM edges WHERE parent_id = ? OR child_id = ?")
                .bind(id.to_hex())
                .bind(id.to_hex())
                .execute(&mut *tx)
                .await
                .map_err(|e| GraphError::Storage(e.to_string()))?;
            sqlx::query("DELETE FROM commits WHERE id = ?")
                .bind(id.to_hex())
                .execute(&mut *tx)
                .await
                .map_err(|e| GraphError::Storage(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(())
    }
}
