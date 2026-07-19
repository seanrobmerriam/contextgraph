//! In-memory fakes of `CommitStore` and `RefStore`, for fast unit/property
//! tests and as a reference implementation of the storage contract.

use crate::commit::{Commit, CommitId};
use crate::error::{GraphError, Result};
use crate::store::{CommitStore, RefStore};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

#[derive(Default)]
pub struct InMemoryCommitStore {
    commits: RwLock<HashMap<CommitId, Commit>>,
    children: RwLock<HashMap<CommitId, HashSet<CommitId>>>,
}

impl InMemoryCommitStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_err(_: impl std::fmt::Debug) -> GraphError {
        GraphError::Storage("in-memory commit store lock poisoned".into())
    }
}

#[async_trait]
impl CommitStore for InMemoryCommitStore {
    async fn put(&self, commit: Commit) -> Result<CommitId> {
        let mut commits = self.commits.write().map_err(Self::lock_err)?;

        if commits.contains_key(&commit.id) {
            return Ok(commit.id);
        }

        for parent in &commit.parent_ids {
            if !commits.contains_key(parent) {
                return Err(GraphError::ParentNotFound(*parent));
            }
        }

        let id = commit.id;
        let parent_ids = commit.parent_ids.clone();
        commits.insert(id, commit);
        drop(commits);

        let mut children = self.children.write().map_err(Self::lock_err)?;
        for parent in parent_ids {
            children.entry(parent).or_default().insert(id);
        }
        children.entry(id).or_default();

        Ok(id)
    }

    async fn get(&self, id: &CommitId) -> Result<Option<Commit>> {
        let commits = self.commits.read().map_err(Self::lock_err)?;
        Ok(commits.get(id).cloned())
    }

    async fn contains(&self, id: &CommitId) -> Result<bool> {
        let commits = self.commits.read().map_err(Self::lock_err)?;
        Ok(commits.contains_key(id))
    }

