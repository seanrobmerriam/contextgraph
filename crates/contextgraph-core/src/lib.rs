//! # contextgraph-core
//!
//! A versioned, git-like context store for LLM agent conversations,
//! implemented in pure Rust. It treats the conversation window as a DAG:
//! every turn is a content-addressed commit, branches are named pointers,
//! and any point in history can be checked out, diffed, or rolled back to.
//!
//! Contextgraph stores and versions context; it **never** calls a model —
//! it's the substrate an agent runtime checks context in and out of.
//!
//! ## Core concepts
//!
//! * **Commit** — an immutable, content-addressed node representing one
//!   delta to the conversation state (a message, a tool call/result, or a
//!   compaction event). Identical content always yields the identical id.
//! * **Branch** — a named, mutable pointer to a commit (exactly like a
//!   `git` ref).
//! * **Checkout** — materializes the ordered message list a model would
//!   see by walking a commit's ancestry and folding deltas (including
//!   compaction ops) in order.
//! * **Bisect** — O(log n) binary search along an ancestry path for the
//!   commit where a caller-supplied predicate flips.
//!
//! ## Storage
//!
//! Persistence lives behind two traits: [`CommitStore`] (the content-
//! addressed blob graph) and [`RefStore`] (the branch pointers). The
//! concrete entry point [`ContextGraph`] combines them; bring your own
//! `S: CommitStore + RefStore`.
//!
//! This crate ships two ready-made stores:
//!
//! * [`mem::InMemoryGraphStore`] — fast, no dependencies, suitable for
//!   tests or short-lived processes.
//! * [`sqlite::SqliteStore`] — SQLite (WAL mode) for durable storage,
//!   designed for an agent runtime.
//!
//! ## Example
//!
//! ```
//! use chrono::Utc;
//! use contextgraph_core::{
//!     Author, Commit, ContextGraph, Delta, Metadata,
//!     commit::{compute_commit_id, CommitId},
//!     graph::CheckoutTarget,
//!     mem::InMemoryGraphStore,
//! };
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let store = InMemoryGraphStore::new();
//! let graph = ContextGraph::new(store);
//!
//! let meta = Metadata::new(Utc::now());
//! let id = graph
//!     .commit_advancing_branch("main", Author::User, Delta::Message { content: "hi".into() }, meta)
//!     .await?;
//!
//! let ctx = graph.checkout(CheckoutTarget::Branch("main".into())).await?;
//! assert_eq!(ctx.messages.len(), 1);
//! # Ok(()) }
//! ```

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
