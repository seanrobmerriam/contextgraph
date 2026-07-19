//! History walks: `log` over a commit's ancestry (or a branch's full
//! history, which is just the ancestry of its head), with metadata-tag
//! filtering and pagination.

use crate::commit::{Commit, CommitId};
use crate::error::{GraphError, Result};
use crate::store::CommitStore;
use serde::Serialize;
use std::collections::BTreeMap;

/// Filters commits by exact tag match. A commit must carry every listed
/// key/value pair (in `metadata.tags`) to pass. An empty filter matches
/// everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    pub tags: BTreeMap<String, String>,
}

impl LogFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }

    fn matches(&self, commit: &Commit) -> bool {
        self.tags
            .iter()
            .all(|(k, v)| commit.metadata.tags.get(k) == Some(v))
    }
}

/// A page of `log` results, newest (closest to the requested head) first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogPage {
    pub commits: Vec<Commit>,
    /// Total commits matching the filter, before pagination.
    pub total_matched: usize,
    pub has_more: bool,
}

/// Walks the first-parent ancestry of `from` back to the root, newest
/// (i.e. `from`) first, applying `filter` and then `offset`/`limit`
/// pagination. This is a plain linear walk — `bisect` (milestone 7) is the
/// log-n counterpart for narrowing to a single flip point, but `log` itself
/// always has to visit every candidate commit to filter and count them.
pub async fn log_ancestors<S: CommitStore + ?Sized>(
    store: &S,
    from: CommitId,
    filter: &LogFilter,
    offset: usize,
    limit: usize,
) -> Result<LogPage> {
    let mut chain = Vec::new();
    let mut current = Some(from);
    while let Some(id) = current {
        let commit = store
            .get(&id)
            .await?
            .ok_or(GraphError::CommitNotFound(id))?;
        current = commit.parent_ids.first().copied();
        chain.push(commit);
    }

    let filtered: Vec<Commit> = chain.into_iter().filter(|c| filter.matches(c)).collect();
    let total_matched = filtered.len();
    let page: Vec<Commit> = filtered.into_iter().skip(offset).take(limit).collect();
    let has_more = offset.saturating_add(page.len()) < total_matched;

    Ok(LogPage {
        commits: page,
        total_matched,
        has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{Author, Commit, Delta, Metadata};
    use crate::mem::InMemoryCommitStore;
    use chrono::Utc;

    async fn build_chain(
        store: &InMemoryCommitStore,
        n: usize,
        tag_every: Option<&str>,
    ) -> Vec<CommitId> {
        let mut ids = Vec::new();
        let mut parent = None;
        for i in 0..n {
            let mut meta = Metadata::new(Utc::now());
            if let Some(tag_value) = tag_every {
                if i % 2 == 0 {
                    meta = meta.with_tag("step", tag_value);
                }
            }
            let commit = Commit::new(
                parent.into_iter().collect(),
                Author::User,
                Delta::Message {
                    content: format!("msg-{i}"),
                },
                meta,
            );
            let id = store.put(commit).await.unwrap();
            ids.push(id);
            parent = Some(id);
        }
        ids
    }

    #[tokio::test]
    async fn log_over_a_chain_returns_newest_first() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 3, None).await;
        let page = log_ancestors(&store, *ids.last().unwrap(), &LogFilter::new(), 0, 10)
            .await
            .unwrap();
        let returned: Vec<CommitId> = page.commits.iter().map(|c| c.id).collect();
        assert_eq!(returned, vec![ids[2], ids[1], ids[0]]);
        assert_eq!(page.total_matched, 3);
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn log_pagination_returns_correct_page_and_has_more_flag() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 5, None).await;
        let page = log_ancestors(&store, *ids.last().unwrap(), &LogFilter::new(), 1, 2)
            .await
            .unwrap();
        let returned: Vec<CommitId> = page.commits.iter().map(|c| c.id).collect();
        assert_eq!(returned, vec![ids[3], ids[2]]);
        assert_eq!(page.total_matched, 5);
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn log_pagination_offset_beyond_range_yields_empty_page_without_more() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 3, None).await;
        let page = log_ancestors(&store, *ids.last().unwrap(), &LogFilter::new(), 10, 5)
            .await
            .unwrap();
        assert!(page.commits.is_empty());
        assert!(!page.has_more);
    }

    #[tokio::test]
    async fn log_with_tag_filter_returns_only_matching_commits_at_the_exact_boundary() {
        let store = InMemoryCommitStore::new();
        // commits 0, 2, 4 get tag step=planning; 1, 3 get none.
        let ids = build_chain(&store, 5, Some("planning")).await;
        let filter = LogFilter::new().with_tag("step", "planning");
        let page = log_ancestors(&store, *ids.last().unwrap(), &filter, 0, 10)
            .await
            .unwrap();
        let returned: Vec<CommitId> = page.commits.iter().map(|c| c.id).collect();
        assert_eq!(returned, vec![ids[4], ids[2], ids[0]]);
        assert_eq!(page.total_matched, 3);
    }

    #[tokio::test]
    async fn log_with_tag_filter_that_matches_nothing_returns_empty_page() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 3, Some("planning")).await;
        let filter = LogFilter::new().with_tag("step", "execution");
        let page = log_ancestors(&store, *ids.last().unwrap(), &filter, 0, 10)
            .await
            .unwrap();
        assert!(page.commits.is_empty());
        assert_eq!(page.total_matched, 0);
    }

    #[tokio::test]
    async fn log_over_single_commit_range_does_not_panic() {
        let store = InMemoryCommitStore::new();
        let ids = build_chain(&store, 1, None).await;
        let page = log_ancestors(&store, ids[0], &LogFilter::new(), 0, 10)
            .await
            .unwrap();
        assert_eq!(page.commits.len(), 1);
    }

    #[tokio::test]
    async fn log_from_nonexistent_commit_fails() {
        let store = InMemoryCommitStore::new();
        let err = log_ancestors(&store, CommitId([9; 32]), &LogFilter::new(), 0, 10)
            .await
            .unwrap_err();
        assert!(matches!(err, GraphError::CommitNotFound(_)));
    }
}
