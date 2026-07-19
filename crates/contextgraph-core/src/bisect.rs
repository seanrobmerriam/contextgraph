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
