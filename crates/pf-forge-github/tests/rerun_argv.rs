//! AC-6: the argv of each call that reads or runs the failed checks of a PR, and the reads of a
//! commit's tree and a branch's tip. A failed GitHub Actions check run of this repo reads as one
//! the forge can run again; a commit status and the check run of another app do not; a token with
//! no push rights blocks all of them. A head that moved gets no re-run. A check that still reads
//! failed after the re-run counts as not run. A truncated tree is an error. No argv holds
//! `--debug`, a shell or an unchecked SHA. No test reaches the network, and the wait between the
//! reads after a re-run is zero.

mod common;

use std::time::Duration;

use common::*;
use pf_engine::port::forge::{ForgePort, PrRef};
use pf_forge_github::GithubForge;
use serde_json::json;

const OTHER_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn pr() -> PrRef {
    PrRef {
        repo: REPO.to_string(),
        number: 7,
    }
}

fn forge(stub: &Stub) -> GithubForge {
    stub.forge().with_rerun_delay(Duration::ZERO)
}

/// The rollup of a PR with failed checks of each kind.
fn mixed_rollup() -> Vec<serde_json::Value> {
    vec![
        actions_run("build", "FAILURE", 11),
        actions_run("lint", "TIMED_OUT", 11),
        actions_run("e2e", "FAILURE", 12),
        failed_status("ci/legacy"),
        other_app_run("scan", "FAILURE"),
        // A check that passed and one that still runs are not failed.
        actions_run("docs", "SUCCESS", 13),
        running("IN_PROGRESS"),
    ]
}

// ------------------------------------------------------------------------ failed_checks

#[test]
fn failed_checks_reads_the_permissions_then_the_rollup() {
    let stub = Stub::new();
    stub.out("api-repo", &permissions(true));
    stub.out("pr-view", &failed_view_json(SHA, mixed_rollup()));
    forge(&stub).failed_checks(&pr()).unwrap();
    assert_eq!(
        stub.calls(),
        vec![
            vec!["api", "repos/synthetic/repo", "--jq", ".permissions"],
            vec![
                "pr",
                "view",
                "7",
                "-R",
                REPO,
                "--json",
                "headRefOid,statusCheckRollup"
            ],
        ]
    );
}

#[test]
fn a_failed_actions_check_run_can_run_again_and_the_others_cannot() {
    let stub = Stub::new();
    stub.out("api-repo", &permissions(true));
    stub.out("pr-view", &failed_view_json(SHA, mixed_rollup()));
    let got = forge(&stub).failed_checks(&pr()).unwrap();
    assert_eq!(got.head_sha, SHA);
    let by_name = |n: &str| got.failed.iter().find(|c| c.name == n).unwrap();
    for name in ["build", "lint", "e2e"] {
        assert_eq!(by_name(name).blocked, None, "{name}");
    }
    assert!(
        by_name("ci/legacy")
            .blocked
            .as_deref()
            .unwrap()
            .contains("a commit status that an outside CI wrote")
    );
    assert!(
        by_name("scan")
            .blocked
            .as_deref()
            .unwrap()
            .contains("another app")
    );
    assert_eq!(got.failed.len(), 5, "{got:?}");
}

#[test]
fn a_token_without_push_rights_blocks_every_check() {
    for perms in [
        permissions(false),
        "null".to_string(),
        json!({"admin": false}).to_string(),
    ] {
        let stub = Stub::new();
        stub.out("api-repo", &perms);
        stub.out("pr-view", &failed_view_json(SHA, mixed_rollup()));
        let got = forge(&stub).failed_checks(&pr()).unwrap();
        assert_eq!(got.failed.len(), 5);
        for check in &got.failed {
            let why = check.blocked.as_deref().unwrap();
            assert!(why.contains("the token may not re-run checks on synthetic/repo"));
        }
    }
    // Maintain and admin are enough.
    for key in ["maintain", "admin"] {
        let stub = Stub::new();
        stub.out("api-repo", &json!({ key: true }).to_string());
        stub.out("pr-view", &failed_view_json(SHA, mixed_rollup()));
        let got = forge(&stub).failed_checks(&pr()).unwrap();
        assert_eq!(got.failed[0].blocked, None, "{key}");
    }
}

#[test]
fn a_pr_with_no_failed_check_has_an_empty_list() {
    let stub = Stub::new();
    stub.out("api-repo", &permissions(true));
    stub.out(
        "pr-view",
        &failed_view_json(SHA, vec![actions_run("build", "SUCCESS", 1)]),
    );
    assert!(forge(&stub).failed_checks(&pr()).unwrap().failed.is_empty());
}

