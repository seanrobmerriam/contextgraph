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
