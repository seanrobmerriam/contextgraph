//! Property tests for compaction-op folding: for arbitrary chains with a
//! compaction op folding away a random subset of the prior manifest,
//! materialization is deterministic (repeated checkouts agree) and the
//! folding itself is idempotent (replaying it never double-removes or
//! errors, even when `replaces` names ids no longer in the window).

use chrono::Utc;
use contextgraph_core::commit::{Author, Commit, CommitId, Delta, Metadata};
use contextgraph_core::materialize::materialize;
use contextgraph_core::mem::InMemoryCommitStore;
use contextgraph_core::store::CommitStore;
use proptest::prelude::*;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn plain(parents: Vec<CommitId>, text: &str) -> Commit {
    Commit::new(
        parents,
        Author::User,
        Delta::Message {
            content: text.to_string(),
        },
        Metadata::new(Utc::now()),
    )
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]

    #[test]
    fn compaction_folding_is_deterministic_over_arbitrary_prefixes(
        texts in prop::collection::vec("[a-z]{1,8}", 1..12),
        // bitmask deciding which prefix commits the compaction replaces
        replace_mask in prop::collection::vec(any::<bool>(), 1..12),
        tail in "[a-z]{1,8}",
    ) {
        let runtime = rt();
        runtime.block_on(async {
            let store = InMemoryCommitStore::new();
            let mut ids = Vec::new();
            let mut parent = None;
            for t in &texts {
                let c = plain(parent.into_iter().collect(), t);
                let id = store.put(c).await.unwrap();
                ids.push(id);
                parent = Some(id);
            }

            let replaces: Vec<CommitId> = ids
                .iter()
                .zip(replace_mask.iter().cycle())
                .filter_map(|(id, &keep)| if keep { Some(*id) } else { None })
                .collect();

            let condensed = compaction(parent.into_iter().collect(), replaces.clone(), &tail);
            let condensed_id = store.put(condensed).await.unwrap();

            let first = materialize(&store, condensed_id).await.unwrap();
            let second = materialize(&store, condensed_id).await.unwrap();
            prop_assert_eq!(&first, &second, "materialization must be deterministic");

            // Every replaced id is gone from the manifest...
            for r in &replaces {
                prop_assert!(!first.manifest.contains(r));
            }
            // ...but still present in the store (history is never lost).
            for r in &replaces {
                prop_assert!(store.contains(r).await.unwrap());
            }
            // The compaction commit itself always contributes.
            prop_assert!(first.manifest.contains(&condensed_id));
            Ok(())
        })?;
    }

    #[test]
    fn compaction_replacing_ids_outside_the_window_never_panics_or_errors(
        texts in prop::collection::vec("[a-z]{1,8}", 1..8),
        stray_bytes in any::<[u8; 32]>(),
    ) {
        let runtime = rt();
        runtime.block_on(async {
            let store = InMemoryCommitStore::new();
            let mut parent = None;
            for t in &texts {
                let c = plain(parent.into_iter().collect(), t);
                let id = store.put(c).await.unwrap();
                parent = Some(id);
            }
            let stray = CommitId::from_bytes(stray_bytes);
            let condensed = compaction(parent.into_iter().collect(), vec![stray], "note");
            let condensed_id = store.put(condensed).await.unwrap();

            let ctx = materialize(&store, condensed_id).await.unwrap();
            prop_assert!(ctx.manifest.contains(&condensed_id));
            Ok(())
        })?;
    }
}
