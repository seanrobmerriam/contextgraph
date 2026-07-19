//! The commit model: the immutable, content-addressed unit of a contextgraph DAG.

use crate::error::{GraphError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// A BLAKE3 content hash identifying a commit. Two commits with byte-identical
/// `parent_ids` + `author` + `delta` + `metadata` always produce the same id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitId(pub [u8; 32]);

impl CommitId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl FromStr for CommitId {
    type Err = GraphError;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = hex::decode(s).map_err(|e| GraphError::InvalidCommitId(format!("{s}: {e}")))?;
        if bytes.len() != 32 {
            return Err(GraphError::InvalidCommitId(format!(
                "expected 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(CommitId(arr))
    }
}

impl TryFrom<String> for CommitId {
    type Error = GraphError;
    fn try_from(value: String) -> Result<Self> {
        CommitId::from_str(&value)
    }
}

impl From<CommitId> for String {
    fn from(value: CommitId) -> Self {
        value.to_hex()
    }
}

/// Minimal hex encode/decode so we don't need an extra dependency.
mod hex {
    pub fn encode(bytes: [u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[derive(Debug)]
    pub struct HexError(pub String);
    impl std::fmt::Display for HexError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "invalid hex: {}", self.0)
        }
    }

    pub fn decode(s: &str) -> std::result::Result<Vec<u8>, HexError> {
        if !s.len().is_multiple_of(2) {
            return Err(HexError(s.to_string()));
        }
        let mut out = Vec::with_capacity(s.len() / 2);
        let bytes = s.as_bytes();
        for chunk in bytes.chunks(2) {
            let hi = (chunk[0] as char)
                .to_digit(16)
                .ok_or_else(|| HexError(s.to_string()))?;
            let lo = (chunk[1] as char)
                .to_digit(16)
                .ok_or_else(|| HexError(s.to_string()))?;
            out.push(((hi << 4) | lo) as u8);
        }
        Ok(out)
    }
}

/// Who authored a commit's delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    User,
    Assistant,
    Tool,
    System,
}

impl fmt::Display for Author {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Author::User => "user",
            Author::Assistant => "assistant",
            Author::Tool => "tool",
            Author::System => "system",
        };
        write!(f, "{s}")
    }
}

/// The payload of a commit: what actually changed.
///
/// `Compaction` is a first-class variant, not a special case bolted onto
/// checkout: it replaces N prior commits' contribution to the materialized
/// window with a single condensed message, without deleting anything from
/// the DAG. `replaces` lists the commit ids (from the parent chain) whose
/// materialized turns are folded away by this op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Delta {
    Message {
        content: String,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        result: serde_json::Value,
    },
    Compaction {
        replaces: Vec<CommitId>,
        summary: String,
    },
    /// The delta of a merge commit itself: a pure audit/lineage marker, not
    /// new conversational content. Materializing a merge commit never
    /// surfaces this as a visible message — see `materialize` and `merge`.
    Merge {
        strategy: MergeStrategy,
    },
}

/// How a merge commit's materialized view is chosen between its two
/// parents. There is no automatic content merging of divergent
/// conversation turns — that's not well-defined for natural language, so
/// both strategies just pick which parent's view "wins" by controlling
/// parent order (`materialize` always follows the first parent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// The merge commit materializes as `branch_a`'s view; `branch_b` is
    /// linked as the second parent for audit/lineage only.
    RecordOnly,
    /// The merge commit materializes as `branch_b`'s view instead.
    PreferOther,
}

/// Token accounting for a commit, if the caller supplies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Caller-supplied, filterable metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub model_id: Option<String>,
    pub token_usage: Option<TokenUsage>,
    pub timestamp: DateTime<Utc>,
    /// Arbitrary key-value tags, e.g. `{"step": "planning"}`. A `BTreeMap` so
    /// that serialization (and therefore the content hash) is order-independent.
    pub tags: BTreeMap<String, String>,
}

impl Metadata {
    pub fn new(timestamp: DateTime<Utc>) -> Self {
        Self {
            model_id: None,
            token_usage: None,
            timestamp,
            tags: BTreeMap::new(),
        }
    }

    pub fn with_tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }
}

/// The canonical, hashed shape of a commit's content (everything except its
/// own id, which is derived from this).
#[derive(Serialize)]
struct CommitContent<'a> {
    parent_ids: &'a [CommitId],
    author: &'a Author,
    delta: &'a Delta,
    metadata: &'a Metadata,
}

/// Computes the content-addressed id for a would-be commit. Pure function:
/// identical inputs always yield the identical id.
pub fn compute_commit_id(
    parent_ids: &[CommitId],
    author: &Author,
    delta: &Delta,
    metadata: &Metadata,
) -> CommitId {
    let content = CommitContent {
        parent_ids,
        author,
        delta,
        metadata,
    };
    // Struct field order is fixed by declaration, and `tags` is a BTreeMap,
    // so this serialization is deterministic for identical content.
    let bytes = serde_json::to_vec(&content).expect("commit content is always serializable");
    CommitId(*blake3::hash(&bytes).as_bytes())
}

