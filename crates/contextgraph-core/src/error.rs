use crate::commit::CommitId;
use thiserror::Error;

/// All errors surfaced by contextgraph-core.
///
/// Every fallible operation in this crate returns `Result<_, GraphError>` on
/// every path; there is no `unwrap`/`expect` outside tests.
#[derive(Debug, Error)]
pub enum GraphError {
    #[error("parent commit not found: {0}")]
    ParentNotFound(CommitId),

    #[error("commit not found: {0}")]
    CommitNotFound(CommitId),

    #[error("branch not found: {0}")]
    BranchNotFound(String),

    #[error("branch already exists: {0}")]
    BranchAlreadyExists(String),

    #[error("merge parents must be two distinct, existing commits")]
    InvalidMergeParents,

    #[error("no common ancestor between {0} and {1}")]
    NoCommonAncestor(CommitId, CommitId),

    #[error("bisect range is invalid: {0} is not an ancestor of {1}")]
    InvalidBisectRange(CommitId, CommitId),

    #[error("invalid tag filter: {0}")]
    InvalidFilter(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("invalid commit id: {0}")]
    InvalidCommitId(String),
}

pub type Result<T> = std::result::Result<T, GraphError>;
