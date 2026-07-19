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

type Graph = ContextGraph<SqliteStore>;

async fn resolve_commit_id(graph: &Graph, s: &str) -> Result<CommitId, GraphError> {
    graph.resolve(&parse_ref(s)).await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let store = SqliteStore::open(&cli.db).await?;
    let graph = ContextGraph::new(store);
    run(&graph, cli.command, cli.json).await
}

async fn run(
    graph: &Graph,
    command: Command,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Commit {
            branch,
            parent,
            author,
            message,
            tags,
        } => {
            let mut metadata = Metadata::new(chrono::Utc::now());
            for (k, v) in tags {
                metadata = metadata.with_tag(k, v);
            }
            let delta = Delta::Message { content: message };

            let id = if let Some(branch_name) = branch {
                graph
                    .commit_advancing_branch(&branch_name, author, delta, metadata)
                    .await?
            } else if let Some(parent_ref) = parent {
                let parent_id = resolve_commit_id(graph, &parent_ref).await?;
                graph
                    .commit(Some(parent_id), author, delta, metadata)
                    .await?
            } else {
                graph.commit(None, author, delta, metadata).await?
            };

            if json {
                println!("{}", serde_json::json!({ "commit_id": id.to_hex() }));
            } else {
                println!("{}", id.to_hex());
            }
        }

        Command::Branch { name, at } => {
            let at_id = resolve_commit_id(graph, &at).await?;
            graph.branch(&name, at_id).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "branch": name, "commit_id": at_id.to_hex() })
                );
            } else {
                println!("created branch '{name}' at {}", at_id.to_hex());
            }
        }

        Command::Branches => {
            let branches = graph.list_branches().await?;
            if json {
                let obj: serde_json::Value = branches
                    .iter()
                    .map(|(n, id)| (n.clone(), serde_json::Value::String(id.to_hex())))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&obj)?);
            } else {
                for (name, id) in branches {
                    println!("{name}\t{}", id.to_hex());
                }
            }
        }

        Command::MoveBranch { name, to } => {
            let to_id = resolve_commit_id(graph, &to).await?;
            graph.move_branch(&name, to_id).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "branch": name, "commit_id": to_id.to_hex() })
                );
            } else {
                println!("moved '{name}' to {}", to_id.to_hex());
            }
        }

        Command::DeleteBranch { name } => {
            graph.delete_branch(&name).await?;
            if json {
                println!("{}", serde_json::json!({ "deleted": name }));
            } else {
                println!("deleted branch '{name}'");
            }
        }

        Command::Fork { from, name } => {
            let from_id = resolve_commit_id(graph, &from).await?;
            graph.fork(from_id, &name).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "branch": name, "commit_id": from_id.to_hex() })
                );
            } else {
                println!("forked '{name}' at {}", from_id.to_hex());
            }
        }

        Command::Rollback { branch, to } => {
            let to_id = resolve_commit_id(graph, &to).await?;
            graph.rollback(&branch, to_id).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "branch": branch, "commit_id": to_id.to_hex() })
                );
            } else {
                println!("rolled back '{branch}' to {}", to_id.to_hex());
            }
        }

        Command::Checkout { target } => {
            let ctx = graph.checkout(parse_ref(&target)).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&ctx)?);
            } else {
                for m in &ctx.messages {
                    println!("[{}] {}", m.author, describe_delta(&m.delta));
                }
            }
        }

        Command::Show { commit } => {
            let id = resolve_commit_id(graph, &commit).await?;
            let c = graph
                .store()
                .get(&id)
                .await?
                .ok_or(GraphError::CommitNotFound(id))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&c)?);
            } else {
                println!("commit {}", c.id.to_hex());
                println!("author: {}", c.author);
                println!(
                    "parents: {}",
                    c.parent_ids
                        .iter()
                        .map(|p| p.to_hex())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!("delta: {}", describe_delta(&c.delta));
            }
        }

        Command::Log {
            target,
            tags,
            offset,
            limit,
        } => {
            let mut filter = LogFilter::new();
            for (k, v) in tags {
                filter = filter.with_tag(k, v);
            }
            let page = graph
                .log(parse_ref(&target), &filter, offset, limit)
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&page)?);
            } else {
                for c in &page.commits {
                    println!("{}  {}", c.id.to_hex(), describe_delta(&c.delta));
                }
                println!("({} total, has_more={})", page.total_matched, page.has_more);
            }
        }

        Command::Diff { a, b } => {
            let d = graph.diff(parse_ref(&a), parse_ref(&b)).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                use contextgraph_core::diff::DiffOp;
                for op in &d.ops {
                    match op {
                        DiffOp::Common(m) => println!("  {}", describe_delta(&m.delta)),
                        DiffOp::Added(m) => println!("+ {}", describe_delta(&m.delta)),
                        DiffOp::Removed(m) => println!("- {}", describe_delta(&m.delta)),
                    }
                }
            }
        }

        Command::Merge {
            branch_a,
            branch_b,
            strategy,
        } => {
            let id = graph.merge(&branch_a, &branch_b, strategy).await?;
            if json {
                println!("{}", serde_json::json!({ "commit_id": id.to_hex() }));
            } else {
                println!("merged '{branch_b}' into '{branch_a}' -> {}", id.to_hex());
            }
        }

        Command::Bisect {
            good,
            bad,
            contains,
        } => {
            let good_id = resolve_commit_id(graph, &good).await?;
            let bad_id = resolve_commit_id(graph, &bad).await?;
            // Checks the *current* state (the latest message), not whether
            // the substring ever appeared anywhere in history — bisecting
            // "does the plan still reference X" is about the live tail of
            // context, not its full past.
            let outcome = graph
                .bisect(good_id, bad_id, |ctx| {
                    ctx.messages
                        .last()
                        .is_some_and(|m| describe_delta(&m.delta).contains(&contains))
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                use contextgraph_core::bisect::BisectOutcome;
                match outcome {
                    BisectOutcome::Flip(r) => println!(
                        "flip found: last_good={} first_bad={} ({} predicate call(s))",
                        r.last_good.to_hex(),
                        r.first_bad.to_hex(),
                        r.predicate_calls
                    ),
                    BisectOutcome::NoFlip => println!("no flip found"),
                }
            }
        }

        Command::Gc => {
            let removed = graph.gc().await?;
            if json {
                let ids: Vec<String> = removed.iter().map(|id| id.to_hex()).collect();
                println!("{}", serde_json::to_string_pretty(&ids)?);
            } else {
                println!("removed {} unreachable commit(s)", removed.len());
                for id in removed {
                    println!("  {}", id.to_hex());
                }
            }
        }
    }
    Ok(())
}
