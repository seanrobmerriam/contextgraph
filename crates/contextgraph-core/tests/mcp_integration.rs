//! MCP integration tests: drive fork/checkout/diff/log/bisect over stdio
//! against a constructed multi-branch DAG fixture, spawning the real
//! contextgraph-mcp binary as a child process (the same path a real MCP
//! client uses).

use chrono::Utc;
use contextgraph_core::commit::{Author, Delta, Metadata};
use contextgraph_core::graph::ContextGraph;
use contextgraph_core::sqlite::SqliteStore;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::Value;

fn text(s: &str) -> Delta {
    Delta::Message {
        content: s.to_string(),
    }
}

/// Seeds a multi-branch fixture: a shared root + one common turn, then
/// forked branches `a` and `b` each with their own divergent turn. Also
/// builds an independent linear `bisect-main` branch with a known flip
/// point, for the bisect test.
async fn seed_fixture(db_path: &str) -> anyhow::Result<()> {
    let store = SqliteStore::open(db_path).await?;
    let graph = ContextGraph::new(store);

    let root = graph
        .commit(None, Author::User, text("root"), Metadata::new(Utc::now()))
        .await?;
    let shared = graph
        .commit(
            Some(root),
            Author::Assistant,
            text("shared turn"),
            Metadata::new(Utc::now()),
        )
        .await?;
    graph.branch("a", shared).await?;
    graph.branch("b", shared).await?;

    let a_head = graph
        .commit(
            Some(shared),
            Author::Assistant,
            text("branch-a-turn"),
            Metadata::new(Utc::now()),
        )
        .await?;
    graph.move_branch("a", a_head).await?;

    let b_head = graph
        .commit(
            Some(shared),
            Author::Assistant,
            text("branch-b-turn"),
            Metadata::new(Utc::now()),
        )
        .await?;
    graph.move_branch("b", b_head).await?;

    // A separate linear chain with a known bisect flip point.
    for (i, msg) in [
        "order 123 placed",
        "plan references order 123",
        "plan still references order 123",
        "plan updated, no mention",
    ]
    .into_iter()
    .enumerate()
    {
        let _ = i;
        graph
            .commit_advancing_branch(
                "bisect-main",
                Author::Assistant,
                text(msg),
                Metadata::new(Utc::now()),
            )
            .await?;
    }

    Ok(())
}
