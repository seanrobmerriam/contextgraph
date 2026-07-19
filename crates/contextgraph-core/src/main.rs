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

#[derive(Debug, Deserialize, JsonSchema)]
struct BisectParams {
    /// A commit where the checked-for behavior is known to still hold.
    good: String,
    /// A commit where the checked-for behavior has flipped.
    bad: String,
    /// Substring to check for in the most recent (latest) message — this
    /// is a check against the live tail of the conversation, not its full
    /// history.
    contains: String,
}

#[derive(Clone)]
pub struct ContextGraphMcp {
    graph: Arc<ContextGraph<SqliteStore>>,
    // Read by the #[tool_handler]-generated call_tool/list_tools methods;
    // rustc's dead-code analysis doesn't see through that macro expansion.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl ContextGraphMcp {
    pub fn new(graph: ContextGraph<SqliteStore>) -> Self {
        Self {
            graph: Arc::new(graph),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl ContextGraphMcp {
    #[tool(
        description = "Create a new branch at an existing commit or branch head. This is the primary verb for A/B'ing a decision point: try two continuations from the same commit by forking twice."
    )]
    async fn fork(
        &self,
        Parameters(ForkParams {
            from,
            new_branch_name,
        }): Parameters<ForkParams>,
    ) -> Result<String, McpError> {
        let from_id = self
            .graph
            .resolve(&parse_ref(&from))
            .await
            .map_err(mcp_err)?;
        self.graph
            .fork(from_id, &new_branch_name)
            .await
            .map_err(mcp_err)?;
        Ok(format!(
            "forked '{new_branch_name}' at {}",
            from_id.to_hex()
        ))
    }

    #[tool(
        description = "Materialize the ordered message list a model would see at a commit or branch, as JSON."
    )]
    async fn checkout(
        &self,
        Parameters(CheckoutParams { target }): Parameters<CheckoutParams>,
    ) -> Result<String, McpError> {
        let ctx = self
            .graph
            .checkout(parse_ref(&target))
            .await
            .map_err(mcp_err)?;
        to_json(&ctx)
    }

    #[tool(
        description = "Structural diff between two commits and/or branches (which turns were added/removed/common), as JSON."
    )]
    async fn diff(
        &self,
        Parameters(DiffParams { a, b }): Parameters<DiffParams>,
    ) -> Result<String, McpError> {
        let d = self
            .graph
            .diff(parse_ref(&a), parse_ref(&b))
            .await
            .map_err(mcp_err)?;
        to_json(&d)
    }

    #[tool(
        description = "Walk ancestry history from a commit or a branch's current head, newest-first, with metadata-tag filtering and offset/limit pagination, as JSON."
    )]
    async fn log(
        &self,
        Parameters(LogParams {
            target,
            tags,
            offset,
            limit,
        }): Parameters<LogParams>,
    ) -> Result<String, McpError> {
        let mut filter = LogFilter::new();
        for (k, v) in tags {
            filter = filter.with_tag(k, v);
        }
        let page = self
            .graph
            .log(parse_ref(&target), &filter, offset, limit)
            .await
            .map_err(mcp_err)?;
        to_json(&page)
    }

    #[tool(
        description = "Binary-search the ancestry between a good and a bad commit for where a substring disappears from the latest message -- git-bisect for agent runs. Returns the last-good/first-bad commit pair, or 'no flip found'."
    )]
    async fn bisect(
        &self,
        Parameters(BisectParams {
            good,
            bad,
            contains,
        }): Parameters<BisectParams>,
    ) -> Result<String, McpError> {
        let good_id = self
            .graph
            .resolve(&parse_ref(&good))
            .await
            .map_err(mcp_err)?;
        let bad_id = self
            .graph
            .resolve(&parse_ref(&bad))
            .await
            .map_err(mcp_err)?;
        let outcome = self
            .graph
            .bisect(good_id, bad_id, |ctx| {
                ctx.messages
                    .last()
                    .is_some_and(|m| describe_delta(&m.delta).contains(&contains))
            })
            .await
            .map_err(mcp_err)?;
        to_json(&outcome)
    }
}
