//! AC-4: the `reviewer` role is read-only. It writes its verdict and report under the run's
//! `out/` directory and nothing else. The review stage sets no `worktree`, so the item's worktree
//! is read-only for it. It has no push, and every forge write is denied (ADR-0005).

mod common;

use std::path::PathBuf;

use common::*;
use pf_rules::{Context, OwnPaths, ToolCall, decide};
use pf_workflow::rules::Allow;

/// The reviewer's seat as the controller builds it for the `review` stage: its working directory
/// is the item's worktree, for reading, and the `worktree` setting is empty. Its report directory
/// is `<run>/out/<unit>`, and the run directory is the other path it may write.
fn reviewer_ctx() -> Context {
    Context {
        seat: "reviewer/u1".to_string(),
        current_branch: String::new(),
        own_branch: String::new(),
        own_tag: None,
        default_branch: "main".to_string(),
        api_hosts: vec!["api.github.com".to_string()],
        cwd: PathBuf::from("/work/u1"),
        push_dir: PathBuf::new(),
        paths: OwnPaths {
            scratch: PathBuf::from("/run/u1/out/u1/scratch"),
            build_target: PathBuf::from("/build/u1"),
            worktree: PathBuf::new(),
            report_and_run: vec![PathBuf::from("/run/u1/out/u1"), PathBuf::from("/run/u1")],
        },
    }
}

fn write(path: &str) -> ToolCall {
    ToolCall::write("write", path)
}

#[test]
fn the_shipped_file_lists_only_the_report_and_run_files() {
    let r = shipped("reviewer");
    assert_eq!(r.role, "reviewer");
    assert_eq!(r.allow, [Allow::WriteOwnReportAndRunFiles]);
}

#[test]
fn a_write_tool_inside_the_item_worktree_is_denied() {
    let r = shipped("reviewer");
    let ctx = reviewer_ctx();
    for tool in ["write", "edit", "multiedit", "notebook_edit", "apply_patch"] {
        for path in ["/work/u1/src/main.rs", "/work/u1/review.md", "src/lib.rs"] {
            let d = decide(&r, &ctx, &ToolCall::write(tool, path));
            assert!(
                !d.is_allow() && d.rule == "write-outside-worktree",
                "{tool} {path}: {d:?}"
            );
        }
    }
}

#[test]
fn a_write_to_its_report_under_out_is_allowed() {
    let r = shipped("reviewer");
    let ctx = reviewer_ctx();
    for path in [
        "/run/u1/out/u1/review.md",
        "/run/u1/out/u1/decisions.md",
        "/run/u1/out/u1/review-notes.txt",
    ] {
        let d = decide(&r, &ctx, &write(path));
        assert!(
            d.is_allow() && d.rule == "write-own-report-and-run-files",
            "{path}: {d:?}"
        );
    }
    // The paths that lead out of it are still denied.
    for path in [
        "/etc/passwd",
        "/work/u2/x",
        "/run/u1/../u2/x",
        "/work/u1/../../etc/x",
    ] {
        let d = decide(&r, &ctx, &write(path));
        assert!(!d.is_allow(), "{path}: {d:?}");
    }
}

#[test]
fn git_push_is_denied_in_each_form() {
    let r = shipped("reviewer");
    let ctx = reviewer_ctx();
    for c in [
        "git push",
        "git push origin work/u1",
        "git -C /work/u1 push origin work/u1",
        "git push origin HEAD",
        "git push origin work/u1 --tags",
        "git push --force origin work/u1",
    ] {
        let d = decide(&r, &ctx, &ToolCall::bash(c));
        assert!(!d.is_allow(), "{c}: {d:?}");
    }
    // The role lists no push, so a push of the item's branch is not the listed case either.
    let mut own = reviewer_ctx();
    own.own_branch = "work/u1".to_string();
    own.current_branch = "work/u1".to_string();
    let d = decide(&r, &own, &ToolCall::bash("git push origin work/u1"));
    assert!(!d.is_allow() && d.rule == "push-not-allowed", "{d:?}");
}