    async fn children(&self, id: &CommitId) -> Result<Vec<CommitId>> {
        let children = self.children.read().map_err(Self::lock_err)?;
        Ok(children
            .get(id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default())
    }

    async fn len(&self) -> Result<usize> {
        let commits = self.commits.read().map_err(Self::lock_err)?;
        Ok(commits.len())
    }

    async fn all_ids(&self) -> Result<Vec<CommitId>> {
        let commits = self.commits.read().map_err(Self::lock_err)?;
        Ok(commits.keys().copied().collect())
    }

    async fn remove_many(&self, ids: &[CommitId]) -> Result<()> {
        let mut commits = self.commits.write().map_err(Self::lock_err)?;
        let mut children = self.children.write().map_err(Self::lock_err)?;
        for id in ids {
            if let Some(commit) = commits.remove(id) {
                for parent in &commit.parent_ids {
                    if let Some(set) = children.get_mut(parent) {
                        set.remove(id);
                    }
                }
            }
            children.remove(id);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryRefStore {
    branches: RwLock<HashMap<String, CommitId>>,
}

impl InMemoryRefStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_err(_: impl std::fmt::Debug) -> GraphError {
        GraphError::Storage("in-memory ref store lock poisoned".into())
    }
}

#[async_trait]
impl RefStore for InMemoryRefStore {
    async fn get_branch(&self, name: &str) -> Result<Option<CommitId>> {
        let branches = self.branches.read().map_err(Self::lock_err)?;
        Ok(branches.get(name).copied())
    }

    async fn set_branch(&self, name: &str, commit_id: CommitId) -> Result<()> {
        let mut branches = self.branches.write().map_err(Self::lock_err)?;
        branches.insert(name.to_string(), commit_id);
        Ok(())
    }

    async fn delete_branch(&self, name: &str) -> Result<()> {
        let mut branches = self.branches.write().map_err(Self::lock_err)?;
        if branches.remove(name).is_none() {
            return Err(GraphError::BranchNotFound(name.to_string()));
        }
        Ok(())
    }

    async fn list_branches(&self) -> Result<Vec<(String, CommitId)>> {
        let branches = self.branches.read().map_err(Self::lock_err)?;
        Ok(branches.iter().map(|(k, v)| (k.clone(), *v)).collect())
    }
}

/// Combines `InMemoryCommitStore` + `InMemoryRefStore` behind a single type
/// that implements both traits, so it can back `ContextGraph<S>` directly
/// (the same shape production code gets from `SqliteStore`).
#[derive(Default)]
pub struct InMemoryGraphStore {
    commits: InMemoryCommitStore,
    refs: InMemoryRefStore,
}

impl InMemoryGraphStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CommitStore for InMemoryGraphStore {
    async fn put(&self, commit: Commit) -> Result<CommitId> {
        self.commits.put(commit).await
    }

    async fn get(&self, id: &CommitId) -> Result<Option<Commit>> {
        self.commits.get(id).await
    }

    async fn contains(&self, id: &CommitId) -> Result<bool> {
        self.commits.contains(id).await
    }

    async fn children(&self, id: &CommitId) -> Result<Vec<CommitId>> {
        self.commits.children(id).await
    }

    async fn len(&self) -> Result<usize> {
        self.commits.len().await
    }

    async fn all_ids(&self) -> Result<Vec<CommitId>> {
        self.commits.all_ids().await
    }

    async fn remove_many(&self, ids: &[CommitId]) -> Result<()> {
        self.commits.remove_many(ids).await
    }
}

#[async_trait]
impl RefStore for InMemoryGraphStore {
    async fn get_branch(&self, name: &str) -> Result<Option<CommitId>> {
        self.refs.get_branch(name).await
    }

    async fn set_branch(&self, name: &str, commit_id: CommitId) -> Result<()> {
        self.refs.set_branch(name, commit_id).await
    }

    async fn delete_branch(&self, name: &str) -> Result<()> {
        self.refs.delete_branch(name).await
    }

    async fn list_branches(&self) -> Result<Vec<(String, CommitId)>> {
        self.refs.list_branches().await
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
    async fn putting_a_commit_with_nonexistent_parent_fails() {
        let store = InMemoryCommitStore::new();
        let orphan = Commit::new(
            vec![CommitId([9; 32])],
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
    async fn putting_root_commit_succeeds_and_is_retrievable() {
        let store = InMemoryCommitStore::new();
        let r = root();
        let id = store.put(r.clone()).await.unwrap();
        assert_eq!(id, r.id);
        let fetched = store.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched, r);
    }

    #[tokio::test]
    async fn putting_identical_commit_twice_does_not_duplicate_storage() {
        let store = InMemoryCommitStore::new();
        let r = root();
        store.put(r.clone()).await.unwrap();
        store.put(r.clone()).await.unwrap();
        assert_eq!(store.len().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn child_commit_is_indexed_under_its_parent() {
        let store = InMemoryCommitStore::new();
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
    async fn getting_nonexistent_commit_returns_none_not_error() {
        let store = InMemoryCommitStore::new();
        let result = store.get(&CommitId([1; 32])).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn branching_at_a_commit_then_reading_it_back_round_trips() {
        let refs = InMemoryRefStore::new();
        let id = CommitId([1; 32]);
        refs.set_branch("main", id).await.unwrap();
        assert_eq!(refs.get_branch("main").await.unwrap(), Some(id));
    }

    #[tokio::test]
    async fn deleting_a_nonexistent_branch_fails() {
        let refs = InMemoryRefStore::new();
        let err = refs.delete_branch("nope").await.unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }

    #[tokio::test]
    async fn moving_a_branch_pointer_overwrites_the_previous_target() {
        let refs = InMemoryRefStore::new();
        let a = CommitId([1; 32]);
        let b = CommitId([2; 32]);
        refs.set_branch("main", a).await.unwrap();
        refs.set_branch("main", b).await.unwrap();
        assert_eq!(refs.get_branch("main").await.unwrap(), Some(b));
    }

    #[tokio::test]
    async fn removing_commits_cleans_up_child_index() {
        let store = InMemoryCommitStore::new();
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
        store.remove_many(&[child.id]).await.unwrap();
        assert_eq!(store.children(&r.id).await.unwrap(), Vec::<CommitId>::new());
        assert_eq!(store.len().await.unwrap(), 1);
    }
}
