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
