//! History walks: `log` over a commit's ancestry (or a branch's full
//! history, which is just the ancestry of its head), with metadata-tag
//! filtering and pagination.

use crate::commit::{Commit, CommitId};
use crate::error::{GraphError, Result};
use crate::store::CommitStore;
use serde::Serialize;
use std::collections::BTreeMap;

/// Filters commits by exact tag match. A commit must carry every listed
/// key/value pair (in `metadata.tags`) to pass. An empty filter matches
/// everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    pub tags: BTreeMap<String, String>,
}

impl LogFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }

    fn matches(&self, commit: &Commit) -> bool {
        self.tags
            .iter()
            .all(|(k, v)| commit.metadata.tags.get(k) == Some(v))
    }
}

/// A page of `log` results, newest (closest to the requested head) first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogPage {
    pub commits: Vec<Commit>,
    /// Total commits matching the filter, before pagination.
    pub total_matched: usize,
    pub has_more: bool,
}

