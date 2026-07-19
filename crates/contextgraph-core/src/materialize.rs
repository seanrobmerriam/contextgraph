//! Materialization: turning a DAG walk into the ordered message list a model
//! would actually see, folding compaction ops as one more delta type rather
//! than a special case bolted onto the walk.

use crate::commit::{Author, CommitId, Delta};
use crate::error::{GraphError, Result};
use crate::store::CommitStore;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// One turn in a materialized context: the delta contributed by a single
/// commit, tagged with the commit that contributed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{Author, Commit, Metadata};
    use crate::mem::InMemoryCommitStore;
    use chrono::Utc;

    fn msg(parents: Vec<CommitId>, author: Author, text: &str) -> Commit {
        Commit::new(
            parents,
            author,
            Delta::Message {
                content: text.to_string(),
            },
            Metadata::new(Utc::now()),
        )
    }

    #[tokio::test]
    async fn materializing_a_single_root_commit_yields_one_message() {
        let store = InMemoryCommitStore::new();
        let root = msg(vec![], Author::System, "root");
        store.put(root.clone()).await.unwrap();

        let ctx = materialize(&store, root.id).await.unwrap();
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.manifest, vec![root.id]);
        assert_eq!(ctx.messages[0].delta, root.delta);
    }

    #[tokio::test]
    async fn materializing_a_chain_preserves_root_to_head_order() {
        let store = InMemoryCommitStore::new();
        let root = msg(vec![], Author::User, "one");
        store.put(root.clone()).await.unwrap();
        let second = msg(vec![root.id], Author::Assistant, "two");
        store.put(second.clone()).await.unwrap();
        let third = msg(vec![second.id], Author::User, "three");
        store.put(third.clone()).await.unwrap();

        let ctx = materialize(&store, third.id).await.unwrap();
        let texts: Vec<String> = ctx
            .messages
            .iter()
            .map(|m| match &m.delta {
                Delta::Message { content } => content.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
        assert_eq!(ctx.manifest, vec![root.id, second.id, third.id]);
    }

    #[tokio::test]
    async fn materializing_nonexistent_commit_fails() {
        let store = InMemoryCommitStore::new();
        let err = materialize(&store, CommitId::from_bytes([1; 32])).await.unwrap_err();
        assert!(matches!(err, GraphError::CommitNotFound(_)));
    }

    fn compaction(parents: Vec<CommitId>, replaces: Vec<CommitId>, summary: &str) -> Commit {
        Commit::new(
            parents,
            Author::System,
            Delta::Compaction {
                replaces,
                summary: summary.to_string(),
            },
            Metadata::new(Utc::now()),
        )
    }

    #[tokio::test]
    async fn compaction_replaces_prior_turns_with_a_single_condensed_message() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], Author::User, "one");
        store.put(a.clone()).await.unwrap();
        let b = msg(vec![a.id], Author::Assistant, "two");
        store.put(b.clone()).await.unwrap();
        let c = msg(vec![b.id], Author::User, "three");
        store.put(c.clone()).await.unwrap();

        let condensed = compaction(vec![c.id], vec![a.id, b.id], "one and two, condensed");
        store.put(condensed.clone()).await.unwrap();

        let ctx = materialize(&store, condensed.id).await.unwrap();
        // a and b's turns are folded away; only the compaction + c remain.
        assert_eq!(ctx.manifest, vec![c.id, condensed.id]);
        assert_eq!(ctx.messages.len(), 2);
        assert!(matches!(ctx.messages[1].delta, Delta::Compaction { .. }));
    }

    #[tokio::test]
    async fn compaction_does_not_delete_replaced_commits_from_the_store() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], Author::User, "one");
        store.put(a.clone()).await.unwrap();
        let condensed = compaction(vec![a.id], vec![a.id], "condensed");
        store.put(condensed.clone()).await.unwrap();

        materialize(&store, condensed.id).await.unwrap();
        // History is never lost: `a` is still fetchable by hash even though
        // it no longer contributes to this projection.
        assert!(store.get(&a.id).await.unwrap().is_some());
        assert_eq!(store.len().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn compaction_referencing_an_id_not_in_the_current_window_is_a_harmless_noop() {
        // A compaction whose `replaces` list includes a commit id that's
        // already outside the materialized window (e.g. folded away by an
        // earlier compaction, or simply never in this chain) must not error
        // or double-remove anything.
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], Author::User, "one");
        store.put(a.clone()).await.unwrap();
        let stray = CommitId::from_bytes([77; 32]);
        let condensed = compaction(vec![a.id], vec![stray], "condensed");
        store.put(condensed.clone()).await.unwrap();

        let ctx = materialize(&store, condensed.id).await.unwrap();
        assert_eq!(ctx.manifest, vec![a.id, condensed.id]);
    }

    #[tokio::test]
    async fn double_compaction_folds_an_already_compacted_region_cleanly() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], Author::User, "one");
        store.put(a.clone()).await.unwrap();
        let b = msg(vec![a.id], Author::Assistant, "two");
        store.put(b.clone()).await.unwrap();
        let first = compaction(vec![b.id], vec![a.id], "a condensed");
        store.put(first.clone()).await.unwrap();
        let c = msg(vec![first.id], Author::User, "three");
        store.put(c.clone()).await.unwrap();
        // Second compaction folds away both the earlier compaction commit
        // and `b`, replacing the whole prefix with one summary.
        let second = compaction(vec![c.id], vec![b.id, first.id], "everything condensed");
        store.put(second.clone()).await.unwrap();

        let ctx = materialize(&store, second.id).await.unwrap();
        assert_eq!(ctx.manifest, vec![c.id, second.id]);
    }

    #[tokio::test]
    async fn materializing_through_compaction_is_deterministic_and_idempotent() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], Author::User, "one");
        store.put(a.clone()).await.unwrap();
        let b = msg(vec![a.id], Author::Assistant, "two");
        store.put(b.clone()).await.unwrap();
        let condensed = compaction(vec![b.id], vec![a.id], "condensed");
        store.put(condensed.clone()).await.unwrap();

        let first = materialize(&store, condensed.id).await.unwrap();
        let second = materialize(&store, condensed.id).await.unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn compaction_with_empty_replaces_list_is_just_an_added_message() {
        let store = InMemoryCommitStore::new();
        let a = msg(vec![], Author::User, "one");
        store.put(a.clone()).await.unwrap();
        let condensed = compaction(vec![a.id], vec![], "note");
        store.put(condensed.clone()).await.unwrap();

        let ctx = materialize(&store, condensed.id).await.unwrap();
        assert_eq!(ctx.manifest, vec![a.id, condensed.id]);
        assert_eq!(ctx.messages.len(), 2);
    }
}
