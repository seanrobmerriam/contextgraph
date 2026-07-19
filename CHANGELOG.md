# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-19

### Added

- Initial public release of `contextgraph-core`, `contextgraph-cli`, and
  `contextgraph-mcp`.
- Content-addressed `Commit` model (BLAKE3) with `parent_ids`, `author`,
  `delta` (Message / ToolCall / ToolResult / Compaction / Merge), and
  caller-supplied `Metadata` (model id, token usage, tags, timestamp).
- `CommitStore` and `RefStore` traits, with in-memory and SQLite (WAL) reference
  implementations.
- `ContextGraph` library API combining both stores: `commit`,
  `commit_advancing_branch`, `branch`, `move_branch`, `delete_branch`,
  `fork`, `rollback`, `checkout` (materializes the ordered message list),
  `log` (ancestry walk with tag filtering and pagination), `diff`
  (structural LCS diff between two materialized contexts), `merge` (explicit
  merge commit with `RecordOnly` / `PreferOther` strategies), `bisect`
  (O(log n) ancestry bisect with caller-supplied predicate), `gc`
  (explicit, opt-in removal of unreachable commits).
- `contextgraph` CLI binary exposing the same operations against a SQLite file,
  with `--json` output.
- `contextgraph-mcp` MCP server exposing `commit`, `branch`, `checkout`,
  `fork`, `diff`, `log`, `bisect` as tools (plus branch-list/HEAD resources)
  over stdio.
