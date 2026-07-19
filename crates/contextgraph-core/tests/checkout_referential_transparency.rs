
//! Property tests: checkout is deterministic and referentially transparent
//! under arbitrary DAG shapes (invariant 3), and content-addressing dedupes
//! identical commits reached via different branch paths.

use chrono::Utc;
use contextgraph_core::commit::{Author, Delta, Metadata};
use contextgraph_core::graph::{CheckoutTarget, ContextGraph};
use contextgraph_core::mem::InMemoryGraphStore;
use proptest::prelude::*;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn meta() -> Metadata {
    Metadata::new(Utc::now())
}

fn text(s: &str) -> Delta {
    Delta::Message {
        content: s.to_string(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]

    /// For any chain of committed messages, checking out the head twice
    /// yields byte-identical (via `PartialEq`) materialized contexts, and
    /// the message order always matches commit order root-to-head.
    #[test]
    fn checkout_is_referentially_transparent_over_arbitrary_chains(
        texts in prop::collection::vec("[a-z]{1,10}", 1..25)
    ) {
        let runtime = rt();
        runtime.block_on(async {
            let g = ContextGraph::new(InMemoryGraphStore::new());
            let mut parent = None;
            let mut expected_order = Vec::new();
            for t in &texts {
                let id = g.commit(parent, Author::User, text(t), meta()).await.unwrap();
                parent = Some(id);
                expected_order.push(t.clone());
            }
            let head = parent.unwrap();

            let a = g.checkout(CheckoutTarget::commit(head)).await.unwrap();
            let b = g.checkout(CheckoutTarget::commit(head)).await.unwrap();
            prop_assert_eq!(&a, &b);

            let actual_order: Vec<String> = a.messages.iter().map(|m| match &m.delta {
                Delta::Message { content } => content.clone(),
                _ => unreachable!(),
            }).collect();
            prop_assert_eq!(actual_order, expected_order);
            Ok(())
        })?;
    }

    /// Two branches that fork from the same commit and then commit
    /// byte-identical content converge on the same commit id, and checking
    /// out either branch head yields the same materialized context
    /// (storage dedupe across fork paths).
    #[test]
    fn forked_branches_committing_identical_content_dedupe_and_checkout_identically(
        shared in "[a-z]{1,10}", divergent in "[a-z]{1,10}"
    ) {
        let runtime = rt();
        runtime.block_on(async {
            let g = ContextGraph::new(InMemoryGraphStore::new());
            let root = g.commit(None, Author::User, text(&shared), meta()).await.unwrap();
            g.branch("a", root).await.unwrap();
            g.branch("b", root).await.unwrap();

            let shared_meta = meta();
            let id_a = g.commit(Some(root), Author::Assistant, text(&divergent), shared_meta.clone()).await.unwrap();
            let id_b = g.commit(Some(root), Author::Assistant, text(&divergent), shared_meta).await.unwrap();
            prop_assert_eq!(id_a, id_b);

            g.move_branch("a", id_a).await.unwrap();
            g.move_branch("b", id_b).await.unwrap();

            let ctx_a = g.checkout(CheckoutTarget::branch("a")).await.unwrap();
            let ctx_b = g.checkout(CheckoutTarget::branch("b")).await.unwrap();
            prop_assert_eq!(ctx_a, ctx_b);
            Ok(())
        })?;
    }
}
