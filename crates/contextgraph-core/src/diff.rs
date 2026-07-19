//! Structural diff between two commits' materialized contexts: which turns
//! were added, removed, or held in common — never a text diff. Messages are
//! immutable, content-addressed atoms, so there is no "modify in place";
//! a changed turn shows up as a `Removed` entry for the old commit paired
//! with an `Added` entry for the new one, exactly as a line-level text diff
//! represents an edited line.

use crate::commit::CommitId;
use crate::materialize::{materialize, MaterializedMessage};
use crate::store::CommitStore;
use crate::Result;
use serde::Serialize;

/// One position in a structural diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DiffOp {
    /// Present, unchanged, in both sides (same commit id).
    Common(MaterializedMessage),
    /// Present only in `to` (the "new" side).
    Added(MaterializedMessage),
    /// Present only in `from` (the "old" side).
    Removed(MaterializedMessage),
}

/// A structural diff between two materialized contexts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextDiff {
    pub from: CommitId,
    pub to: CommitId,
    pub ops: Vec<DiffOp>,
}

impl ContextDiff {
    pub fn added(&self) -> impl Iterator<Item = &MaterializedMessage> {
        self.ops.iter().filter_map(|op| match op {
            DiffOp::Added(m) => Some(m),
            _ => None,
        })
    }

    pub fn removed(&self) -> impl Iterator<Item = &MaterializedMessage> {
        self.ops.iter().filter_map(|op| match op {
            DiffOp::Removed(m) => Some(m),
            _ => None,
        })
    }

    pub fn is_identical(&self) -> bool {
        self.ops.iter().all(|op| matches!(op, DiffOp::Common(_)))
    }
}

/// Diffs the materialized contexts of two arbitrary commits.
pub async fn diff<S: CommitStore + ?Sized>(
    store: &S,
    from: CommitId,
    to: CommitId,
) -> Result<ContextDiff> {
    let from_ctx = materialize(store, from).await?;
    let to_ctx = materialize(store, to).await?;
    let ops = lcs_diff(&from_ctx.messages, &to_ctx.messages);
    Ok(ContextDiff { from, to, ops })
}

/// Classic LCS-based sequence diff over commit ids, since two messages are
/// "the same" iff they came from the same (immutable) commit.
fn lcs_diff(a: &[MaterializedMessage], b: &[MaterializedMessage]) -> Vec<DiffOp> {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i].commit_id == b[j].commit_id {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i].commit_id == b[j].commit_id {
            ops.push(DiffOp::Common(a[i].clone()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffOp::Removed(a[i].clone()));
            i += 1;
        } else {
            ops.push(DiffOp::Added(b[j].clone()));
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp::Removed(a[i].clone()));
        i += 1;
    }
    while j < m {
        ops.push(DiffOp::Added(b[j].clone()));
        j += 1;
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{Author, Commit, Delta, Metadata};
    use crate::mem::InMemoryCommitStore;
    use chrono::Utc;

    fn msg(parents: Vec<CommitId>, text: &str) -> Commit {
        Commit::new(
            parents,
            Author::User,
            Delta::Message {
                content: text.to_string(),
            },
            Metadata::new(Utc::now()),
        )
    }

    #[tokio::test]
    async fn diffing_a_commit_against_itself_yields_no_added_or_removed() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], "hi");
        store.put(a.clone()).await.unwrap();
        let d = diff(&store, a.id, a.id).await.unwrap();
        assert!(d.is_identical());
        assert_eq!(d.added().count(), 0);
        assert_eq!(d.removed().count(), 0);
    }

    #[tokio::test]
    async fn diffing_two_forked_branches_shows_shared_prefix_as_common() {
        let store = InMemoryCommitStore::new();
        let root = msg(vec![], "root");
        store.put(root.clone()).await.unwrap();
        let shared = msg(vec![root.id], "shared");
        store.put(shared.clone()).await.unwrap();

        let a = msg(vec![shared.id], "branch-a-turn");
        store.put(a.clone()).await.unwrap();
        let b = msg(vec![shared.id], "branch-b-turn");
        store.put(b.clone()).await.unwrap();

        let d = diff(&store, a.id, b.id).await.unwrap();
        let commons: Vec<_> = d
            .ops
            .iter()
            .filter(|op| matches!(op, DiffOp::Common(_)))
            .collect();
        assert_eq!(commons.len(), 2); // root, shared
        assert_eq!(d.removed().count(), 1); // branch-a-turn
        assert_eq!(d.added().count(), 1); // branch-b-turn
    }

    #[tokio::test]
    async fn diffing_a_commit_against_its_own_child_shows_one_addition() {
        let store = InMemoryCommitStore::new();
        let root = msg(vec![], "root");
        store.put(root.clone()).await.unwrap();
        let child = msg(vec![root.id], "child");
        store.put(child.clone()).await.unwrap();

        let d = diff(&store, root.id, child.id).await.unwrap();
        assert_eq!(d.added().count(), 1);
        assert_eq!(d.removed().count(), 0);
        assert!(!d.is_identical());
    }

    #[tokio::test]
    async fn diffing_two_completely_unrelated_roots_shows_full_removal_and_addition() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], "a-only");
        store.put(a.clone()).await.unwrap();
        let b = msg(vec![], "b-only");
        store.put(b.clone()).await.unwrap();

        let d = diff(&store, a.id, b.id).await.unwrap();
        assert_eq!(d.removed().count(), 1);
        assert_eq!(d.added().count(), 1);
        assert_eq!(
            d.ops
                .iter()
                .filter(|op| matches!(op, DiffOp::Common(_)))
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn diffing_two_empty_equal_single_commits_is_identical() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], "");
        store.put(a.clone()).await.unwrap();
        let d = diff(&store, a.id, a.id).await.unwrap();
        assert!(d.is_identical());
    }
}
