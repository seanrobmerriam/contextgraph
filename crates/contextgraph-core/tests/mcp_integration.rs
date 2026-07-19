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

#[tokio::test]
async fn fork_checkout_and_diff_work_over_stdio_against_a_multi_branch_fixture(
) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fixture.db").to_string_lossy().to_string();
    seed_fixture(&db_path).await?;

    let client = spawn_client(&db_path).await?;

    // fork: create a third branch from branch a's head.
    let fork_result = client
        .call_tool(
            CallToolRequestParams::new("fork")
                .with_arguments(call_args(&[("from", "a"), ("new_branch_name", "a-fork")])),
        )
        .await?;
    assert!(!fork_result.is_error.unwrap_or(false));
    assert!(text_of(&fork_result).contains("forked 'a-fork'"));

    // checkout: branch b's materialized context should contain both the
    // shared prefix and b's own divergent turn, but never a's.
    let checkout_result = client
        .call_tool(
            CallToolRequestParams::new("checkout").with_arguments(call_args(&[("target", "b")])),
        )
        .await?;
    let ctx: Value = serde_json::from_str(&text_of(&checkout_result))?;
    let contents: Vec<String> = ctx["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            m["delta"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(contents, vec!["root", "shared turn", "branch-b-turn"]);

    // diff: a vs b should show the shared prefix in common and each
    // branch's own turn as removed/added respectively.
    let diff_result = client
        .call_tool(
            CallToolRequestParams::new("diff").with_arguments(call_args(&[("a", "a"), ("b", "b")])),
        )
        .await?;
    let diff: Value = serde_json::from_str(&text_of(&diff_result))?;
    let ops = diff["ops"].as_array().unwrap();
    let kinds: Vec<&str> = ops
        .iter()
        .map(|op| op.as_object().unwrap().keys().next().unwrap().as_str())
        .collect();
    assert_eq!(kinds, vec!["Common", "Common", "Removed", "Added"]);

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn bisect_finds_the_flip_point_over_stdio() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fixture.db").to_string_lossy().to_string();
    seed_fixture(&db_path).await?;

    // Recover the good/bad commit ids from `log` first.
    let client = spawn_client(&db_path).await?;
    let log_result = client
        .call_tool(
            CallToolRequestParams::new("log")
                .with_arguments(call_args(&[("target", "bisect-main")])),
        )
        .await?;
    let page: Value = serde_json::from_str(&text_of(&log_result))?;
    let commits = page["commits"].as_array().unwrap();
    // log returns newest-first; last entry is the root ("order 123 placed").
    let bad_id = commits[0]["id"].as_str().unwrap().to_string();
    let good_id = commits[commits.len() - 1]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let bisect_result = client
        .call_tool(
            CallToolRequestParams::new("bisect").with_arguments(call_args(&[
                ("good", &good_id),
                ("bad", &bad_id),
                ("contains", "order 123"),
            ])),
        )
        .await?;
    let outcome: Value = serde_json::from_str(&text_of(&bisect_result))?;
    assert!(
        outcome.get("Flip").is_some(),
        "expected a flip, got {outcome:?}"
    );

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn resources_expose_branch_list_and_head() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fixture.db").to_string_lossy().to_string();

    // Seed only a `main` branch so HEAD is well-defined.
    let store = SqliteStore::open(&db_path).await?;
    let graph = ContextGraph::new(store);
    graph
        .commit_advancing_branch("main", Author::User, text("hi"), Metadata::new(Utc::now()))
        .await?;

    let client = spawn_client(&db_path).await?;
    let resources = client.list_all_resources().await?;
    let uris: Vec<String> = resources.iter().map(|r| r.uri.clone()).collect();
    assert!(uris.contains(&"contextgraph://branches".to_string()));
    assert!(uris.contains(&"contextgraph://head".to_string()));

    let head = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "contextgraph://head",
        ))
        .await?;
    let head_text = match &head.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        other => panic!("expected text resource contents, got {other:?}"),
    };
    assert_eq!(head_text.len(), 64); // a hex commit id

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn checking_out_a_nonexistent_branch_returns_a_tool_error() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fixture.db").to_string_lossy().to_string();
    seed_fixture(&db_path).await?;

    let client = spawn_client(&db_path).await?;
    let result = client
        .call_tool(
            CallToolRequestParams::new("checkout")
                .with_arguments(call_args(&[("target", "no-such-branch")])),
        )
        .await;

    // Either a protocol-level error or a tool-level isError result is
    // acceptable, but it must not silently succeed.
    if let Ok(r) = result {
        assert!(r.is_error.unwrap_or(false));
    }

    client.cancel().await?;
    Ok(())
}