/// An immutable, content-addressed node in the context DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parent_ids: Vec<CommitId>,
    pub author: Author,
    pub delta: Delta,
    pub metadata: Metadata,
}

impl Commit {
    /// Builds a new commit, computing its content-addressed id.
    pub fn new(
        parent_ids: Vec<CommitId>,
        author: Author,
        delta: Delta,
        metadata: Metadata,
    ) -> Self {
        let id = compute_commit_id(&parent_ids, &author, &delta, &metadata);
        Self {
            id,
            parent_ids,
            author,
            delta,
            metadata,
        }
    }

    pub fn is_root(&self) -> bool {
        self.parent_ids.is_empty()
    }

    pub fn is_merge(&self) -> bool {
        self.parent_ids.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn identical_content_yields_identical_commit_id() {
        let delta = Delta::Message {
            content: "hello".into(),
        };
        let meta = Metadata::new(ts()).with_tag("step", "planning");
        let id1 = compute_commit_id(&[], &Author::User, &delta, &meta);
        let id2 = compute_commit_id(&[], &Author::User, &delta, &meta);
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_delta_content_yields_different_commit_id() {
        let meta = Metadata::new(ts());
        let id1 = compute_commit_id(
            &[],
            &Author::User,
            &Delta::Message {
                content: "a".into(),
            },
            &meta,
        );
        let id2 = compute_commit_id(
            &[],
            &Author::User,
            &Delta::Message {
                content: "b".into(),
            },
            &meta,
        );
        assert_ne!(id1, id2);
    }

    #[test]
    fn tag_insertion_order_does_not_affect_commit_id() {
        let delta = Delta::Message {
            content: "hi".into(),
        };
        let mut meta_a = Metadata::new(ts());
        meta_a.tags.insert("a".into(), "1".into());
        meta_a.tags.insert("b".into(), "2".into());

        let mut meta_b = Metadata::new(ts());
        meta_b.tags.insert("b".into(), "2".into());
        meta_b.tags.insert("a".into(), "1".into());

        let id_a = compute_commit_id(&[], &Author::User, &delta, &meta_a);
        let id_b = compute_commit_id(&[], &Author::User, &delta, &meta_b);
        assert_eq!(id_a, id_b);
    }

    #[test]
    fn different_parent_order_yields_different_commit_id() {
        let delta = Delta::Message {
            content: "merge".into(),
        };
        let meta = Metadata::new(ts());
        let p1 = CommitId([1u8; 32]);
        let p2 = CommitId([2u8; 32]);
        let id_ab = compute_commit_id(&[p1, p2], &Author::System, &delta, &meta);
        let id_ba = compute_commit_id(&[p2, p1], &Author::System, &delta, &meta);
        assert_ne!(
            id_ab, id_ba,
            "parent order is semantically significant for merges"
        );
    }

    #[test]
    fn commit_id_round_trips_through_hex_string() {
        let id = CommitId([42u8; 32]);
        let hex = id.to_hex();
        let parsed: CommitId = hex.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn commit_id_from_str_rejects_wrong_length() {
        let err = CommitId::from_str("abcd").unwrap_err();
        assert!(matches!(err, GraphError::InvalidCommitId(_)));
    }

    #[test]
    fn commit_id_from_str_rejects_non_hex_characters() {
        let bad = "zz".repeat(32);
        let err = CommitId::from_str(&bad).unwrap_err();
        assert!(matches!(err, GraphError::InvalidCommitId(_)));
    }

    #[test]
    fn root_commit_has_no_parents_and_is_recognized_as_root() {
        let commit = Commit::new(
            vec![],
            Author::System,
            Delta::Message {
                content: "init".into(),
            },
            Metadata::new(ts()),
        );
        assert!(commit.is_root());
        assert!(!commit.is_merge());
    }

    #[test]
    fn commit_with_two_parents_is_recognized_as_merge() {
        let commit = Commit::new(
            vec![CommitId([1; 32]), CommitId([2; 32])],
            Author::System,
            Delta::Message {
                content: "merge".into(),
            },
            Metadata::new(ts()),
        );
        assert!(commit.is_merge());
        assert!(!commit.is_root());
    }

    #[test]
    fn committing_identical_content_twice_is_idempotent_at_the_id_level() {
        let meta = Metadata::new(ts());
        let c1 = Commit::new(
            vec![],
            Author::User,
            Delta::Message {
                content: "x".into(),
            },
            meta.clone(),
        );
        let c2 = Commit::new(
            vec![],
            Author::User,
            Delta::Message {
                content: "x".into(),
            },
            meta,
        );
        assert_eq!(c1.id, c2.id);
    }

    #[test]
    fn empty_message_delta_is_representable_and_hashes_consistently() {
        let meta = Metadata::new(ts());
        let delta = Delta::Message {
            content: String::new(),
        };
        let id1 = compute_commit_id(&[], &Author::User, &delta, &meta);
        let id2 = compute_commit_id(&[], &Author::User, &delta, &meta);
        assert_eq!(id1, id2);
    }
}
