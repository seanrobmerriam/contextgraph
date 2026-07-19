//! End-to-end CLI tests: drive the built binary against a temp SQLite file
//! and assert on stdout, exactly as a real user's shell session would.

use std::process::{Command, Output};

struct Db {
    _dir: tempfile::TempDir,
    path: String,
}

impl Db {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db").to_string_lossy().to_string();
        Self { _dir: dir, path }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_contextgraph"))
            .arg("--db")
            .arg(&self.path)
            .args(args)
            .output()
            .expect("failed to run contextgraph binary")
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

#[test]
fn committing_and_checking_out_a_branch_round_trips_the_message() {
    let db = Db::new();
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "hello",
    ]);
    let out = db.stdout(&["checkout", "main"]);
    assert_eq!(out, "[user] hello");
}

#[test]
fn forking_from_main_creates_an_independent_branch_at_the_same_head() {
    let db = Db::new();
    let head = db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "root",
    ]);
    db.stdout(&["fork", "main", "speculative"]);
    let branches = db.stdout(&["branches"]);
    assert!(branches.contains("speculative"));
    assert!(branches.contains(&head));
}

#[test]
fn diffing_two_forked_branches_shows_their_divergent_turns() {
    let db = Db::new();
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "root",
    ]);
    db.stdout(&["fork", "main", "a"]);
    db.stdout(&["fork", "main", "b"]);
    db.stdout(&[
        "commit",
        "--branch",
        "a",
        "--author",
        "assistant",
        "--message",
        "plan-a",
    ]);
    db.stdout(&[
        "commit",
        "--branch",
        "b",
        "--author",
        "assistant",
        "--message",
        "plan-b",
    ]);

    let diff = db.stdout(&["diff", "a", "b"]);
    assert!(diff.contains("- plan-a"));
    assert!(diff.contains("+ plan-b"));
}

#[test]
fn bisect_finds_the_commit_where_a_substring_disappears() {
    let db = Db::new();
    let mut ids = Vec::new();
    for msg in [
        "order 123 placed",
        "still mentions order 123",
        "no mention now",
    ] {
        ids.push(db.stdout(&[
            "commit",
            "--branch",
            "main",
            "--author",
            "assistant",
            "--message",
            msg,
        ]));
    }

    let out = db.stdout(&["bisect", &ids[0], &ids[2], "--contains", "order 123"]);
    assert!(out.contains(&format!("last_good={}", ids[1])));
    assert!(out.contains(&format!("first_bad={}", ids[2])));
}

#[test]
fn merge_advances_branch_a_and_json_output_is_valid_json() {
    let db = Db::new();
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "root",
    ]);
    db.stdout(&["fork", "main", "a"]);
    db.stdout(&["fork", "main", "b"]);
    db.stdout(&[
        "commit",
        "--branch",
        "a",
        "--author",
        "assistant",
        "--message",
        "a-turn",
    ]);
    db.stdout(&[
        "commit",
        "--branch",
        "b",
        "--author",
        "assistant",
        "--message",
        "b-turn",
    ]);

    let out = db.run(&["--json", "merge", "a", "b", "--strategy", "record-only"]);
    assert!(out.status.success());
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("merge --json output must be valid JSON");
    assert!(json.get("commit_id").is_some());
}

#[test]
fn checking_out_a_nonexistent_branch_exits_nonzero_with_a_clear_error() {
    let db = Db::new();
    let out = db.run(&["checkout", "no-such-branch"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("BranchNotFound") || stderr.contains("no-such-branch"));
}

#[test]
fn gc_reports_zero_removed_when_every_commit_is_still_reachable() {
    let db = Db::new();
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "root",
    ]);
    let out = db.stdout(&["gc"]);
    assert!(out.contains("removed 0"));
}

#[test]
fn json_checkout_output_round_trips_through_serde() {
    let db = Db::new();
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "hello",
    ]);
    let out = db.run(&["--json", "checkout", "main"]);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["messages"][0]["delta"]["content"], "hello");
}

