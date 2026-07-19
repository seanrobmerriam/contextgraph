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
///
/// ```
/// use contextgraph_core::{CommitId, graph::CheckoutTarget};
/// let by_id: CheckoutTarget = CommitId::from_bytes([0u8; 32]).into();
/// let by_branch = CheckoutTarget::branch("main");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutTarget {
    Commit(CommitId),
    Branch(String),
}

impl CheckoutTarget {
    /// Construct from a specific commit id.
    pub fn commit(id: CommitId) -> Self {
        Self::Commit(id)
    }

    /// Construct from a branch name.
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
    ///
    /// # Errors
    ///
    /// * [`GraphError::CommitNotFound`] if a commit target does not exist.
    /// * [`GraphError::BranchNotFound`] if a branch target does not exist.
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
    ///
    /// # Errors
    ///
    /// * [`GraphError::ParentNotFound`] if `parent` is `Some` and the
    ///   referenced commit is not in the store.
    /// * [`GraphError::Storage`] if the underlying store fails to persist.
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
    /// Commits a new turn and advances `branch_name` to point at it in one
    /// step. The commit is durably written before the branch pointer moves,
    /// so no reader can ever observe a branch pointing at a not-yet-persisted
    /// commit.
    ///
    /// # Errors
    ///
    /// Any error returned by [`commit`](Self::commit) (e.g. parent not
    /// found) or [`store.set_branch`](CommitStore::put).
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
    ///
    /// # Errors
    ///
    /// * [`GraphError::CommitNotFound`] if `at_commit_id` is not in the store.
    /// * [`GraphError::BranchAlreadyExists`] if a branch named `name` is
    ///   already defined.
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
    ///
    /// # Errors
    ///
    /// * [`GraphError::BranchNotFound`] if no branch named `name` exists.
    /// * [`GraphError::CommitNotFound`] if `to_commit_id` is not in the store.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::InMemoryGraphStore;
    use chrono::Utc;

    fn meta() -> Metadata {
        Metadata::new(Utc::now())
    }

    fn text(s: &str) -> Delta {
        Delta::Message {
            content: s.to_string(),
        }
    }

