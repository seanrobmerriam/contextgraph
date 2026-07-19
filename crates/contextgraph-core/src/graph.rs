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
