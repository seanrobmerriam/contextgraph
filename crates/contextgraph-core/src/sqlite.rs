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

#[async_trait]
impl RefStore for SqliteStore {
    async fn get_branch(&self, name: &str) -> Result<Option<CommitId>> {
        let row = sqlx::query("SELECT commit_id FROM branches WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        match row {
            None => Ok(None),
            Some(row) => Ok(Some(CommitId::from_str(
                row.get::<String, _>("commit_id").as_str(),
            )?)),
        }
    }

    async fn set_branch(&self, name: &str, commit_id: CommitId) -> Result<()> {
        sqlx::query(
            "INSERT INTO branches (name, commit_id) VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET commit_id = excluded.commit_id",
        )
        .bind(name)
        .bind(commit_id.to_hex())
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn delete_branch(&self, name: &str) -> Result<()> {
        let result = sqlx::query("DELETE FROM branches WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(GraphError::BranchNotFound(name.to_string()));
        }
        Ok(())
    }

    async fn list_branches(&self) -> Result<Vec<(String, CommitId)>> {
        let rows = sqlx::query("SELECT name, commit_id FROM branches")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| GraphError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| {
                let id = CommitId::from_str(r.get::<String, _>("commit_id").as_str())?;
                Ok((r.get::<String, _>("name"), id))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{Author, Delta, Metadata};
    use chrono::Utc;

    fn root() -> Commit {
        Commit::new(
            vec![],
            Author::System,
            Delta::Message {
                content: "root".into(),
            },
            Metadata::new(Utc::now()),
        )
    }

    #[tokio::test]
    async fn sqlite_store_persists_and_retrieves_a_commit() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let r = root();
        let id = store.put(r.clone()).await.unwrap();
        let fetched = store.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched, r);
    }

    #[tokio::test]
    async fn sqlite_store_rejects_commit_with_missing_parent() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let orphan = Commit::new(
            vec![CommitId::from_bytes([7; 32])],
            Author::User,
            Delta::Message {
                content: "x".into(),
            },
            Metadata::new(Utc::now()),
        );
        let err = store.put(orphan).await.unwrap_err();
        assert!(matches!(err, GraphError::ParentNotFound(_)));
    }

    #[tokio::test]
    async fn sqlite_store_is_idempotent_on_duplicate_put() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let r = root();
        store.put(r.clone()).await.unwrap();
        store.put(r.clone()).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn sqlite_store_indexes_children_for_traversal() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let r = root();
        store.put(r.clone()).await.unwrap();
        let child = Commit::new(
            vec![r.id],
            Author::User,
            Delta::Message {
                content: "hi".into(),
            },
            Metadata::new(Utc::now()),
        );
        store.put(child.clone()).await.unwrap();
        let kids = store.children(&r.id).await.unwrap();
        assert_eq!(kids, vec![child.id]);
    }

    #[tokio::test]
    async fn sqlite_ref_store_round_trips_branch_pointer() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let r = root();
        store.put(r.clone()).await.unwrap();
        store.set_branch("main", r.id).await.unwrap();
        assert_eq!(store.get_branch("main").await.unwrap(), Some(r.id));
    }

    #[tokio::test]
    async fn sqlite_ref_store_delete_of_missing_branch_fails() {
        let store = SqliteStore::open_in_memory().await.unwrap();
        let err = store.delete_branch("nope").await.unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }

    #[tokio::test]
    async fn opening_a_store_in_a_nonexistent_directory_fails_with_a_clear_message() {
        let err = SqliteStore::open("/no/such/directory/at/all/store.db")
            .await
            .unwrap_err();
        match err {
            GraphError::Storage(msg) => {
                assert!(msg.contains("does not exist"), "message was: {msg}");
                assert!(
                    msg.contains("/no/such/directory/at/all"),
                    "message was: {msg}"
                );
            }
            other => panic!("expected Storage error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opening_an_in_memory_store_bypasses_path_validation() {
        // Must not be rejected as "directory does not exist" just because
        // ":memory:" isn't a real filesystem path.
        SqliteStore::open_in_memory().await.unwrap();
    }

    #[tokio::test]
    async fn opening_a_store_at_an_existing_directory_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db").to_string_lossy().to_string();
        SqliteStore::open(&path).await.unwrap();
    }
}
