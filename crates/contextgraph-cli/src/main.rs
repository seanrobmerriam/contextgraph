use clap::{Parser, Subcommand};
use contextgraph_core::commit::{Author, Delta, MergeStrategy, Metadata};
use contextgraph_core::graph::{CheckoutTarget, ContextGraph};
use contextgraph_core::log::LogFilter;
use contextgraph_core::sqlite::SqliteStore;
use contextgraph_core::{CommitId, CommitStore, GraphError};
use std::str::FromStr;

#[derive(Parser)]
#[command(
    name = "contextgraph",
    about = "Versioned, git-like context store for LLM agent conversations"
)]
struct Cli {
    /// Path to the SQLite store (created if missing).
    #[arg(long, global = true, default_value = "contextgraph.db")]
    db: String,

    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a commit: a root commit, or a child of --parent / the head of --branch.
    Commit {
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value = "user", value_parser = parse_author)]
        author: Author,
        #[arg(long)]
        message: String,
        #[arg(long = "tag", value_parser = parse_tag)]
        tags: Vec<(String, String)>,
    },
    /// Create a branch pointer at an existing commit.
    Branch { name: String, at: String },
    /// List all branches and their current heads.
    Branches,
    /// Move an existing branch pointer to a different commit.
    MoveBranch { name: String, to: String },
    /// Delete a branch pointer (the commits it pointed to are untouched).
    DeleteBranch { name: String },
    /// Create a new branch at an existing commit — the developer-facing
    /// verb for "try an alternative continuation from here".
    Fork { from: String, name: String },
    /// Move a branch pointer backward; commits ahead of it are not deleted.
    Rollback { branch: String, to: String },
    /// Materialize the ordered message list at a commit or branch.
    Checkout { target: String },
    /// Show a single commit's raw content.
    Show { commit: String },
    /// Walk ancestry history from a commit or a branch's current head.
    Log {
        target: String,
        #[arg(long = "tag", value_parser = parse_tag)]
        tags: Vec<(String, String)>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Structural diff between two commits and/or branches.
    Diff { a: String, b: String },
    /// Merge branch_b into branch_a, creating an explicit merge commit.
    Merge {
        branch_a: String,
        branch_b: String,
        #[arg(long, default_value = "record-only", value_parser = parse_strategy)]
        strategy: MergeStrategy,
    },
    /// Binary-search the ancestry between good and bad for where a
    /// substring disappears from the materialized context.
    Bisect {
        good: String,
        bad: String,
        #[arg(long)]
        contains: String,
    },
    /// Explicit, opt-in garbage collection of commits unreachable from any branch.
    Gc,
}

fn parse_author(s: &str) -> Result<Author, String> {
    match s {
        "user" => Ok(Author::User),
        "assistant" => Ok(Author::Assistant),
        "tool" => Ok(Author::Tool),
        "system" => Ok(Author::System),
        other => Err(format!(
            "unknown author '{other}': expected user, assistant, tool, or system"
        )),
    }
}

fn parse_strategy(s: &str) -> Result<MergeStrategy, String> {
    match s {
        "record-only" => Ok(MergeStrategy::RecordOnly),
        "prefer-other" => Ok(MergeStrategy::PreferOther),
        other => Err(format!(
            "unknown strategy '{other}': expected record-only or prefer-other"
        )),
    }
}

fn parse_tag(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("tag '{s}' must be in KEY=VALUE form"))
}

/// A bare hex commit id is treated as a commit; anything else is a branch
/// name (mirroring git's "looks like a SHA vs. looks like a ref" heuristic).
fn parse_ref(s: &str) -> CheckoutTarget {
    match CommitId::from_str(s) {
        Ok(id) => CheckoutTarget::Commit(id),
        Err(_) => CheckoutTarget::Branch(s.to_string()),
    }
}

fn describe_delta(delta: &Delta) -> String {
    match delta {
        Delta::Message { content } => content.clone(),
        Delta::ToolCall { name, call_id, .. } => format!("tool_call {name} ({call_id})"),
        Delta::ToolResult { call_id, .. } => format!("tool_result ({call_id})"),
        Delta::Compaction { summary, replaces } => {
            format!("[compaction of {} commit(s)] {summary}", replaces.len())
        }
        Delta::Merge { strategy } => format!("[merge: {strategy:?}]"),
    }
}