#[test]
fn branch_move_branch_and_delete_branch_round_trip() {
    let db = Db::new();
    let root = db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "root",
    ]);
    db.stdout(&["branch", "b1", &root]);
    let branches = db.stdout(&["branches"]);
    // Exact-match on the branch-name column: a bare `contains("b1")` could
    // spuriously match "b1" appearing inside some other branch's hex commit
    // id (hex digits include 'b' and '1').
    assert!(branches.lines().any(|l| l.starts_with("b1\t")));

    let second = db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "assistant",
        "--message",
        "two",
    ]);
    db.stdout(&["move-branch", "b1", &second]);
    let out = db.stdout(&["checkout", "b1"]);
    assert!(out.contains("two"));

    db.stdout(&["delete-branch", "b1"]);
    let branches_after = db.stdout(&["branches"]);
    assert!(!branches_after.lines().any(|l| l.starts_with("b1\t")));
}

#[test]
fn rollback_moves_branch_backward_and_root_commit_with_no_branch_prints_a_bare_id() {
    let db = Db::new();
    // A commit with neither --branch nor --parent is an unattached root commit.
    let root = db.stdout(&["commit", "--author", "user", "--message", "root"]);
    assert_eq!(root.len(), 64);

    db.stdout(&["branch", "main", &root]);
    let second = db.stdout(&[
        "commit",
        "--parent",
        &root,
        "--author",
        "assistant",
        "--message",
        "two",
    ]);
    db.stdout(&["move-branch", "main", &second]);
    db.stdout(&["rollback", "main", &root]);
    let out = db.stdout(&["checkout", "main"]);
    assert_eq!(out, "[user] root");
}

#[test]
fn show_prints_commit_metadata_for_a_hex_id() {
    let db = Db::new();
    let root = db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "system",
        "--message",
        "root",
    ]);
    let out = db.stdout(&["show", &root]);
    assert!(out.contains(&format!("commit {root}")));
    assert!(out.contains("author: system"));
    assert!(out.contains("delta: root"));
}

#[test]
fn merge_prefer_other_materializes_branch_b_view() {
    let db = Db::new();
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "root",
    ]);
    db.stdout(&["fork", "main", "a"]);
    db.stdout(&["fork", "main", "b"]);
    db.stdout(&[
        "commit",
        "--branch",
        "a",
        "--author",
        "assistant",
        "--message",
        "a-turn",
    ]);
    db.stdout(&[
        "commit",
        "--branch",
        "b",
        "--author",
        "assistant",
        "--message",
        "b-turn",
    ]);
    db.stdout(&["merge", "a", "b", "--strategy", "prefer-other"]);
    let out = db.stdout(&["checkout", "a"]);
    assert!(out.contains("b-turn"));
    assert!(!out.contains("a-turn"));
}

#[test]
fn log_with_tag_filter_and_pagination_narrows_results() {
    let db = Db::new();
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "one",
        "--tag",
        "step=planning",
    ]);
    db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "assistant",
        "--message",
        "two",
        "--tag",
        "step=execution",
    ]);
    let out = db.stdout(&["log", "main", "--tag", "step=planning"]);
    assert!(out.contains("one"));
    assert!(!out.contains("two"));

    let paged = db.stdout(&["log", "main", "--limit", "1"]);
    assert!(paged.contains("(2 total, has_more=true)"));
}

#[test]
fn gc_removes_a_commit_unreachable_from_any_branch() {
    let db = Db::new();
    let root = db.stdout(&[
        "commit",
        "--branch",
        "main",
        "--author",
        "user",
        "--message",
        "root",
    ]);
    // Orphan: committed with --parent, never attached to a branch.
    db.stdout(&[
        "commit",
        "--parent",
        &root,
        "--author",
        "assistant",
        "--message",
        "orphan",
    ]);
    let out = db.stdout(&["--json", "gc"]);
    let ids: Vec<String> = serde_json::from_str(&out).unwrap();
    assert_eq!(ids.len(), 1);
}

#[test]
fn committing_with_a_nonexistent_parent_exits_nonzero() {
    let db = Db::new();
    let out = db.run(&[
        "commit",
        "--parent",
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        "--author",
        "user",
        "--message",
        "x",
    ]);
    assert!(!out.status.success());
}
