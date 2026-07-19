//! `ContextGraph`: the embeddable library entry point combining a
//! `CommitStore` + `RefStore` (usually the same backing object) with the
//! guard-clause validation and transaction ordering the spec requires.

use crate::bisect::{bisect as bisect_commits, BisectOutcome};
use crate::commit::{Author, Commit, CommitId, Delta, MergeStrategy, Metadata};
use crate::diff::{diff as diff_commits, ContextDiff};
use crate::error::{GraphError, Result};
use crate::gc::gc as gc_store;
use crate::log::{log_ancestors, LogFilter, LogPage};
use crate::materialize::{materialize, MaterializedContext};
use crate::merge::merge as merge_branches;
use crate::store::{CommitStore, RefStore};

/// What to check out: a specific commit, or the current head of a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutTarget {
    Commit(CommitId),
    Branch(String),
}

impl CheckoutTarget {
    pub fn commit(id: CommitId) -> Self {
        Self::Commit(id)
    }

    pub fn branch(name: impl Into<String>) -> Self {
        Self::Branch(name.into())
    }
}

impl From<CommitId> for CheckoutTarget {
    fn from(id: CommitId) -> Self {
        Self::Commit(id)
    }
}

/// The embeddable core API: an agent runtime links this directly and calls
/// `commit`/`checkout` inline per turn.
pub struct ContextGraph<S> {
    store: S,
}