#[test]
fn a_pr_of_another_repo_reaches_no_gh_call() {
    let stub = Stub::new();
    let other = PrRef {
        repo: "someone/else".to_string(),
        number: 7,
    };
    assert!(forge(&stub).failed_checks(&other).is_err());
    assert!(forge(&stub).rerun_failed_checks(&other, SHA).is_err());
    assert!(
        forge(&stub)
            .tree("someone/else", SHA)
            .is_err_and(|e| e.0.contains("someone/else"))
    );
    assert!(forge(&stub).branch_tip("someone/else", "main").is_err());
    assert!(stub.calls().is_empty(), "{:?}", stub.calls());
}

#[test]
fn unreadable_permissions_and_rollups_are_forge_errors() {
    for (perms, view) in [
        ("not json".to_string(), failed_view_json(SHA, vec![])),
        (permissions(true), "{".to_string()),
        // A head that is not a full object name.
        (permissions(true), failed_view_json("--admin", vec![])),
        // A failed check with no name.
        (
            permissions(true),
            failed_view_json(
                SHA,
                vec![
                    json!({"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"}),
                ],
            ),
        ),
    ] {
        let stub = Stub::new();
        stub.out("api-repo", &perms);
        stub.out("pr-view", &view);
        assert!(forge(&stub).failed_checks(&pr()).is_err(), "{perms} {view}");
    }
    let stub = Stub::new();
    stub.answer("api-repo", "", "HTTP 404: Not Found\n", 1);
    assert!(forge(&stub).failed_checks(&pr()).is_err());
}

// ------------------------------------------------------------------ rerun_failed_checks

#[test]
fn the_rerun_reads_the_head_runs_each_workflow_run_once_and_reads_again() {
    let stub = Stub::new();
    // Read 1 is the head read before the re-run. Read 2 shows the failed checks gone.
    stub.out("pr-view", &failed_view_json(SHA, mixed_rollup()));
    stub.answer_nth(
        "pr-view",
        2,
        &failed_view_json(SHA, vec![running("IN_PROGRESS")]),
        "",
        0,
    );
    let effect = forge(&stub).rerun_failed_checks(&pr(), SHA).unwrap();
    assert_eq!(effect.repo, REPO);
    assert_eq!(effect.number, 7);
    assert_eq!(effect.sha, SHA);
    assert_eq!(effect.head_moved, None);
    let mut ran = effect.ran.clone();
    ran.sort();
    assert_eq!(ran, ["build", "e2e", "lint"]);
    let mut not_ran: Vec<&str> = effect.not_ran.iter().map(|(n, _)| n.as_str()).collect();
    not_ran.sort_unstable();
    assert_eq!(not_ran, ["ci/legacy", "scan"]);

    // One call per workflow run, never `--debug`.
    assert_eq!(
        stub.rerun_calls(),
        vec![
            vec!["run", "rerun", "11", "--failed", "-R", REPO],
            vec!["run", "rerun", "12", "--failed", "-R", REPO],
        ]
    );
    let all = stub.calls();
    assert_eq!(all.len(), 4, "{all:?}");
    assert_eq!(all[0][0..2], ["pr", "view"]);
    assert_eq!(all[3][0..2], ["pr", "view"]);
    assert!(all.iter().flatten().all(|a| a != "--debug"));
}

#[test]
fn a_moved_head_runs_nothing_and_names_the_head_it_found() {
    let stub = Stub::new();
    stub.out("pr-view", &failed_view_json(OTHER_SHA, mixed_rollup()));
    let effect = forge(&stub).rerun_failed_checks(&pr(), SHA).unwrap();
    assert_eq!(effect.head_moved.as_deref(), Some(OTHER_SHA));
    assert_eq!(effect.sha, OTHER_SHA);
    assert!(effect.ran.is_empty() && effect.not_ran.is_empty());
    assert!(stub.rerun_calls().is_empty(), "{:?}", stub.calls());
    assert_eq!(stub.calls().len(), 1);
}

#[test]
fn a_check_that_still_reads_failed_after_the_reads_counts_as_not_run() {
    let stub = Stub::new();
    stub.out(
        "pr-view",
        &failed_view_json(SHA, vec![actions_run("build", "FAILURE", 11)]),
    );
    let effect = forge(&stub).rerun_failed_checks(&pr(), SHA).unwrap();
    assert!(effect.ran.is_empty(), "{effect:?}");
    assert_eq!(effect.not_ran.len(), 1);
    assert_eq!(effect.not_ran[0].0, "build");
    assert!(effect.not_ran[0].1.contains("still reads failed"));
    // One head read and at most three reads after the re-run.
    assert_eq!(stub.calls_of("pr", "view").len(), 4);
    assert_eq!(stub.rerun_calls().len(), 1);
}

