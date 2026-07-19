//! `bisect`: git-bisect-for-agent-runs. Given a `good` commit (predicate
//! holds) and a `bad` commit (predicate has flipped), finds the exact
//! commit along their ancestry path where the predicate first goes false.
//!
//! Building the candidate path from `bad` back to `good` is an unavoidable
//! O(depth) pointer-chase (there's no way to know the range without
//! visiting it, same as `git rev-list` before a real bisect). The actual
//! search *within* that path — the part that matters, because evaluating
//! the predicate is the expensive step (materializing + checking agent
//! behavior) — is a proper binary search: O(log n) predicate calls, never
//! a linear scan.

use crate::commit::CommitId;
use crate::error::{GraphError, Result};
use crate::materialize::{materialize, MaterializedContext};
use crate::store::CommitStore;
use serde::Serialize;

/// The outcome of a bisect run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum BisectOutcome {
    /// The predicate flips somewhere strictly between `good` and `bad`.
    Flip(BisectResult),
    /// The predicate never flips across the given range (it's `true`
    /// (still "good") at `bad` too, or already `false` at `good`). This is
    /// a normal, clearly-reported outcome — not an error.
    NoFlip,
}

/// The two adjacent commits straddling the flip point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BisectResult {
    /// The last commit (closest to `bad`) where the predicate still held.
    pub last_good: CommitId,
    /// The first commit (closest to `good`) where the predicate no longer held.
    pub first_bad: CommitId,
    /// Number of predicate evaluations performed (for verifying O(log n)).
    pub predicate_calls: usize,
}

/// Walks the first-parent ancestry from `bad` back to `good`, returning the
/// path in root-to-descendant order (`good` first, `bad` last). Fails if
/// `good` is not on `bad`'s first-parent ancestry path.
async fn ancestry_path<S: CommitStore + ?Sized>(
    store: &S,
    good: CommitId,
    bad: CommitId,
) -> Result<Vec<CommitId>> {
    let mut path = vec![bad];
    let mut current = bad;
    while current != good {
        let commit = store
            .get(&current)
            .await?
            .ok_or(GraphError::CommitNotFound(current))?;
        match commit.parent_ids.first() {
            Some(parent) => {
                current = *parent;
                path.push(current);
            }
            None => return Err(GraphError::InvalidBisectRange(good, bad)),
        }
    }
    path.reverse();
    Ok(path)
}

/// Binary-searches the ancestry path between `good` and `bad` for the exact
/// commit where `predicate` flips from `true` to `false`.
///
/// Contract: `predicate` is expected to be monotonic along the path — true
/// for a prefix starting at `good`, false for a suffix ending at `bad`.
/// Constructed test DAGs uphold this by design; if it doesn't hold in
/// practice (e.g. flaky predicate), the search still terminates in
/// O(log n) calls but the reported boundary is only meaningful under the
/// monotonicity assumption, exactly as with `git bisect`.
pub async fn bisect<S, F>(
    store: &S,
    good: CommitId,
    bad: CommitId,
    mut predicate: F,
) -> Result<BisectOutcome>
where
    S: CommitStore + ?Sized,
    F: FnMut(&MaterializedContext) -> bool,
{
    let path = ancestry_path(store, good, bad).await?;

    // Trivial range: a single commit has no boundary to find.
    if path.len() == 1 {
        return Ok(BisectOutcome::NoFlip);
    }

    let mut calls = 0usize;
    let mut lo = 0usize;
    let mut hi = path.len() - 1;

    let good_ctx = materialize(store, path[lo]).await?;
    calls += 1;
    if !predicate(&good_ctx) {
        return Ok(BisectOutcome::NoFlip);
    }

    let bad_ctx = materialize(store, path[hi]).await?;
    calls += 1;
    if predicate(&bad_ctx) {
        return Ok(BisectOutcome::NoFlip);
    }

    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let mid_ctx = materialize(store, path[mid]).await?;
        calls += 1;
        if predicate(&mid_ctx) {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    Ok(BisectOutcome::Flip(BisectResult {
        last_good: path[lo],
        first_bad: path[hi],
        predicate_calls: calls,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{Author, Commit, Delta, Metadata};
    use crate::mem::InMemoryCommitStore;
    use chrono::Utc;

    /// Builds a linear chain of `n` commits, tagging each with its index so
    /// tests can define a predicate purely in terms of position.
    async fn build_chain(store: &InMemoryCommitStore, n: usize) -> Vec<CommitId> {
        let mut ids = Vec::new();
        let mut parent = None;
        for i in 0..n {
            let commit = Commit::new(
                parent.into_iter().collect(),
                Author::User,
                Delta::Message {
                    content: format!("turn-{i}"),
                },
                Metadata::new(Utc::now()).with_tag("index", i.to_string()),
            );
            let id = store.put(commit).await.unwrap();
            ids.push(id);
            parent = Some(id);
        }
        ids
    }

    fn index_of(ctx: &MaterializedContext) -> usize {
        // The predicate operates on the tag of the *last* contributing
        // commit's tag, which we look up via the manifest's tail — but tags
        // live on Metadata, not on MaterializedMessage, so tests instead
        // decode the index from the message content ("turn-N").
        let last = ctx.messages.last().unwrap();
        match &last.delta {
            Delta::Message { content } => content
                .strip_prefix("turn-")
                .and_then(|s| s.parse().ok())
                .unwrap(),
            _ => unreachable!(),
        }
    }
