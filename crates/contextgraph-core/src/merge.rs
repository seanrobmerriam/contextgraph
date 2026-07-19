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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::Delta as D;
    use crate::materialize::materialize;
    use crate::mem::InMemoryGraphStore;
    use crate::store::CommitStore;

    fn text(s: &str) -> Delta {
        D::Message {
            content: s.to_string(),
        }
    }

    async fn setup_diverged(store: &InMemoryGraphStore) -> (CommitId, CommitId, CommitId) {
        let root = Commit::new(
            vec![],
            Author::User,
            text("root"),
            Metadata::new(Utc::now()),
        );
        let root_id = store.put(root).await.unwrap();
        store.set_branch("a", root_id).await.unwrap();
        store.set_branch("b", root_id).await.unwrap();

        let a_turn = Commit::new(
            vec![root_id],
            Author::Assistant,
            text("a-turn"),
            Metadata::new(Utc::now()),
        );
        let a_id = store.put(a_turn).await.unwrap();
        store.set_branch("a", a_id).await.unwrap();

        let b_turn = Commit::new(
            vec![root_id],
            Author::Assistant,
            text("b-turn"),
            Metadata::new(Utc::now()),
        );
        let b_id = store.put(b_turn).await.unwrap();
        store.set_branch("b", b_id).await.unwrap();

        (root_id, a_id, b_id)
    }

    #[tokio::test]
    async fn record_only_merge_materializes_as_branch_a_view() {
        let store = InMemoryGraphStore::new();
        let (root_id, a_id, b_id) = setup_diverged(&store).await;

        let merge_id = merge(&store, "a", "b", MergeStrategy::RecordOnly)
            .await
            .unwrap();

        let ctx = materialize(&store, merge_id).await.unwrap();
        // Branch a's content ("a-turn") is present; branch b's ("b-turn") never is.
        let texts: Vec<String> = ctx
            .messages
            .iter()
            .filter_map(|m| match &m.delta {
                D::Message { content } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["root", "a-turn"]);

        // But the manifest records the merge commit's lineage, including
        // that b's head is linked as the alternative parent.
        assert_eq!(ctx.manifest, vec![root_id, a_id, merge_id]);
        let merge_commit = store.get(&merge_id).await.unwrap().unwrap();
        assert_eq!(merge_commit.parent_ids, vec![a_id, b_id]);
    }

    #[tokio::test]
    async fn prefer_other_merge_materializes_as_branch_b_view() {
        let store = InMemoryGraphStore::new();
        let (_, _, b_id) = setup_diverged(&store).await;

        let merge_id = merge(&store, "a", "b", MergeStrategy::PreferOther)
            .await
            .unwrap();

        let ctx = materialize(&store, merge_id).await.unwrap();
        let texts: Vec<String> = ctx
            .messages
            .iter()
            .filter_map(|m| match &m.delta {
                D::Message { content } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["root", "b-turn"]);

        let merge_commit = store.get(&merge_id).await.unwrap().unwrap();
        assert_eq!(
            merge_commit.parent_ids,
            vec![b_id, merge_commit.parent_ids[1]]
        );
    }

    #[tokio::test]
    async fn merge_advances_branch_a_but_leaves_branch_b_untouched() {
        let store = InMemoryGraphStore::new();
        let (_, a_id, b_id) = setup_diverged(&store).await;

        let merge_id = merge(&store, "a", "b", MergeStrategy::RecordOnly)
            .await
            .unwrap();

        assert_eq!(store.get_branch("a").await.unwrap(), Some(merge_id));
        assert_eq!(store.get_branch("b").await.unwrap(), Some(b_id));
        // Both original heads remain independently checkoutable.
        assert!(store.contains(&a_id).await.unwrap());
        assert!(store.contains(&b_id).await.unwrap());
    }

    #[tokio::test]
    async fn merging_branches_with_no_common_ancestor_still_succeeds() {
        let store = InMemoryGraphStore::new();
        let a_root = Commit::new(
            vec![],
            Author::User,
            text("a-root"),
            Metadata::new(Utc::now()),
        );
        let a_id = store.put(a_root).await.unwrap();
        store.set_branch("a", a_id).await.unwrap();

        let b_root = Commit::new(
            vec![],
            Author::User,
            text("b-root"),
            Metadata::new(Utc::now()),
        );
        let b_id = store.put(b_root).await.unwrap();
        store.set_branch("b", b_id).await.unwrap();

        let merge_id = merge(&store, "a", "b", MergeStrategy::RecordOnly)
            .await
            .unwrap();
        let ctx = materialize(&store, merge_id).await.unwrap();
        assert_eq!(ctx.messages.len(), 1); // just a-root; no shared history, no error
    }

    #[tokio::test]
    async fn merging_a_branch_with_itself_fails() {
        let store = InMemoryGraphStore::new();
        let root = Commit::new(
            vec![],
            Author::User,
            text("root"),
            Metadata::new(Utc::now()),
        );
        let id = store.put(root).await.unwrap();
        store.set_branch("a", id).await.unwrap();
        store.set_branch("b", id).await.unwrap();

        let err = merge(&store, "a", "b", MergeStrategy::RecordOnly)
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::InvalidMergeParents));
    }

    #[tokio::test]
    async fn merging_a_nonexistent_branch_fails() {
        let store = InMemoryGraphStore::new();
        let root = Commit::new(
            vec![],
            Author::User,
            text("root"),
            Metadata::new(Utc::now()),
        );
        let id = store.put(root).await.unwrap();
        store.set_branch("a", id).await.unwrap();

        let err = merge(&store, "a", "nope", MergeStrategy::RecordOnly)
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::BranchNotFound(_)));
    }
}
