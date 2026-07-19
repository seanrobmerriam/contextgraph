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

/// Removes every commit not reachable from any branch, returning the ids
/// that were removed. A commit dropped from every branch (e.g. via
/// `rollback`) but still hash-reachable from nowhere else becomes eligible
/// here — until `gc` runs, it stays in storage.
pub async fn gc<S: CommitStore + RefStore + ?Sized>(store: &S) -> Result<Vec<CommitId>> {
    let reachable = reachable_from_branches(store).await?;
    let all = store.all_ids().await?;
    let unreachable: Vec<CommitId> = all
        .into_iter()
        .filter(|id| !reachable.contains(id))
        .collect();
    store.remove_many(&unreachable).await?;
    Ok(unreachable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{Author, Commit, Delta, Metadata};
    use crate::mem::InMemoryGraphStore;
    use crate::store::{CommitStore, RefStore};
    use chrono::Utc;

    fn msg(parents: Vec<CommitId>, text: &str) -> Commit {
        Commit::new(
            parents,
            Author::User,
            Delta::Message {
                content: text.to_string(),
            },
            Metadata::new(Utc::now()),
        )
    }

    #[tokio::test]
    async fn gc_removes_commits_unreachable_from_every_branch() {
        let store = InMemoryGraphStore::new();
        let root = msg(vec![], "root");
        store.put(root.clone()).await.unwrap();
        store.set_branch("main", root.id).await.unwrap();

        // A commit built on root but never referenced by any branch.
        let orphan = msg(vec![root.id], "orphan");
        store.put(orphan.clone()).await.unwrap();

        let removed = gc(&store).await.unwrap();
        assert_eq!(removed, vec![orphan.id]);
        assert!(!store.contains(&orphan.id).await.unwrap());
        assert!(store.contains(&root.id).await.unwrap());
    }

    #[tokio::test]
    async fn gc_never_removes_commits_reachable_from_any_branch() {
        let store = InMemoryGraphStore::new();
        let root = msg(vec![], "root");
        store.put(root.clone()).await.unwrap();
        store.set_branch("main", root.id).await.unwrap();
        let child = msg(vec![root.id], "child");
        store.put(child.clone()).await.unwrap();
        store.set_branch("feature", child.id).await.unwrap();

        let removed = gc(&store).await.unwrap();
        assert!(removed.is_empty());
        assert!(store.contains(&root.id).await.unwrap());
        assert!(store.contains(&child.id).await.unwrap());
    }

    #[tokio::test]
    async fn gc_keeps_both_parents_of_a_reachable_merge_commit() {
        let store = InMemoryGraphStore::new();
        let root = msg(vec![], "root");
        store.put(root.clone()).await.unwrap();
        let a = msg(vec![root.id], "a");
        store.put(a.clone()).await.unwrap();
        let b = msg(vec![root.id], "b");
        store.put(b.clone()).await.unwrap();
        // A "merge" commit with two parents, reachable only via `main`.
        let merged = Commit::new(
            vec![a.id, b.id],
            Author::System,
            Delta::Message {
                content: "merge".into(),
            },
            Metadata::new(Utc::now()),
        );
        store.put(merged.clone()).await.unwrap();
        store.set_branch("main", merged.id).await.unwrap();

        let removed = gc(&store).await.unwrap();
        assert!(removed.is_empty());
        assert!(store.contains(&a.id).await.unwrap());
        assert!(store.contains(&b.id).await.unwrap());
    }

    #[tokio::test]
    async fn gc_with_no_branches_removes_everything() {
        let store = InMemoryGraphStore::new();
        let root = msg(vec![], "root");
        store.put(root.clone()).await.unwrap();

        let removed = gc(&store).await.unwrap();
        assert_eq!(removed, vec![root.id]);
        assert_eq!(store.len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn gc_is_a_no_op_on_an_empty_store() {
        let store = InMemoryGraphStore::new();
        let removed = gc(&store).await.unwrap();
        assert!(removed.is_empty());
    }
}
