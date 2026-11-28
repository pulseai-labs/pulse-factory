//! AC-8 of `r2.s3.w3`: the project's Orchestrator settles a red check with `rerun_check`
//! (`docs/contracts/requests.md`, "Settle"; ADR-0005, ADR-0016, amended). The act commits first and
//! then asks the forge to run the PR's failed checks again, on the head the settle stored. The
//! forge is the bare forge of the paper env: a PR is a file, `prs/<n>.reruns` counts the runs.
//!
//! The controller runs in the test process on the fake adapter, with a recording chat. A caller is
//! this test binary as a helper below a shell whose PID the test registers as the chat's process.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use common::*;
use pf_api::types::SettleAnswer;
use pf_forge_bare::{FailedSpec, PrChecks};
use serde_json::Value;
use settle_rig::*;

const KEY: &str = "authority.rerun_checks_per_head";

fn answer_of(resp: &Value) -> SettleAnswer {
    serde_json::from_value(done(resp)).expect("the answer decodes as SettleAnswer")
}

/// A rig with unit `u1` parked at its merge on a red check of PR 5. Returns the rig, the forge and
/// the PR's head.
fn red() -> (SRig, Forge, String) {
    let rig = SRig::new(&[], Some(20));
    let forge = Forge::new(&rig);
    forge.item(&rig, "u1", 1, 5, PrChecks::Red);
    forge.finish_item(&rig, "u1", 5);
    let head = forge.head(&rig, "work/u1");
    (rig, forge, head)
}

