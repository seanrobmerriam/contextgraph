//! `merge`: reconciling two branches representing alternative continuations.
//!
//! Merging conversation branches is fundamentally different from merging
//! code: two forked branches are usually alternative continuations, not
//! compatible edits to reconcile line-by-line. There is no automatic
//! content merging of divergent natural-language turns here, and this
//! module does not attempt to invent one. Instead, `merge` always creates
//! an explicit merge commit referencing both parents for audit/lineage,
//! and its materialized view is defined purely by parent order:
//! `RecordOnly` puts `branch_a`'s head first (so it materializes as
//! `branch_a`'s view, with `branch_b` linked as an alternative), and
//! `PreferOther` puts `branch_b`'s head first instead. Both strategies are
//! "no silent merging" — the untaken branch's content never appears in the
//! merged view; it's only reachable by checking it out directly.

use crate::commit::{Author, Commit, CommitId, Delta, MergeStrategy, Metadata};
use crate::error::{GraphError, Result};
use crate::store::{CommitStore, RefStore};
use chrono::Utc;

/// Merges `branch_b` into `branch_a`: creates a merge commit referencing
/// both branches' current heads (order determined by `strategy`), advances
/// `branch_a` to point at it, and leaves `branch_b` untouched. Fails if
/// either branch doesn't exist, or if both heads are the same commit
/// (nothing to merge).
pub async fn merge<S: CommitStore + RefStore>(
    store: &S,
    branch_a: &str,
    branch_b: &str,
    strategy: MergeStrategy,
) -> Result<CommitId> {
    let a_head = store
        .get_branch(branch_a)
        .await?
        .ok_or_else(|| GraphError::BranchNotFound(branch_a.to_string()))?;
    let b_head = store
        .get_branch(branch_b)
        .await?
        .ok_or_else(|| GraphError::BranchNotFound(branch_b.to_string()))?;

    if a_head == b_head {
        return Err(GraphError::InvalidMergeParents);
    }

    let parent_ids = match strategy {
        MergeStrategy::RecordOnly => vec![a_head, b_head],
        MergeStrategy::PreferOther => vec![b_head, a_head],
    };

    let commit = Commit::new(
        parent_ids,
        Author::System,
        Delta::Merge { strategy },
        Metadata::new(Utc::now()),
    );
    let id = store.put(commit).await?;
    store.set_branch(branch_a, id).await?;
    Ok(id)
}
