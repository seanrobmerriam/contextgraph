
//! Property test: for arbitrary chain lengths and arbitrary (constructed,
//! known-by-construction) flip points, `bisect` always finds the exact
//! boundary, using only O(log n) predicate calls.

use chrono::Utc;
use contextgraph_core::bisect::{bisect, BisectOutcome};
use contextgraph_core::commit::{Author, Commit, CommitId, Delta, Metadata};
use contextgraph_core::mem::InMemoryCommitStore;
use contextgraph_core::store::CommitStore;
use contextgraph_core::MaterializedContext;
use proptest::prelude::*;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn index_of(ctx: &MaterializedContext) -> usize {
    let last = ctx.messages.last().unwrap();
    match &last.delta {
        Delta::Message { content } => content.strip_prefix("turn-").unwrap().parse().unwrap(),
        _ => unreachable!(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn bisect_finds_the_true_flip_point_for_arbitrary_chain_lengths_and_flip_positions(
        n in 2usize..80,
        flip_frac in 0.0f64..1.0,
    ) {
        let runtime = rt();
        runtime.block_on(async {
            let store = InMemoryCommitStore::new();
            let mut ids = Vec::new();
            let mut parent = None;
            for i in 0..n {
                let commit = Commit::new(
                    parent.into_iter().collect::<Vec<CommitId>>(),
                    Author::User,
                    Delta::Message { content: format!("turn-{i}") },
                    Metadata::new(Utc::now()),
                );
                let id = store.put(commit).await.unwrap();
                ids.push(id);
                parent = Some(id);
            }

            // flip_at in [1, n-1]: index of the first "bad" commit.
            let flip_at = 1 + (flip_frac * (n - 1) as f64) as usize;
            let flip_at = flip_at.clamp(1, n - 1);

            let good = ids[0];
            let bad = *ids.last().unwrap();
            let mut calls = 0usize;
            let outcome = bisect(&store, good, bad, |ctx| {
                calls += 1;
                index_of(ctx) < flip_at
            }).await.unwrap();

            match outcome {
                BisectOutcome::Flip(r) => {
                    prop_assert_eq!(r.last_good, ids[flip_at - 1]);
                    prop_assert_eq!(r.first_bad, ids[flip_at]);
                }
                BisectOutcome::NoFlip => prop_assert!(false, "expected a flip at {flip_at} for n={n}"),
            }

            let max_expected = (n as f64).log2().ceil() as usize + 2;
            prop_assert!(calls <= max_expected, "n={n} flip_at={flip_at} used {calls} calls, expected <= {max_expected}");
            Ok(())
        })?;
    }
}
