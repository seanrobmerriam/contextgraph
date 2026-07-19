//! Storage traits. `CommitStore` owns the content-addressed blob graph;
//! `RefStore` owns branch pointers. Both are behind traits so unit tests can
//! run against fast in-memory fakes while production code uses SQLite.

use crate::commit::{Commit, CommitId};
use crate::error::Result;
use async_trait::async_trait;

/// Content-addressed storage for commits, plus the parent/child index needed
/// for ancestry walks (`log`, `bisect`) without touching blob contents.
#[async_trait]
pub trait CommitStore: Send + Sync {
    /// Stores a commit. Content-addressed and idempotent: storing
    /// byte-identical content twice returns the same id without creating a
    /// second entry. Enforces invariant 2 (acyclic, no dangling parents) by
    /// rejecting any non-root commit whose parents are not already present.
    async fn put(&self, commit: Commit) -> Result<CommitId>;

    /// Fetches a commit by id, if it exists.
    async fn get(&self, id: &CommitId) -> Result<Option<Commit>>;

    /// True if a commit with this id has been stored.
    async fn contains(&self, id: &CommitId) -> Result<bool>;

    /// Direct children of a commit (reverse edge index), for ancestry walks.
    async fn children(&self, id: &CommitId) -> Result<Vec<CommitId>>;

    /// Total number of stored commits (used by gc / diagnostics).
    async fn len(&self) -> Result<usize>;

    async fn is_empty(&self) -> Result<bool> {
        Ok(self.len().await? == 0)
    }

    /// All commit ids currently stored. Used by `gc` to compute reachability.
    async fn all_ids(&self) -> Result<Vec<CommitId>>;

    /// Removes commits by id unconditionally. Callers (e.g. `gc`) are
    /// responsible for having already proven these ids are unreachable from
    /// any branch. Not exposed as a normal write-path operation.
    async fn remove_many(&self, ids: &[CommitId]) -> Result<()>;
}
