//! AC-9 of `r2.s3.w3`: the project's Orchestrator sends a unit at a merge conflict to the track's
//! conflict stage with `resolve_conflict` (`docs/contracts/requests.md`, "Settle"; ADR-0004,
//! ADR-0005, amended). The conflict is real: the bare forge's `merge-tree` reads it. The seat is
//! the test: it merges the base into the branch, or rebases the branch onto it, and pushes. When the
//! stage is done the controller compares the paths the resolution changed with the track's
//! `guarded` globs, and the unit goes on to the track's `then` stage, `review`, or parks above the
//! line with `resolution_changed_guarded`.
//!
//! The controller runs in the test process on the fake adapter, with a recording chat. The package
//! is the fixture track with the conflict stage `resolve` and the review loop added.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use std::process::Command;

use common::chat::*;
use common::*;
use pf_adapter_fake::{Scenario, Step};
use pf_engine::admission::PushOutcome;
use pf_forge_bare::PrChecks;
use serde_json::Value;
use settle_rig::*;

fn options() -> Options {
    Options {
        review_loop: true,
        conflict_stage: true,
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    }
}

/// Try a git command in the seat's clone. A merge or a rebase that stops on a conflict is not an
/// error here: the seat resolves it.
fn git_try(dir: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_EDITOR", "true")
        .output()
        .expect("git")
        .status
        .success()
}

/// A rig with unit `u1` parked at its merge on a real merge conflict of PR 5: the PR changed
/// `shared.txt`, and the base changed `shared.txt` and `tests/a.txt` after. Returns the head the PR
/// had when it parked.
fn conflicted() -> (SRig, Forge, String) {
    let rig = SRig::build(&[], Some(20), Link::Own, None, options(), None, |_| {});
    let forge = Forge::new(&rig);
    forge.commit("work/u1", SPINE_BRANCH, "shared.txt", "pr\n");
    forge
        .forge
        .write_pr(5, "work/u1", SPINE_BRANCH, PrChecks::Green)
        .unwrap();
    let head = forge.head(&rig, "work/u1");
    rig.live.ports.fake.script(
        "u1",
        Scenario::Script(vec![
            Step::Push {
                branch: "work/u1".to_string(),
                sha: head.clone(),
                outcome: PushOutcome::Pushed,
            },
            cost(100),
            Step::TurnEnd,
        ]),
    );
    rig.live.queue_item("u1", 1);
    forge.commit(SPINE_BRANCH, SPINE_BRANCH, "shared.txt", "base\n");
    forge.commit(SPINE_BRANCH, SPINE_BRANCH, "tests/a.txt", "a2\n");
    forge.finish_item(&rig, "u1", 5);
    // The first review passes on the PR's head, and the unit meets its merge.
    std::fs::write(
        rig.out("u1", "review.md"),
        format!("verdict: pass\nhead: {head}\n"),
    )
    .unwrap();
    rig.live.ports.fake.finish(&rig.session_of("u1"));
    rig.tick();
    assert_eq!(
        rig.status_of("u1"),
        "parked",
        "{:?}\n{}",
        rig.stage_nodes("u1"),
        std::fs::read_to_string(&rig.live.handle().info.ledger).unwrap_or_default()
    );
    (rig, forge, head)
}

