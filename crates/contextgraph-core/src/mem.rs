//! In-memory fakes of `CommitStore` and `RefStore`, for fast unit/property
//! tests and as a reference implementation of the storage contract.

use crate::commit::{Commit, CommitId};
use crate::error::{GraphError, Result};
use crate::store::{CommitStore, RefStore};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

#[derive(Default)]
pub struct InMemoryCommitStore {
    commits: RwLock<HashMap<CommitId, Commit>>,
    children: RwLock<HashMap<CommitId, HashSet<CommitId>>>,
}

impl InMemoryCommitStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_err(_: impl std::fmt::Debug) -> GraphError {
        GraphError::Storage("in-memory commit store lock poisoned".into())
    }
}
