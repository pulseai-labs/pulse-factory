//! AC-1: for scripted `gh` output, the forge maps the checks and the mergeable state to the port's
//! values. An empty rollup is pending; one running check is pending; one failure, one cancel and
//! one timeout are each red; all passed is green; neutral beside a success is green; only neutral
//! is pending. `CONFLICTING` is a conflict, and `UNKNOWN` is pending. `base`, `branch` and `sha`
//! come from the output.

mod common;

use common::*;
use pf_engine::port::forge::{Checks, ForgePort, Mergeable, PrRef};

fn pr() -> PrRef {
    PrRef {
        repo: REPO.to_string(),
        number: 7,
    }
}

fn checks_of(rollup: Vec<serde_json::Value>) -> Checks {
    let stub = Stub::new();
    stub.out("pr-view", &view_json("MERGEABLE", rollup));
    stub.forge().pr_status(&pr()).unwrap().checks
}

#[test]
fn an_empty_rollup_is_pending() {
    assert_eq!(checks_of(vec![]), Checks::Pending);
}

#[test]
fn a_missing_rollup_is_pending_not_green() {
    let stub = Stub::new();
    stub.out(
        "pr-view",
        &format!(
            r#"{{"headRefName":"b","headRefOid":"{SHA}","baseRefName":"main","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":null}}"#
        ),
    );
    assert_eq!(
        stub.forge().pr_status(&pr()).unwrap().checks,
        Checks::Pending
    );
}

#[test]
fn a_running_check_is_pending() {
    for status in ["IN_PROGRESS", "QUEUED", "PENDING", "WAITING"] {
        assert_eq!(
            checks_of(vec![running(status)]),
            Checks::Pending,
            "{status}"
        );
    }
    // A running check beside a success is still pending.
    assert_eq!(
        checks_of(vec![run("SUCCESS"), running("IN_PROGRESS")]),
        Checks::Pending
    );
}

#[test]
fn a_failure_a_cancel_and_a_timeout_are_each_red() {
    for conclusion in ["FAILURE", "CANCELLED", "TIMED_OUT"] {
        assert_eq!(
            checks_of(vec![run(conclusion)]),
            Checks::Red,
            "{conclusion}"
        );
        // Red wins over a success and over a check that still runs.
        assert_eq!(
            checks_of(vec![
                run("SUCCESS"),
                run(conclusion),
                running("IN_PROGRESS")
            ]),
            Checks::Red,
            "{conclusion} beside others"
        );
    }
}

#[test]
fn a_failed_commit_status_is_red_and_a_pending_one_is_pending() {
    assert_eq!(checks_of(vec![context("FAILURE")]), Checks::Red);
    assert_eq!(checks_of(vec![context("ERROR")]), Checks::Red);
    assert_eq!(checks_of(vec![context("PENDING")]), Checks::Pending);
    assert_eq!(
        checks_of(vec![context("SUCCESS"), run("SUCCESS")]),
        Checks::Green
    );
}

#[test]
fn all_passed_is_green() {
    assert_eq!(checks_of(vec![run("SUCCESS")]), Checks::Green);
    assert_eq!(
        checks_of(vec![run("SUCCESS"), run("SUCCESS")]),
        Checks::Green
    );
}

#[test]
fn neutral_beside_a_success_is_green_and_only_neutral_is_pending() {
    assert_eq!(
        checks_of(vec![run("SUCCESS"), run("NEUTRAL")]),
        Checks::Green
    );
    assert_eq!(
        checks_of(vec![run("SKIPPED"), run("SUCCESS")]),
        Checks::Green
    );
    assert_eq!(checks_of(vec![run("NEUTRAL")]), Checks::Pending);
    assert_eq!(
        checks_of(vec![run("SKIPPED"), run("NEUTRAL")]),
        Checks::Pending
    );
}

#[test]
fn an_unknown_conclusion_is_an_error_not_green() {
    let stub = Stub::new();
    stub.out("pr-view", &view_json("MERGEABLE", vec![run("SPLENDID")]));
    assert!(stub.forge().pr_status(&pr()).is_err());
}

#[test]
fn conflicting_is_a_conflict_and_unknown_is_pending() {
    let stub = Stub::new();
    stub.out("pr-view", &view_json("CONFLICTING", vec![run("SUCCESS")]));
    let s = stub.forge().pr_status(&pr()).unwrap();
    assert_eq!(s.mergeable, Mergeable::Conflict);
    assert_eq!(s.checks, Checks::Green);

    // GitHub is still computing: never clean, never a conflict. The checks read as pending.
    let stub = Stub::new();
    stub.out("pr-view", &view_json("UNKNOWN", vec![run("SUCCESS")]));
    let s = stub.forge().pr_status(&pr()).unwrap();
    assert_eq!(s.checks, Checks::Pending);
    assert_ne!(s.mergeable, Mergeable::Conflict);

    // Red stays red while the mergeable state is still unknown.
    let stub = Stub::new();
    stub.out("pr-view", &view_json("UNKNOWN", vec![run("FAILURE")]));
    assert_eq!(stub.forge().pr_status(&pr()).unwrap().checks, Checks::Red);

    let stub = Stub::new();
    stub.out("pr-view", &view_json("MERGEABLE", vec![run("SUCCESS")]));
    let s = stub.forge().pr_status(&pr()).unwrap();
    assert_eq!((s.checks, s.mergeable), (Checks::Green, Mergeable::Clean));
}

#[test]
fn base_branch_and_sha_come_from_the_output() {
    let stub = Stub::new();
    stub.out("pr-view", &view_json("MERGEABLE", vec![run("SUCCESS")]));
    let s = stub.forge().pr_status(&pr()).unwrap();
    assert_eq!(s.base, "spine/r0");
    assert_eq!(s.branch, "work/u1");
    assert_eq!(s.sha, SHA);
    assert_eq!(s.repo, REPO);
}

#[test]
fn the_view_call_is_an_argv_with_the_json_fields_and_the_repo() {
    let stub = Stub::new();
    stub.out("pr-view", &view_json("MERGEABLE", vec![run("SUCCESS")]));
    stub.forge().pr_status(&pr()).unwrap();
    let calls = stub.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0],
        [
            "pr",
            "view",
            "7",
            "-R",
            REPO,
            "--json",
            "headRefName,headRefOid,baseRefName,mergeable,mergeStateStatus,statusCheckRollup"
        ]
    );
}

#[test]
fn a_pr_of_another_repo_is_a_forge_error_and_reaches_no_gh_call() {
    let stub = Stub::new();
    let other = PrRef {
        repo: "someone/else".to_string(),
        number: 7,
    };
    assert!(stub.forge().pr_status(&other).is_err());
    assert!(stub.calls().is_empty());
}
