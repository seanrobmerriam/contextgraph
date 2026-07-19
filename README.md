# Contextgraph

A versioned, git-like context store for LLM agent conversations, written in
Rust. It treats the context window as a git-like DAG: every turn is a
commit, workflows can branch and fork speculative paths, and any point in
history can be checked out, diffed, or rolled back to.

Contextgraph is the storage/versioning substrate — **it does not run
agents or call models itself**. It's the thing an agent runtime checks
context in and out of, the way a build tool checks code in and out of git.

## Core concept: commits, not messages

The fundamental unit is a **commit**, not a raw chat message — an
immutable, content-addressed node representing one delta to the
conversation state (a message, a tool call/result, or a compaction event
that condenses N prior turns into one summary without deleting them).
Commit ids are BLAKE3 hashes of their own content, so identical content
always yields the identical id and forks that converge share storage for
free.

A **branch** is a named, mutable pointer to a commit — exactly like a git
ref. **Checkout** materializes the ordered message list a model would see
by walking a commit's ancestry and folding deltas (including compaction
ops) in order.

## Crates

- **`contextgraph-core`** — the library: the commit/DAG model, content
  addressing, `CommitStore`/`RefStore` traits (in-memory + SQLite/WAL
  implementations), and the `ContextGraph` engine (`commit`, `branch`,
  `checkout`, `fork`, `rollback`, `log`, `diff`, `merge`, `bisect`, `gc`).
  This is the primary integration surface for an agent runtime.
- **`contextgraph-cli`** — a `contextgraph` binary exposing the same
  operations over a SQLite file, with `--json` output.
- **`contextgraph-mcp`** — an MCP server exposing `fork`, `checkout`,
  `diff`, `log`, and `bisect` as tools (plus branch-list/HEAD resources)
  over stdio, so an agent can branch its own conversation, compare two
  continuations, or bisect a regression without leaving the chat loop.

## Building

```sh
cargo build --workspace
cargo test --workspace
```

## Worked example

Fork two continuations at a planning step, diff them, then bisect a
longer run to find where a bad tool result derailed the plan. This uses
the `contextgraph` CLI against a scratch SQLite file — the same steps map
directly onto the `contextgraph-core` library API or the MCP tools.

### 1. Set up a planning step, then fork two continuations from it

```sh
DB=/tmp/example.db
CG="cargo run -q -p contextgraph-cli --bin contextgraph -- --db $DB"

$CG commit --branch main --author user --message "Cancel order #4821"
$CG commit --branch main --author assistant --message "Plan: look up order #4821, then cancel it"

# Fork two speculative continuations from the same decision point.
$CG fork main plan-a
$CG fork main plan-b

$CG commit --branch plan-a --author assistant --message "plan-a: call cancel_order(4821) directly"
$CG commit --branch plan-b --author assistant --message "plan-b: verify order status before cancelling"
```

### 2. Diff the two continuations

```sh
$CG diff plan-a plan-b
```

```
  Cancel order #4821
  Plan: look up order #4821, then cancel it
- plan-a: call cancel_order(4821) directly
+ plan-b: verify order status before cancelling
```

The shared planning turns show up as common (`  `); each branch's own
continuation shows up as removed (`-`, plan-a's turn) or added (`+`,
plan-b's turn) — a structural diff of *turns*, never a text diff.

### 3. Bisect a longer run to find where a bad tool result derailed the plan

Suppose a longer conversation on `main` keeps planning around order #4821
even after it was cancelled — a regression introduced somewhere in the
middle of the run by a stale tool result.

```sh
$CG commit --branch main --author tool --message "tool_result: order #4821 status = cancelled"
$CG commit --branch main --author assistant --message "plan still references order #4821"
$CG commit --branch main --author assistant --message "plan still references order #4821"
$CG commit --branch main --author assistant --message "plan updated, no further action needed"

$CG log main --limit 10
```

Grab the oldest ("good") and newest ("bad") commit ids from `log`, then
bisect for where the substring `order #4821` disappears from the *latest*
message (i.e. the live tail of the conversation, not its full history):

```sh
$CG bisect <good-commit-id> <bad-commit-id> --contains "order #4821"
```

```
flip found: last_good=<commit where the plan still mentions #4821> first_bad=<commit where it's finally dropped> (5 predicate call(s))
```

`bisect` performs a proper O(log n) binary search over the ancestry path
— it doesn't linearly scan every commit — narrowing straight to the exact
commit where the plan stopped referencing the cancelled order.

### The same flow via the MCP server

```sh
CONTEXTGRAPH_DB=/tmp/example.db cargo run -q -p contextgraph-mcp
```

then call the `fork`, `checkout`, `diff`, `log`, and `bisect` tools (and
read the `contextgraph://branches` / `contextgraph://head` resources) over
stdio, exactly as above.

## Merging is not code merging

`merge(branch_a, branch_b, strategy)` creates an explicit merge commit
referencing both parents for audit/lineage. There is **no automatic
content merging** of divergent conversation turns — that's not
well-defined for natural language. `RecordOnly` materializes as
`branch_a`'s view (with `branch_b` linked as the alternative parent);
`PreferOther` materializes as `branch_b`'s view instead. If you need the
content of the branch that didn't "win", check it out directly — nothing
is deleted.

## Garbage collection

Commits are immutable and never deleted implicitly — dropping a branch
(`delete-branch`/`rollback`) never removes the commits it pointed to. `gc`
is a separate, explicit, opt-in operation that removes only commits
unreachable from every remaining branch.

## Explicitly out of scope

Automatic conflict resolution or semantic merging of divergent
conversation content, multi-node replication, model invocation
(Contextgraph stores and versions context; it never calls an LLM), and
encryption at rest.

## Coverage

```sh
cargo llvm-cov --workspace --fail-under-lines 80
```