    #[tokio::test]
    async fn committing_with_no_parent_creates_a_root_commit() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let id = g
            .commit(None, Author::User, text("hi"), meta())
            .await
            .unwrap();
        let ctx = g.checkout(CheckoutTarget::commit(id)).await.unwrap();
        assert_eq!(ctx.messages.len(), 1);
    }

    #[tokio::test]
    async fn committing_with_nonexistent_parent_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let err = g
            .commit(Some(CommitId::from_bytes([1; 32])), Author::User, text("hi"), meta())
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::ParentNotFound(_)));
    }

    #[tokio::test]
    async fn branching_at_a_nonexistent_commit_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let err = g.branch("main", CommitId::from_bytes([1; 32])).await.unwrap_err();
        assert!(matches!(err, GraphError::CommitNotFound(_)));
    }

    #[tokio::test]
    async fn branching_with_an_already_used_name_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let id = g
            .commit(None, Author::User, text("hi"), meta())
            .await
            .unwrap();
        g.branch("main", id).await.unwrap();
        let err = g.branch("main", id).await.unwrap_err();
        assert!(matches!(err, GraphError::BranchAlreadyExists(_)));
    }

    #[tokio::test]
    async fn moving_a_nonexistent_branch_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let id = g
            .commit(None, Author::User, text("hi"), meta())
            .await
            .unwrap();
        let err = g.move_branch("nope", id).await.unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }

    #[tokio::test]
    async fn moving_a_branch_to_a_nonexistent_commit_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let id = g
            .commit(None, Author::User, text("hi"), meta())
            .await
            .unwrap();
        g.branch("main", id).await.unwrap();
        let err = g.move_branch("main", CommitId::from_bytes([9; 32])).await.unwrap_err();
        assert!(matches!(err, GraphError::CommitNotFound(_)));
    }

    #[tokio::test]
    async fn deleting_a_nonexistent_branch_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let err = g.delete_branch("nope").await.unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }

    #[tokio::test]
    async fn checking_out_a_nonexistent_branch_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let err = g
            .checkout(CheckoutTarget::branch("nope"))
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }

    #[tokio::test]
    async fn checking_out_a_nonexistent_commit_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let err = g
            .checkout(CheckoutTarget::commit(CommitId::from_bytes([1; 32])))
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::CommitNotFound(_)));
    }

    #[tokio::test]
    async fn commit_advancing_branch_creates_commit_and_moves_pointer_atomically_in_order() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let id1 = g
            .commit_advancing_branch("main", Author::User, text("one"), meta())
            .await
            .unwrap();
        assert_eq!(g.store().get_branch("main").await.unwrap(), Some(id1));

        let id2 = g
            .commit_advancing_branch("main", Author::Assistant, text("two"), meta())
            .await
            .unwrap();
        assert_eq!(g.store().get_branch("main").await.unwrap(), Some(id2));

        let ctx = g.checkout(CheckoutTarget::branch("main")).await.unwrap();
        assert_eq!(ctx.messages.len(), 2);
    }

    #[tokio::test]
    async fn checking_out_same_commit_twice_yields_byte_identical_materialized_context() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("hi"), meta())
            .await
            .unwrap();
        let child = g
            .commit(Some(root), Author::Assistant, text("there"), meta())
            .await
            .unwrap();

        let a = g.checkout(CheckoutTarget::commit(child)).await.unwrap();
        let b = g.checkout(CheckoutTarget::commit(child)).await.unwrap();
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn committing_identical_content_via_two_different_parents_paths_still_dedupes() {
        // Two branches both commit the exact same text on top of the same
        // parent: this must produce one commit id, referenced from both
        // branches (free storage dedupe across forks).
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("hi"), meta())
            .await
            .unwrap();
        g.branch("a", root).await.unwrap();
        g.branch("b", root).await.unwrap();

        let shared_meta = meta();
        let via_a = g
            .commit(
                Some(root),
                Author::Assistant,
                text("dup"),
                shared_meta.clone(),
            )
            .await
            .unwrap();
        let via_b = g
            .commit(Some(root), Author::Assistant, text("dup"), shared_meta)
            .await
            .unwrap();
        assert_eq!(via_a, via_b);
    }

    #[tokio::test]
    async fn forking_from_an_existing_commit_creates_an_independent_branch() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("plan"), meta())
            .await
            .unwrap();
        g.fork(root, "speculative-a").await.unwrap();
        assert_eq!(
            g.store().get_branch("speculative-a").await.unwrap(),
            Some(root)
        );
    }

    #[tokio::test]
    async fn forking_from_a_fork_produces_a_third_independent_branch() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("plan"), meta())
            .await
            .unwrap();
        g.fork(root, "fork-1").await.unwrap();
        let child = g
            .commit(Some(root), Author::Assistant, text("step"), meta())
            .await
            .unwrap();
        g.move_branch("fork-1", child).await.unwrap();

        // Forking again, from the fork's current head.
        g.fork(child, "fork-2").await.unwrap();
        assert_eq!(g.store().get_branch("fork-2").await.unwrap(), Some(child));
        // The original fork is untouched by the new one.
        assert_eq!(g.store().get_branch("fork-1").await.unwrap(), Some(child));
    }

    #[tokio::test]
    async fn forking_from_a_nonexistent_commit_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let err = g.fork(CommitId::from_bytes([1; 32]), "x").await.unwrap_err();
        assert!(matches!(err, GraphError::CommitNotFound(_)));
    }

    #[tokio::test]
    async fn rollback_moves_branch_backward_without_deleting_forward_commits() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit_advancing_branch("main", Author::User, text("one"), meta())
            .await
            .unwrap();
        let ahead = g
            .commit_advancing_branch("main", Author::Assistant, text("two"), meta())
            .await
            .unwrap();

        g.rollback("main", root).await.unwrap();
        assert_eq!(g.store().get_branch("main").await.unwrap(), Some(root));

        // The commit that was "ahead" is still reachable by hash.
        assert!(g.store().contains(&ahead).await.unwrap());
        let ctx = g.checkout(CheckoutTarget::commit(ahead)).await.unwrap();
        assert_eq!(ctx.messages.len(), 2);
    }

    #[tokio::test]
    async fn rollback_of_a_nonexistent_branch_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("hi"), meta())
            .await
            .unwrap();
        let err = g.rollback("nope", root).await.unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }

    #[tokio::test]
    async fn log_over_a_branch_name_resolves_its_current_head_first() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        g.commit_advancing_branch("main", Author::User, text("one"), meta())
            .await
            .unwrap();
        let second = g
            .commit_advancing_branch("main", Author::Assistant, text("two"), meta())
            .await
            .unwrap();

        let page = g
            .log(
                CheckoutTarget::branch("main"),
                &crate::log::LogFilter::new(),
                0,
                10,
            )
            .await
            .unwrap();
        assert_eq!(page.commits[0].id, second);
        assert_eq!(page.total_matched, 2);
    }

    #[tokio::test]
    async fn diff_between_two_branch_heads_resolves_each_branch_first() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("root"), meta())
            .await
            .unwrap();
        g.branch("a", root).await.unwrap();
        g.branch("b", root).await.unwrap();
        let a_head = g
            .commit(Some(root), Author::Assistant, text("a-only"), meta())
            .await
            .unwrap();
        g.move_branch("a", a_head).await.unwrap();

        let d = g
            .diff(CheckoutTarget::branch("a"), CheckoutTarget::branch("b"))
            .await
            .unwrap();
        assert_eq!(d.added().count(), 0);
        assert_eq!(d.removed().count(), 1);
    }

    #[tokio::test]
    async fn diffing_against_a_nonexistent_branch_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("root"), meta())
            .await
            .unwrap();
        g.branch("a", root).await.unwrap();
        let err = g
            .diff(CheckoutTarget::branch("a"), CheckoutTarget::branch("nope"))
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }

    #[tokio::test]
    async fn merging_two_branches_through_context_graph_advances_branch_a_only() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let root = g
            .commit(None, Author::User, text("root"), meta())
            .await
            .unwrap();
        g.branch("a", root).await.unwrap();
        g.branch("b", root).await.unwrap();
        let a_head = g
            .commit(Some(root), Author::Assistant, text("a-turn"), meta())
            .await
            .unwrap();
        g.move_branch("a", a_head).await.unwrap();
        let b_head = g
            .commit(Some(root), Author::Assistant, text("b-turn"), meta())
            .await
            .unwrap();
        g.move_branch("b", b_head).await.unwrap();

        let merge_id = g
            .merge("a", "b", crate::commit::MergeStrategy::RecordOnly)
            .await
            .unwrap();
        assert_eq!(g.store().get_branch("a").await.unwrap(), Some(merge_id));
        assert_eq!(g.store().get_branch("b").await.unwrap(), Some(b_head));
    }

    #[tokio::test]
    async fn log_over_a_nonexistent_branch_fails() {
        let g = ContextGraph::new(InMemoryGraphStore::new());
        let err = g
            .log(
                CheckoutTarget::branch("nope"),
                &crate::log::LogFilter::new(),
                0,
                10,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }
}