#[test]
fn a_review_comment_or_approval_on_the_forge_is_denied() {
    let r = shipped("reviewer");
    let ctx = reviewer_ctx();
    for c in [
        "gh pr review --approve",
        "gh pr review 12 --approve",
        "gh pr review 12 --request-changes -b 'no'",
        "gh pr review --comment -b 'fine'",
        "gh pr comment 12 -b 'looks good'",
        "gh pr comment 12 --body-file review.md",
        "gh --repo acme/widgets pr comment 12 -b x",
        "gh -R acme/widgets pr review 12 --approve",
        "echo ok && gh pr review --approve",
        "gh pr view 12 && gh pr comment 12 -b x",
        "bash -c 'gh pr comment 12 -b x'",
        "echo $(gh pr comment 12 -b x)",
    ] {
        let d = decide(&r, &ctx, &ToolCall::bash(c));
        assert!(
            !d.is_allow() && d.rule == "gh-pr-write",
            "{c}: {:?} {} ({})",
            d.outcome,
            d.rule,
            d.reason
        );
    }
}

#[test]
fn a_forge_write_is_denied_for_every_role_and_a_forge_read_is_not() {
    for c in [
        "gh pr review --approve",
        "gh pr comment 3 -b x",
        "gh pr edit 3 --base main",
        "gh pr close 3",
        "gh pr reopen 3",
        "gh pr ready 3",
    ] {
        denied_for_every_role(c, "gh-pr-write");
    }
    for c in [
        "gh pr view 12 --json headRefOid --jq .headRefOid",
        "gh pr view work/u1 --repo acme/widgets --json body,headRefOid",
        "gh pr diff 12",
        "gh pr list --head work/u1 --state open",
        "gh pr checks 12",
        "gh pr status",
        "gh pr create --base main --head work/u1 --title t --body b",
        "echo \"gh pr comment 12\"",
        "git log --grep='gh pr review'",
    ] {
        allowed_for_every_role(c);
    }
}

#[test]
fn the_denials_of_every_seat_hold_for_the_reviewer() {
    let r = shipped("reviewer");
    let ctx = reviewer_ctx();
    for (c, rule) in [
        ("gh pr merge 12", "gh-pr-merge"),
        ("git push origin main", "push-default-branch"),
        ("git push origin :old", "branch-delete"),
        ("git branch -D work/u1", "branch-delete"),
        ("git commit --no-verify -m x", "no-verify"),
        (
            "git commit -m 'x\n\nCo-Authored-By: Claude <noreply@anthropic.com>'",
            "ai-trailer",
        ),
        ("gh api -X POST repos/o/r/issues/1/comments", "gh-api-write"),
        (
            "curl -X POST https://api.github.com/repos/o/r/issues/1/comments",
            "forge-api-curl",
        ),
    ] {
        let d = decide(&r, &ctx, &ToolCall::bash(c));
        assert!(!d.is_allow() && d.rule == rule, "{c}: {d:?}");
    }
}

#[test]
fn reading_the_pr_and_the_worktree_is_allowed() {
    let r = shipped("reviewer");
    let ctx = reviewer_ctx();
    for c in [
        "git log --oneline -n 20",
        "git diff 0123abc..HEAD --stat",
        "git show HEAD",
        "git rev-parse HEAD",
        "git status --porcelain",
        "cat /run/u1/out/u1/review-prev.md",
        "sed -n 1,40p src/main.rs",
        "grep -rn 'unsafe' src",
        "gh pr view 12 --json headRefOid --jq .headRefOid",
    ] {
        let d = decide(&r, &ctx, &ToolCall::bash(c));
        assert!(d.is_allow(), "{c}: {d:?}");
    }
}

#[test]
fn shell_redirection_is_not_guarded_by_path_and_the_brief_forbids_it() {
    // The known limit of ADR-0005: the rules read a command's words, not the files a redirection
    // opens. The reviewer's brief forbids a redirection into the worktree.
    let r = shipped("reviewer");
    let ctx = reviewer_ctx();
    let d = decide(&r, &ctx, &ToolCall::bash("echo x > /work/u1/notes.txt"));
    assert!(
        d.is_allow(),
        "if the rules start to guard redirection, update role-rules.md: {d:?}"
    );
    let brief = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../workflows/ossify-spine/briefs/review.md"),
    )
    .unwrap();
    assert!(brief.contains("redirection"), "{brief}");
    let doc = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/contracts/role-rules.md"),
    )
    .unwrap();
    assert!(
        doc.contains("redirection") && doc.contains("reviewer"),
        "role-rules.md must record the known limit for the reviewer"
    );
}
