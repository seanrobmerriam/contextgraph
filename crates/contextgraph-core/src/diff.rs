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