#[test]
fn a_check_that_clears_on_the_second_read_stops_the_reads() {
    let stub = Stub::new();
    stub.out(
        "pr-view",
        &failed_view_json(SHA, vec![actions_run("build", "FAILURE", 11)]),
    );
    stub.answer_nth(
        "pr-view",
        3,
        &failed_view_json(SHA, vec![running("QUEUED")]),
        "",
        0,
    );
    let effect = forge(&stub).rerun_failed_checks(&pr(), SHA).unwrap();
    assert_eq!(effect.ran, ["build"]);
    assert!(effect.not_ran.is_empty());
    assert_eq!(stub.calls_of("pr", "view").len(), 3);
}

#[test]
fn a_workflow_run_that_gh_refuses_is_not_run_and_the_others_still_run() {
    let stub = Stub::new();
    stub.out("pr-view", &failed_view_json(SHA, mixed_rollup()));
    stub.answer_nth(
        "pr-view",
        2,
        &failed_view_json(SHA, vec![running("QUEUED")]),
        "",
        0,
    );
    stub.answer_nth("run", 1, "", "run 11 cannot be rerun; it is running\n", 1);
    let effect = forge(&stub).rerun_failed_checks(&pr(), SHA).unwrap();
    assert_eq!(effect.ran, ["e2e"]);
    for name in ["build", "lint"] {
        let why = &effect.not_ran.iter().find(|(n, _)| n == name).unwrap().1;
        assert!(why.contains("cannot be rerun"), "{why}");
    }
    assert_eq!(stub.rerun_calls().len(), 2);
}

#[test]
fn a_token_in_stderr_is_absent_from_the_reason() {
    let stub = Stub::new();
    stub.out(
        "pr-view",
        &failed_view_json(SHA, vec![actions_run("build", "FAILURE", 11)]),
    );
    stub.answer(
        "run",
        "",
        "bad credentials ghp_abcdefghijklmnopqrstuvwxyz0123456789\n",
        1,
    );
    let effect = forge(&stub).rerun_failed_checks(&pr(), SHA).unwrap();
    let why = &effect.not_ran[0].1;
    assert!(
        !why.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "{why}"
    );
}

#[test]
fn a_read_that_fails_is_a_forge_error_with_no_rerun() {
    let stub = Stub::new();
    stub.answer("pr-view", "", "could not resolve to a PullRequest\n", 1);
    assert!(forge(&stub).rerun_failed_checks(&pr(), SHA).is_err());
    assert!(stub.rerun_calls().is_empty());
}

#[test]
fn an_expected_sha_that_is_not_a_full_object_name_is_an_error_with_no_call() {
    let stub = Stub::new();
    for bad in [
        "",
        "--debug",
        "abc",
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    ] {
        assert!(
            forge(&stub).rerun_failed_checks(&pr(), bad).is_err(),
            "{bad:?}"
        );
    }
    assert!(stub.calls().is_empty());
}

#[test]
fn a_run_id_is_taken_from_an_actions_url_of_this_repo_only() {
    let stub = Stub::new();
    stub.out("api-repo", &permissions(true));
    let url = |u: &str| {
        json!({"__typename": "CheckRun", "name": "x", "status": "COMPLETED",
               "conclusion": "FAILURE", "detailsUrl": u})
    };
    for (u, rerunnable) in [
        (
            "https://github.com/synthetic/repo/actions/runs/5/job/9",
            true,
        ),
        ("https://github.com/synthetic/repo/actions/runs/5", true),
        ("https://github.com/other/repo/actions/runs/5/job/9", false),
        ("https://github.com/synthetic/repo/actions/runs/", false),
        ("https://github.com/synthetic/repo/actions/runs/5x", false),
        (
            "https://github.com/synthetic/repo/actions/runs/--debug",
            false,
        ),
        (
            "https://github.com.evil.invalid/synthetic/repo/actions/runs/5",
            false,
        ),
    ] {
        stub.out("pr-view", &failed_view_json(SHA, vec![url(u)]));
        let got = forge(&stub).failed_checks(&pr()).unwrap();
        assert_eq!(got.failed[0].blocked.is_none(), rerunnable, "{u}");
    }
}

// ------------------------------------------------------------------- tree and branch_tip

fn tree_json(truncated: bool) -> String {
    json!({
        "sha": SHA,
        "truncated": truncated,
        "tree": [
            {"path": "src", "mode": "040000", "type": "tree", "sha": OTHER_SHA},
            {"path": "src/a.rs", "mode": "100644", "type": "blob", "sha": MERGE_SHA},
            {"path": "README.md", "mode": "100644", "type": "blob", "sha": OTHER_SHA},
        ],
    })
    .to_string()
}

