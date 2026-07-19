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

fn call_args(pairs: &[(&str, &str)]) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    map
}

fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .map(|t| t.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn spawn_client(
    db_path: &str,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    let bin = env!("CARGO_BIN_EXE_contextgraph-mcp");
    let db_path = db_path.to_string();
    let transport = TokioChildProcess::new(tokio::process::Command::new(bin).configure(|cmd| {
        cmd.env("CONTEXTGRAPH_DB", &db_path);
    }))?;
    let client = ().serve(transport).await?;
    Ok(client)
}