impl<S: CommitStore + RefStore> ContextGraph<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// Resolves a checkout target to a concrete commit id, without
    /// materializing.
    pub async fn resolve(&self, target: &CheckoutTarget) -> Result<CommitId> {
        match target {
            CheckoutTarget::Commit(id) => {
                if !self.store.contains(id).await? {
                    return Err(GraphError::CommitNotFound(*id));
                }
                Ok(*id)
            }
            CheckoutTarget::Branch(name) => self
                .store
                .get_branch(name)
                .await?
                .ok_or_else(|| GraphError::BranchNotFound(name.clone())),
        }
    }

    /// Creates a new commit as a child of `parent` (`None` for a root
    /// commit). Content-addressed and idempotent: identical content always
    /// returns the same id without creating a duplicate node. Does not move
    /// any branch pointer — see `commit_advancing_branch` for the combined
    /// op used by normal per-turn agent loops.
    pub async fn commit(
        &self,
        parent: Option<CommitId>,
        author: Author,
        delta: Delta,
        metadata: Metadata,
    ) -> Result<CommitId> {
        if let Some(p) = parent {
            if !self.store.contains(&p).await? {
                return Err(GraphError::ParentNotFound(p));
            }
        }
        let parent_ids = parent.into_iter().collect::<Vec<_>>();
        let commit = Commit::new(parent_ids, author, delta, metadata);
        self.store.put(commit).await
    }

    /// Commits a new turn and advances `branch_name` to point at it in one
    /// step. The commit is durably written before the branch pointer moves,
    /// so no reader can ever observe a branch pointing at a not-yet-persisted
    /// commit.
    pub async fn commit_advancing_branch(
        &self,
        branch_name: &str,
        author: Author,
        delta: Delta,
        metadata: Metadata,
    ) -> Result<CommitId> {
        let parent = self.store.get_branch(branch_name).await?;
        let id = self.commit(parent, author, delta, metadata).await?;
        self.store.set_branch(branch_name, id).await?;
        Ok(id)
    }

    /// Creates a new branch pointer at an existing commit. Fails if the
    /// branch already exists or the commit does not.
    pub async fn branch(&self, name: &str, at_commit_id: CommitId) -> Result<()> {
        if !self.store.contains(&at_commit_id).await? {
            return Err(GraphError::CommitNotFound(at_commit_id));
        }
        if self.store.get_branch(name).await?.is_some() {
            return Err(GraphError::BranchAlreadyExists(name.to_string()));
        }
        self.store.set_branch(name, at_commit_id).await
    }

    /// Moves an existing branch pointer to a different (existing) commit.
    pub async fn move_branch(&self, name: &str, to_commit_id: CommitId) -> Result<()> {
        if self.store.get_branch(name).await?.is_none() {
            return Err(GraphError::BranchNotFound(name.to_string()));
        }
        if !self.store.contains(&to_commit_id).await? {
            return Err(GraphError::CommitNotFound(to_commit_id));
        }
        self.store.set_branch(name, to_commit_id).await
    }

    pub async fn delete_branch(&self, name: &str) -> Result<()> {
        self.store.delete_branch(name).await
    }

    pub async fn list_branches(&self) -> Result<Vec<(String, CommitId)>> {
        self.store.list_branches().await
    }

    /// Materializes the ordered message list a model would see at `target`.
    pub async fn checkout(&self, target: CheckoutTarget) -> Result<MaterializedContext> {
        let head = self.resolve(&target).await?;
        materialize(&self.store, head).await
    }

    /// Explicit speculative-branch creation: "try two continuations from the
    /// same decision point" in one call. Semantically identical to `branch`
    /// (a fork *is* a branch at an existing commit) but exposed under its
    /// own name since it's the primary developer-facing verb for A/B'ing a
    /// decision point — including forking again from an existing fork.
    pub async fn fork(&self, from_commit_id: CommitId, new_branch_name: &str) -> Result<()> {
        self.branch(new_branch_name, from_commit_id).await
    }

    /// Moves `branch_name` backward to `to_commit_id`. Matches git's `reset`
    /// semantics: commits "ahead" of the new position are not deleted — they
    /// remain reachable by hash, or by any other branch still pointing past
    /// them.
    pub async fn rollback(&self, branch_name: &str, to_commit_id: CommitId) -> Result<()> {
        self.move_branch(branch_name, to_commit_id).await
    }

    /// Walks the ancestry of `target` (a commit, or a branch's current
    /// head) newest-first, applying `filter` and then offset/limit
    /// pagination.
    pub async fn log(
        &self,
        target: CheckoutTarget,
        filter: &LogFilter,
        offset: usize,
        limit: usize,
    ) -> Result<LogPage> {
        let head = self.resolve(&target).await?;
        log_ancestors(&self.store, head, filter, offset, limit).await
    }

    /// Structural diff between two materialized contexts. `from`/`to` can
    /// each independently be a commit or a branch's current head, so this
    /// covers both the commit-pair and branch-pair forms.
    pub async fn diff(&self, from: CheckoutTarget, to: CheckoutTarget) -> Result<ContextDiff> {
        let from_id = self.resolve(&from).await?;
        let to_id = self.resolve(&to).await?;
        diff_commits(&self.store, from_id, to_id).await
    }

    /// Merges `branch_b` into `branch_a`, creating an explicit merge commit
    /// and advancing `branch_a` to it. See `crate::merge` for why this never
    /// silently merges divergent conversation content.
    pub async fn merge(
        &self,
        branch_a: &str,
        branch_b: &str,
        strategy: MergeStrategy,
    ) -> Result<CommitId> {
        merge_branches(&self.store, branch_a, branch_b, strategy).await
    }

    /// Binary-searches the ancestry path between `good` and `bad` for the
    /// exact commit where `predicate` flips from true to false.
    pub async fn bisect<F>(
        &self,
        good: CommitId,
        bad: CommitId,
        predicate: F,
    ) -> Result<BisectOutcome>
    where
        F: FnMut(&MaterializedContext) -> bool,
    {
        bisect_commits(&self.store, good, bad, predicate).await
    }

    /// Explicit, opt-in garbage collection: removes every commit not
    /// reachable from any branch. Never runs implicitly.
    pub async fn gc(&self) -> Result<Vec<CommitId>> {
        gc_store(&self.store).await
    }
}