#[test]
fn the_tree_call_asks_for_the_commit_recursively_and_lists_the_files() {
    let stub = Stub::new();
    stub.out("api-tree", &tree_json(false));
    let files = forge(&stub).tree(REPO, SHA).unwrap();
    assert_eq!(
        stub.calls(),
        vec![vec![
            "api".to_string(),
            "--method".to_string(),
            "GET".to_string(),
            format!("repos/synthetic/repo/git/trees/{SHA}?recursive=1"),
        ]]
    );
    assert_eq!(files.len(), 2, "a tree entry is not a file: {files:?}");
    assert_eq!(files["src/a.rs"], MERGE_SHA);
    assert_eq!(files["README.md"], OTHER_SHA);
}

#[test]
fn a_truncated_tree_is_an_error() {
    let stub = Stub::new();
    stub.out("api-tree", &tree_json(true));
    let err = forge(&stub).tree(REPO, SHA).unwrap_err();
    assert!(err.0.contains("too large"), "{err}");
}

#[test]
fn an_unreadable_tree_is_an_error() {
    for text in [
        "",
        "{}",
        "[]",
        r#"{"tree":[]}"#,
        r#"{"truncated":false,"tree":[{"path":"a","type":"blob","sha":"zz"}]}"#,
        r#"{"truncated":false,"tree":[{"path":"a","type":"weird","sha":"0123456789abcdef0123456789abcdef01234567"}]}"#,
    ] {
        let stub = Stub::new();
        stub.out("api-tree", text);
        assert!(forge(&stub).tree(REPO, SHA).is_err(), "{text:?}");
    }
}

#[test]
fn a_sha_that_is_not_a_full_object_name_reaches_no_tree_call() {
    let stub = Stub::new();
    for bad in ["", "--method", "main", "abc", "0123456789abcdef/../x"] {
        assert!(forge(&stub).tree(REPO, bad).is_err(), "{bad:?}");
    }
    assert!(stub.calls().is_empty());
}

#[test]
fn the_tip_call_reads_the_ref_of_the_branch() {
    let stub = Stub::new();
    stub.out(
        "api-ref",
        &json!({"ref": "refs/heads/spine/r0", "object": {"sha": MERGE_SHA, "type": "commit"}})
            .to_string(),
    );
    let tip = forge(&stub).branch_tip(REPO, "spine/r0").unwrap();
    assert_eq!(tip, MERGE_SHA);
    assert_eq!(
        stub.calls(),
        vec![vec![
            "api",
            "--method",
            "GET",
            "repos/synthetic/repo/git/ref/heads/spine/r0"
        ]]
    );
}

#[test]
fn a_branch_name_that_is_not_plain_reaches_no_tip_call() {
    let stub = Stub::new();
    for bad in [
        "", "-x", "/x", "a..b", "a//b", "a b", "a;b", "$(x)", "a?b", "a/",
    ] {
        assert!(forge(&stub).branch_tip(REPO, bad).is_err(), "{bad:?}");
    }
    assert!(stub.calls().is_empty());
}

#[test]
fn an_unreadable_tip_is_an_error() {
    for text in [
        "",
        "{}",
        r#"{"object":{"sha":"zz"}}"#,
        r#"{"object":{"sha":"--admin"}}"#,
    ] {
        let stub = Stub::new();
        stub.out("api-ref", text);
        assert!(forge(&stub).branch_tip(REPO, "main").is_err(), "{text:?}");
    }
}

// ----------------------------------------------------------------------------- argv hygiene

#[test]
fn no_argv_holds_a_debug_flag_a_shell_or_an_unchecked_sha() {
    let stub = Stub::new();
    stub.out("api-repo", &permissions(true));
    stub.out("pr-view", &failed_view_json(SHA, mixed_rollup()));
    stub.out("api-tree", &tree_json(false));
    stub.out("api-ref", &json!({"object": {"sha": SHA}}).to_string());
    let f = forge(&stub);
    let _ = f.failed_checks(&pr());
    let _ = f.rerun_failed_checks(&pr(), SHA);
    let _ = f.tree(REPO, SHA);
    let _ = f.branch_tip(REPO, "work/u1");
    let calls = stub.calls();
    assert!(calls.len() >= 6);
    for call in calls {
        for arg in &call {
            assert_ne!(arg, "--debug", "{call:?}");
            assert!(!["sh", "bash", "-c"].contains(&arg.as_str()), "{call:?}");
        }
        // The program is always `gh` and the first word is a plain command.
        assert!(["api", "pr", "run"].contains(&call[0].as_str()), "{call:?}");
    }
}
