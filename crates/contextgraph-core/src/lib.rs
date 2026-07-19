//! contextgraph-core: a versioned, git-like context store for LLM agent
//! conversations. It stores and versions context; it never calls a model.

pub mod bisect;
pub mod commit;
pub mod diff;
pub mod error;
pub mod gc;
pub mod graph;
pub mod log;
pub mod materialize;
pub mod mem;
pub mod merge;
pub mod sqlite;
pub mod store;

pub use bisect::{BisectOutcome, BisectResult};
pub use commit::{Author, Commit, CommitId, Delta, MergeStrategy, Metadata, TokenUsage};
pub use diff::{ContextDiff, DiffOp};
pub use error::{GraphError, Result};
pub use graph::{CheckoutTarget, ContextGraph};
pub use log::{LogFilter, LogPage};
pub use materialize::{MaterializedContext, MaterializedMessage};
pub use store::{CommitStore, RefStore};
