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

    async fn bisect_flip_at(n: usize, flip_at: usize) -> (BisectOutcome, usize) {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, n).await;
        let good = ids[0];
        let bad = *ids.last().unwrap();
        let mut calls = 0usize;
        let outcome = bisect(&store, good, bad, |ctx| {
            calls += 1;
            index_of(ctx) < flip_at
        })
        .await
        .unwrap();
        (outcome, calls)
    }

    #[tokio::test]
    async fn bisect_finds_the_exact_known_flip_point_in_a_small_chain() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 10).await;
        let flip_at = 6; // predicate true for indices 0..6, false for 6..10
        let outcome = bisect(&store, ids[0], *ids.last().unwrap(), |ctx| {
            index_of(ctx) < flip_at
        })
        .await
        .unwrap();

        match outcome {
            BisectOutcome::Flip(r) => {
                assert_eq!(r.last_good, ids[flip_at - 1]);
                assert_eq!(r.first_bad, ids[flip_at]);
            }
            BisectOutcome::NoFlip => panic!("expected a flip"),
        }
    }

    #[tokio::test]
    async fn bisect_over_two_commit_range_resolves_without_looping() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 2).await;
        let outcome = bisect(&store, ids[0], ids[1], |ctx| index_of(ctx) < 1)
            .await
            .unwrap();
        match outcome {
            BisectOutcome::Flip(r) => {
                assert_eq!(r.last_good, ids[0]);
                assert_eq!(r.first_bad, ids[1]);
            }
            BisectOutcome::NoFlip => panic!("expected a flip"),
        }
    }

    #[tokio::test]
    async fn bisect_over_a_single_commit_range_does_not_panic_or_underflow() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 1).await;
        let outcome = bisect(&store, ids[0], ids[0], |_| true).await.unwrap();
        assert_eq!(outcome, BisectOutcome::NoFlip);
    }

    #[tokio::test]
    async fn bisect_with_a_predicate_that_never_flips_reports_no_flip_cleanly() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 8).await;
        let outcome = bisect(&store, ids[0], *ids.last().unwrap(), |_| true)
            .await
            .unwrap();
        assert_eq!(outcome, BisectOutcome::NoFlip);
    }

    #[tokio::test]
    async fn bisect_where_good_already_exhibits_bad_behavior_reports_no_flip() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 5).await;
        let outcome = bisect(&store, ids[0], *ids.last().unwrap(), |_| false)
            .await
            .unwrap();
        assert_eq!(outcome, BisectOutcome::NoFlip);
    }

    #[tokio::test]
    async fn bisect_from_a_commit_that_is_not_an_ancestor_fails() {
        let store = InMemoryCommitStore::new();
        let a = build_chain(&store, 3).await;
        let unrelated = Commit::new(
            vec![],
            Author::User,
            Delta::Message {
                content: "unrelated".into(),
            },
            Metadata::new(Utc::now()),
        );
        let unrelated_id = store.put(unrelated).await.unwrap();

        let err = bisect(&store, unrelated_id, *a.last().unwrap(), |_| true)
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::InvalidBisectRange(_, _)));
    }

    #[tokio::test]
    async fn bisect_performs_logarithmically_many_predicate_calls_across_sizes() {
        // Power-of-two-adjacent sizes are where off-by-one bisect bugs
        // cluster, so exercise a spread of them.
        for &n in &[2usize, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65] {
            let flip_at = n / 2;
            let (outcome, calls) = bisect_flip_at(n, flip_at.max(1)).await;
            assert!(matches!(outcome, BisectOutcome::Flip(_)), "n={n}");
            let max_expected = (n as f64).log2().ceil() as usize + 2;
            assert!(
                calls <= max_expected,
                "n={n} used {calls} predicate calls, expected <= {max_expected}"
            );
        }
    }
}
