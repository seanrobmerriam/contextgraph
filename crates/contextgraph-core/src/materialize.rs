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

/// Walks the first-parent chain from `head` back to the root, applying
/// deltas in order (oldest first) and folding compaction ops as it goes.
///
/// First-parent walk is deliberate: it is what lets a merge commit's
/// materialized view be defined purely by parent order (see `merge`'s
/// `RecordOnly`/`PreferOther` strategies in milestone 6) without any special
/// casing here.
pub async fn materialize<S: CommitStore + ?Sized>(
    store: &S,
    head: CommitId,
) -> Result<MaterializedContext> {
    let mut chain = Vec::new();
    let mut current = Some(head);
    while let Some(id) = current {
        let commit = store
            .get(&id)
            .await?
            .ok_or(GraphError::CommitNotFound(id))?;
        current = commit.parent_ids.first().copied();
        chain.push(commit);
    }
    chain.reverse();

    let mut messages: Vec<MaterializedMessage> = Vec::new();
    let mut manifest: Vec<CommitId> = Vec::new();

    for commit in chain {
        let commit_id = commit.id;
        let author = commit.author;
        match commit.delta {
            Delta::Compaction { replaces, summary } => {
                let replaced: HashSet<CommitId> = replaces.iter().copied().collect();
                messages.retain(|m| !replaced.contains(&m.commit_id));
                manifest.retain(|id| !replaced.contains(id));
                messages.push(MaterializedMessage {
                    commit_id,
                    author,
                    delta: Delta::Compaction { replaces, summary },
                });
                manifest.push(commit_id);
            }
            // A merge commit is a pure audit/lineage marker: it contributes
            // to the manifest (so lineage of which commits led here is
            // complete) but never surfaces as a visible message. The
            // materialized *messages* for a merge commit are therefore
            // identical to its first parent's — "branch_a's view" exactly,
            // per the RecordOnly/PreferOther contract in `merge`.
            Delta::Merge { .. } => {
                manifest.push(commit_id);
            }
            other => {
                messages.push(MaterializedMessage {
                    commit_id,
                    author,
                    delta: other,
                });
                manifest.push(commit_id);
            }
        }
    }

    Ok(MaterializedContext {
        head,
        messages,
        manifest,
    })
}
