//! Materialization: turning a DAG walk into the ordered message list a model
//! would actually see, folding compaction ops as one more delta type rather
//! than a special case bolted onto the walk.

use crate::commit::{Author, CommitId, Delta};
use crate::error::{GraphError, Result};
use crate::store::CommitStore;
use serde::Serialize;
use std::collections::HashSet;

/// One turn in a materialized context: the delta contributed by a single
/// commit, tagged with the commit that contributed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaterializedMessage {
    pub commit_id: CommitId,
    pub author: Author,
    pub delta: Delta,
}

/// The ordered message list a model would see when checked out at `head`,
/// plus a manifest of which commits currently contribute to it. Compaction
/// ops remove entries from `messages`/`manifest` without touching the
/// underlying DAG — the commits they replace remain reachable by id, just
/// not part of this projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaterializedContext {
    pub head: CommitId,
    pub messages: Vec<MaterializedMessage>,
    pub manifest: Vec<CommitId>,
}
