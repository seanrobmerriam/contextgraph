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

