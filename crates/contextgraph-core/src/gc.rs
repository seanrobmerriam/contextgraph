//! `gc`: explicit, opt-in garbage collection. Never runs as a side effect of
//! normal use — commits are only ever removed when a caller deliberately
//! invokes this.

use crate::commit::CommitId;
use crate::error::Result;
use crate::store::{CommitStore, RefStore};
use std::collections::HashSet;

/// All commit ids reachable from any branch pointer, following *every*
/// parent edge (not just first-parent) — a merge commit's second parent
/// must stay reachable too.
pub async fn reachable_from_branches<S: CommitStore + RefStore + ?Sized>(
    store: &S,
) -> Result<HashSet<CommitId>> {
    let branches = store.list_branches().await?;
    let mut visited = HashSet::new();
    let mut stack: Vec<CommitId> = branches.into_iter().map(|(_, id)| id).collect();

    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        if let Some(commit) = store.get(&id).await? {
            stack.extend(commit.parent_ids);
        }
    }
    Ok(visited)
}