fn cause_of(rig: &SRig, request: &str) -> String {
    let all = cli_json(&rig.live, &["requests", "--all"]);
    all["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == request)
        .unwrap_or_else(|| panic!("no request {request}"))["cause"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Settle the conflict and let the `resolve` stage launch. The seat then does nothing but end.
fn send_to_resolve(rig: &SRig, id: &str, rev: u32) {
    // The conflict stage's seat pushes nothing by itself: the test plays it.
    rig.live
        .ports
        .fake
        .script("u1", Scenario::Script(vec![Step::TurnEnd]));
    let resp = rig.settle(id, rev, "resolve_conflict", None, WHY);
    done(&resp);
    rig.tick();
    assert_eq!(
        rig.stage_nodes("u1").last().unwrap(),
        &("resolve".to_string(), "running".to_string()),
        "{:?}\n{}",
        rig.stage_nodes("u1"),
        std::fs::read_to_string(&rig.live.handle().info.ledger).unwrap_or_default()
    );
}

/// The seat merges the base into the branch, resolves `shared.txt`, writes `extra` too, and pushes.
fn resolve_by_merge(forge: &Forge, extra: Option<(&str, &str)>) {
    git_try(&forge.work, &["fetch", "--quiet", "origin"]);
    assert!(git_try(
        &forge.work,
        &["checkout", "--quiet", "-B", "work/u1", "origin/work/u1"]
    ));
    assert!(
        !git_try(&forge.work, &["merge", "--no-edit", "origin/spine/s1"]),
        "the merge must stop on the conflict"
    );
    std::fs::write(forge.work.join("shared.txt"), "merged\n").unwrap();
    if let Some((file, text)) = extra {
        std::fs::write(forge.work.join(file), text).unwrap();
    }
    assert!(git_try(&forge.work, &["add", "-A"]));
    assert!(git_try(
        &forge.work,
        &["commit", "--quiet", "-m", "resolve"]
    ));
    assert!(git_try(
        &forge.work,
        &["push", "--quiet", "origin", "work/u1"]
    ));
}

/// The seat rebases the branch onto the base, resolves, and writes the branch to the bare repo.
fn resolve_by_rebase(forge: &Forge, extra: Option<(&str, &str)>) {
    git_try(&forge.work, &["fetch", "--quiet", "origin"]);
    assert!(git_try(
        &forge.work,
        &["checkout", "--quiet", "-B", "work/u1", "origin/work/u1"]
    ));
    assert!(
        !git_try(&forge.work, &["rebase", "origin/spine/s1"]),
        "the rebase must stop on the conflict"
    );
    std::fs::write(forge.work.join("shared.txt"), "merged\n").unwrap();
    if let Some((file, text)) = extra {
        std::fs::write(forge.work.join(file), text).unwrap();
    }
    assert!(git_try(&forge.work, &["add", "-A"]));
    assert!(git_try(&forge.work, &["rebase", "--continue"]));
    if extra.is_some() {
        // The extra file joins the rebased commit.
        assert!(git_try(&forge.work, &["add", "-A"]));
        git_try(&forge.work, &["commit", "--quiet", "--amend", "--no-edit"]);
    }
    assert!(git_try(
        &forge.work,
        &["push", "--quiet", "--force", "origin", "work/u1"]
    ));
}

/// The resolve stage is done: its done file, the end of its turn, one tick.
fn finish_resolve(rig: &SRig) {
    rig.pass("u1", &rig.out("u1", "resolve-done"));
}

fn open_cause(rig: &SRig) -> (String, String) {
    let (id, _) = rig.request("u1");
    let cause = cause_of(rig, &id);
    (id, cause)
}

#[test]
fn a_conflict_is_settled_with_resolve_conflict_and_the_resolution_is_reviewed_before_the_merge() {
    let (rig, forge, head) = conflicted();
    let (id, rev) = rig.request("u1");
    assert_eq!(cause_of(&rig, &id), "merge_conflict");

    send_to_resolve(&rig, &id, rev);
    // The stored answer holds the PR's head, read before the commit.
    assert_eq!(rig.answer_of(&id)["head_sha"], head.as_str());
    assert!(rig.results().is_empty(), "resolve_conflict has no result");

    resolve_by_merge(&forge, None);
    finish_resolve(&rig);

    // `resolve` is done, and `review` launches. The merge rule does not run before it.
    rig.tick();
    let nodes = rig.stage_nodes("u1");
    assert_eq!(
        nodes.last().unwrap(),
        &("review".to_string(), "running".to_string()),
        "{nodes:?}"
    );
    assert_eq!(rig.status_of("u1"), "running");
    let merged = git(
        &rig.live.bare,
        &["rev-parse", &format!("refs/heads/{SPINE_BRANCH}")],
    );
    let head_now = forge.head(&rig, "work/u1");
    assert_ne!(merged, head_now, "the PR is not merged");
    assert!(
        rig.run_ledger("settle")[0].contains(&format!("act=resolve_conflict head={head}")),
        "{:#?}",
        rig.run_ledger("settle")
    );
}

#[test]
fn a_resolution_that_only_takes_the_bases_change_to_a_test_file_does_not_park() {
    let (rig, forge, _) = conflicted();
    let (id, rev) = rig.request("u1");
    send_to_resolve(&rig, &id, rev);
    // The merge brings the base's `tests/a.txt` as it is: the resolution wrote nothing there.
    resolve_by_merge(&forge, None);
    finish_resolve(&rig);
    rig.tick();
    assert_eq!(rig.status_of("u1"), "running");
    assert!(
        rig.stage_nodes("u1").iter().any(|(s, _)| s == "review"),
        "{:?}",
        rig.stage_nodes("u1")
    );
}

#[test]
fn a_resolution_that_writes_its_own_change_to_a_test_file_parks_above_the_line() {
    for rebased in [false, true] {
        let (rig, forge, _) = conflicted();
        let (id, rev) = rig.request("u1");
        send_to_resolve(&rig, &id, rev);
        let extra = Some(("tests/a.txt", "mine\n"));
        if rebased {
            resolve_by_rebase(&forge, extra);
        } else {
            resolve_by_merge(&forge, extra);
        }
        finish_resolve(&rig);
        assert_eq!(rig.status_of("u1"), "parked", "rebased: {rebased}");
        let (id2, cause) = open_cause(&rig);
        assert_ne!(id2, id);
        assert_eq!(cause, "resolution_changed_guarded", "rebased: {rebased}");
        let all = cli_json(&rig.live, &["requests", "--all"]);
        let row = all["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id2)
            .unwrap()
            .clone();
        assert_eq!(row["line"], "above", "{row}");
        // An `above` request refuses every act of the grant.
        let (open, open_rev) = rig.request("u1");
        let text = why(&rig.settle(&open, open_rev, "clear", None, WHY));
        assert!(
            text.contains(&format!(
                "request {open} is above the line: only the claim may settle it"
            )),
            "{text}"
        );
        // The request names the guarded path.
        let shown: Value = rig.request_json(&open);
        assert!(shown.to_string().contains("tests/a.txt"), "{shown}");
    }
}

#[test]
fn a_forge_read_that_fails_parks_the_unit_too() {
    let (rig, forge, _) = conflicted();
    let (id, rev) = rig.request("u1");
    send_to_resolve(&rig, &id, rev);
    resolve_by_merge(&forge, None);
    // The PR cannot be read when the stage ends.
    let pr_file = forge.forge.reruns_path(5).with_file_name("5.json");
    std::fs::remove_file(&pr_file).unwrap();
    finish_resolve(&rig);
    assert_eq!(rig.status_of("u1"), "parked");
    let (_, cause) = open_cause(&rig);
    assert_eq!(cause, "resolution_changed_guarded");
}

#[test]
fn the_compare_uses_the_head_the_settle_stored_not_the_head_at_the_end() {
    let (rig, forge, head) = conflicted();
    let (id, rev) = rig.request("u1");
    send_to_resolve(&rig, &id, rev);
    // The branch moves again before the stage is done (the seat pushed twice). The old side of the
    // compare is still the stored head, so the test file the first push wrote is a change of the
    // resolution, and the unit parks.
    resolve_by_merge(&forge, None);
    forge.commit("work/u1", "work/u1", "tests/a.txt", "mine\n");
    assert_eq!(rig.answer_of(&id)["head_sha"], head.as_str());
    finish_resolve(&rig);
    assert_eq!(rig.status_of("u1"), "parked");
    let (_, cause) = open_cause(&rig);
    assert_eq!(cause, "resolution_changed_guarded");
}
