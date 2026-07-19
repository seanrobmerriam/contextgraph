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
