//! Property tests for invariants 1, 2, and 4 from the spec:
//! 1. Commit ids are pure functions of content (dedupe across paths).
//! 2. The DAG is acyclic; every non-root commit has an existing parent.
//! 4. Branch moves are the only mutable state; commits are never mutated.
//!
//! (Invariant 3 — checkout determinism — is exercised once `checkout` lands
//! in the M2 engine; invariant 5 once `merge` lands in M6.)

use chrono::Utc;
use contextgraph_core::commit::{Author, Commit, Delta, Metadata};
use contextgraph_core::mem::{InMemoryCommitStore, InMemoryRefStore};
use contextgraph_core::store::{CommitStore, RefStore};
use contextgraph_core::CommitId;
use proptest::prelude::*;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn make_commit(parents: Vec<CommitId>, text: &str) -> Commit {
    Commit::new(
        parents,
        Author::User,
        Delta::Message {
            content: text.to_string(),
        },
        Metadata::new(Utc::now()),
    )
}

#[derive(Debug, Clone)]
enum Op {
    NewRoot(String),
    NewChild(usize, String),
    Duplicate(usize),
}

fn op_strategy(max_index: usize) -> impl Strategy<Value = Op> {
    if max_index == 0 {
        "[a-z]{1,8}".prop_map(Op::NewRoot).boxed()
    } else {
        prop_oneof![
            "[a-z]{1,8}".prop_map(Op::NewRoot),
            (0..max_index, "[a-z]{1,8}").prop_map(|(i, s)| Op::NewChild(i, s)),
            (0..max_index).prop_map(Op::Duplicate),
        ]
        .boxed()
    }
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    // Each op at position `i` may only reference indices `0..i`, since that's
    // all that will have been `created` by the time it executes.
    (1usize..15).prop_flat_map(|len| (0..len).map(op_strategy).collect::<Vec<_>>())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn arbitrary_commit_sequences_preserve_content_addressing_and_acyclicity(ops in ops_strategy()) {
        let runtime = rt();
        runtime.block_on(async {
            let store = InMemoryCommitStore::new();
            let mut created: Vec<CommitId> = Vec::new();
            let mut distinct_contents: std::collections::HashSet<CommitId> = std::collections::HashSet::new();

            for op in ops {
                match op {
                    Op::NewRoot(text) => {
                        let c = make_commit(vec![], &format!("root:{text}"));
                        let id = store.put(c).await.unwrap();
                        distinct_contents.insert(id);
                        created.push(id);
                    }
                    Op::NewChild(idx, text) => {
                        let parent = created[idx % created.len().max(1)];
                        let c = make_commit(vec![parent], &text);
                        let id = store.put(c).await.unwrap();
                        distinct_contents.insert(id);
                        created.push(id);

                        // Invariant 2: parent must already exist (it does, since
                        // we only reference earlier `created` entries).
                        prop_assert!(store.contains(&parent).await.unwrap());
                    }
                    Op::Duplicate(idx) => {
                        if created.is_empty() { continue; }
                        let target = created[idx % created.len()];
                        let original = store.get(&target).await.unwrap().unwrap();
                        // Re-putting byte-identical content must yield the same id
                        // and must not create a new storage entry (invariant 1).
                        let before_len = store.len().await.unwrap();
                        let id_again = store.put(original).await.unwrap();
                        let after_len = store.len().await.unwrap();
                        prop_assert_eq!(id_again, target);
                        prop_assert_eq!(before_len, after_len);
                        created.push(id_again);
                    }
                }
            }

            // Invariant 1: total distinct storage entries equals distinct content ids.
            prop_assert_eq!(store.len().await.unwrap(), distinct_contents.len());

            // Invariant 2: every stored commit's parents are present in the store,
            // and every parent is either the same commit's ancestor path (never
            // itself), i.e. no self-loops / dangling refs.
            for id in store.all_ids().await.unwrap() {
                let c = store.get(&id).await.unwrap().unwrap();
                for p in &c.parent_ids {
                    prop_assert!(store.contains(p).await.unwrap());
                    prop_assert_ne!(*p, id, "a commit can never be its own parent");
                }
            }
            Ok(())
        })?;
    }

    #[test]
    fn commits_are_never_mutated_by_branch_pointer_moves(text in "[a-z]{1,8}", other_text in "[a-z]{1,8}") {
        let runtime = rt();
        runtime.block_on(async {
            let store = InMemoryCommitStore::new();
            let refs = InMemoryRefStore::new();

            let root = make_commit(vec![], &text);
            let root_id = store.put(root.clone()).await.unwrap();
            let child = make_commit(vec![root_id], &other_text);
            let child_id = store.put(child).await.unwrap();

            refs.set_branch("main", root_id).await.unwrap();
            refs.set_branch("main", child_id).await.unwrap();
            refs.set_branch("main", root_id).await.unwrap();

            // The commit itself never changes regardless of how many times
            // branch pointers move across it (invariant 4).
            let fetched = store.get(&root_id).await.unwrap().unwrap();
            prop_assert_eq!(fetched, root);
            Ok(())
        })?;
    }
}