fn reruns(forge: &Forge) -> Vec<String> {
    std::fs::read_to_string(forge.forge.reruns_path(5))
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The unit's PR goes red again on the same head, and the merge step parks it.
fn red_again(rig: &SRig, forge: &Forge) {
    forge
        .forge
        .write_pr(5, "work/u1", SPINE_BRANCH, PrChecks::Red)
        .unwrap();
    rig.tick();
}

fn set_limit(rig: &SRig, value: &str) {
    let out = rig
        .live
        .cli(&["settings", "set", KEY, value, "--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn a_red_check_is_run_again_on_the_stored_head_and_the_unit_merges_when_green() {
    let (rig, forge, head) = red();
    let (id, rev) = rig.request("u1");
    assert_eq!(rig.status_of("u1"), "parked");

    let resp = rig.settle(&id, rev, "rerun_check", None, WHY);
    let answer = answer_of(&resp);
    assert!(!answer.replayed);
    assert_eq!(answer.settled.act, "rerun_check");

    // The stored answer holds the head the engine read before the commit.
    assert_eq!(rig.answer_of(&id)["head_sha"], head.as_str());
    // The settle line has the head; the result line has the repo, the PR and the SHA.
    let settle = rig.run_ledger("settle");
    assert!(
        settle[0].contains(&format!("act=rerun_check head={head}")),
        "{settle:#?}"
    );
    let result = rig.run_ledger("settle-result");
    assert_eq!(result.len(), 1, "{result:#?}");
    assert!(
        result[0].contains("rerun_check ran")
            && result[0].contains(&format!("repo={}", forge.forge.repo()))
            && result[0].contains("pr=5")
            && result[0].contains(&format!("sha={head}"))
            && result[0].contains("checks=checks"),
        "{result:#?}"
    );
    // The forge ran the checks again once, on that head.
    let lines = reruns(&forge);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains(&head), "{lines:?}");
    let results = rig.results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, format!("settle-result:{id}:{rev}"));
    assert_eq!(results[0].1["outcome"], "ran");

    // The unit runs on at its merge. The checks are pending, so it waits; green, it merges.
    assert_eq!(rig.status_of("u1"), "running");
    rig.tick();
    assert_eq!(rig.status_of("u1"), "running", "pending checks: it waits");
    forge
        .forge
        .write_pr(5, "work/u1", SPINE_BRANCH, PrChecks::Green)
        .unwrap();
    rig.tick();
    assert_eq!(rig.status_of("u1"), "done");
}

#[test]
fn a_second_red_park_on_the_same_head_is_refused_until_the_limit_is_raised() {
    let (rig, forge, head) = red();
    let (id, rev) = rig.request("u1");
    done(&rig.settle(&id, rev, "rerun_check", None, WHY));
    red_again(&rig, &forge);
    let (id2, rev2) = rig.request("u1");
    assert_ne!(id2, id, "a new park is a new request");

    let text = why(&rig.settle(&id2, rev2, "rerun_check", None, WHY));
    assert!(
        text.contains(&format!(
            "the act rerun_check was admitted 1 times for head {head} of this unit's PR, and {KEY} is 1"
        )),
        "{text}"
    );
    assert_eq!(reruns(&forge).len(), 1, "no second forge call");
    assert_eq!(rig.admissions("settle"), 1);

    // The limit is read fresh: raised to 2 for the project, the same settle is admitted.
    set_limit(&rig, "2");
    done(&rig.settle(&id2, rev2, "rerun_check", None, WHY));
    assert_eq!(reruns(&forge).len(), 2);
    assert_eq!(rig.admissions("settle"), 2);
    // A third is over the limit again.
    red_again(&rig, &forge);
    let (id3, rev3) = rig.request("u1");
    let text = why(&rig.settle(&id3, rev3, "rerun_check", None, WHY));
    assert!(text.contains("was admitted 2 times"), "{text}");
}

#[test]
fn two_settles_at_the_same_time_are_one_admission_and_one_forge_call() {
    let (rig, forge, _) = red();
    let (id, rev) = rig.request("u1");
    let args = settle_args(&id, rev, "rerun_check", None, WHY);
    let answers = rig.call_twice("settle", &args);
    let replays: Vec<bool> = answers.iter().map(|a| answer_of(a).replayed).collect();
    assert_eq!(
        replays.iter().filter(|r| **r).count(),
        1,
        "one admission and one replay of it: {answers:?}"
    );
    assert_eq!(rig.admissions("settle"), 1);
    assert_eq!(reruns(&forge).len(), 1, "the forge is called once");
    assert_eq!(rig.results().len(), 1);

    // The unit parks red again on the same head: the next settle is refused at the count.
    red_again(&rig, &forge);
    let (id2, rev2) = rig.request("u1");
    let text = why(&rig.settle(&id2, rev2, "rerun_check", None, WHY));
    assert!(text.contains("was admitted 1 times for head"), "{text}");
    assert_eq!(reruns(&forge).len(), 1);
}

#[test]
fn a_failed_check_that_cannot_run_again_is_refused_before_the_commit() {
    let (rig, forge, _) = red();
    forge
        .forge
        .write_pr_with(
            5,
            "work/u1",
            SPINE_BRANCH,
            PrChecks::Red,
            &[
                FailedSpec {
                    name: "build".to_string(),
                    rerunnable: true,
                },
                FailedSpec {
                    name: "scan".to_string(),
                    rerunnable: false,
                },
            ],
            None,
        )
        .unwrap();
    let (id, rev) = rig.request("u1");
    let text = why(&rig.settle(&id, rev, "rerun_check", None, WHY));
    assert!(
        text.contains("the failed check scan of PR")
            && text.contains("cannot be run again by this forge"),
        "{text}"
    );
    assert!(!forge.forge.reruns_path(5).exists(), "no forge re-run");
    assert_eq!(rig.admissions("settle"), 0, "no admission in the run");
    assert_eq!(rig.status_of("u1"), "parked");
    assert!(rig.results().is_empty());
}

#[test]
fn a_track_whose_base_is_the_default_branch_refuses_the_act() {
    let rig = SRig::build(
        &[],
        Some(20),
        Link::Own,
        None,
        Options::default(),
        Some("main"),
        |_| {},
    );
    let forge = Forge::new(&rig);
    // The PR merges into `main`, the forge's default branch.
    forge.commit("work/u1", "main", "u1.txt", "x\n");
    forge
        .forge
        .write_pr(5, "work/u1", "main", PrChecks::Red)
        .unwrap();
    let head = forge.head(&rig, "work/u1");
    rig.live.ports.fake.script(
        "u1",
        pf_adapter_fake::Scenario::Script(vec![
            pf_adapter_fake::Step::Push {
                branch: "work/u1".to_string(),
                sha: head,
                outcome: pf_engine::admission::PushOutcome::Pushed,
            },
            cost(100),
            pf_adapter_fake::Step::TurnEnd,
        ]),
    );
    rig.live.queue_item("u1", 1);
    forge.finish_item(&rig, "u1", 5);
    let (id, rev) = rig.request("u1");
    let text = why(&rig.settle(&id, rev, "rerun_check", None, WHY));
    assert!(
        text.contains(&format!(
            "request {id} is above the line: only the claim may settle it"
        )),
        "{text}"
    );
    assert!(!forge.forge.reruns_path(5).exists());
    assert_eq!(rig.admissions("settle"), 0);
}
