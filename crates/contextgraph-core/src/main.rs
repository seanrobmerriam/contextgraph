//! MCP server exposing contextgraph's fork/checkout/diff/log/bisect as
//! tools, plus the branch list and (conventional) HEAD as resources, over
//! stdio. Contextgraph never calls a model itself — this server only lets
//! an agent (or a developer driving one) branch its own conversation,
//! compare two continuations, or bisect a regression without leaving the
//! chat loop.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult,
        Resource, ResourceContents, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::Deserialize;

use contextgraph_core::commit::Delta;
use contextgraph_core::graph::{CheckoutTarget, ContextGraph};
use contextgraph_core::log::LogFilter;
use contextgraph_core::sqlite::SqliteStore;
use contextgraph_core::{CommitId, GraphError, RefStore};

/// A bare hex commit id is a commit; anything else is a branch name (same
/// heuristic the CLI uses).
fn parse_ref(s: &str) -> CheckoutTarget {
    match CommitId::from_str(s) {
        Ok(id) => CheckoutTarget::Commit(id),
        Err(_) => CheckoutTarget::Branch(s.to_string()),
    }
}

fn mcp_err(e: GraphError) -> McpError {
    McpError::invalid_params(e.to_string(), None)
}

fn to_json(value: &impl serde::Serialize) -> Result<String, McpError> {
    serde_json::to_string_pretty(value).map_err(|e| McpError::internal_error(e.to_string(), None))
}

fn describe_delta(delta: &Delta) -> String {
    match delta {
        Delta::Message { content } => content.clone(),
        Delta::ToolCall { name, call_id, .. } => format!("tool_call {name} ({call_id})"),
        Delta::ToolResult { call_id, .. } => format!("tool_result ({call_id})"),
        Delta::Compaction { summary, .. } => summary.clone(),
        Delta::Merge { strategy } => format!("[merge: {strategy:?}]"),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ForkParams {
    /// The commit or branch to fork from.
    from: String,
    /// Name of the new branch to create at that point.
    new_branch_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CheckoutParams {
    /// A branch name or a hex commit id.
    target: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DiffParams {
    /// A branch name or a hex commit id (the "old"/"from" side).
    a: String,
    /// A branch name or a hex commit id (the "new"/"to" side).
    b: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LogParams {
    /// A branch name or a hex commit id to walk ancestry from.
    target: String,
    /// Only include commits carrying all of these metadata tags (exact match).
    #[serde(default)]
    tags: BTreeMap<String, String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_log_limit")]
    limit: usize,
}

fn default_log_limit() -> usize {
    20
}
