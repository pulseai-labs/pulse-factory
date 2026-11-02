//! AC-5: with a fake forge, a PR whose base is the default branch, a PR for another repo and a head
//! with no recorded push each park the unit, with a reason that names what differs, and no merge
//! call is made. A track with `head_check` false merges with no recorded push. A head that moves
//! between the read and the merge is refused, and the unit parks.

mod common;

use std::collections::BTreeMap;

use common::*;
use pf_engine::Status;
use pf_engine::admission::PushOutcome;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::{ForgeOutcome, PrRef};
use pf_engine::unit::Signal;

const REPO: &str = "synthetic/repo";
const BASE: &str = "spine/r0";

/// One unit `u1` with PR 1, whose head is `sha-u1` on `work/u1` and whose base is `base`.
fn run_with(wf: pf_engine::Workflow, base: &str) -> Env {
    run_with_params(wf, base, BTreeMap::new())
}

fn run_with_params(wf: pf_engine::Workflow, base: &str, params: BTreeMap<String, String>) -> Env {
    let env = env_with_params(wf, params);
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    let mut script = PrScript::green("work/u1", "sha-u1");
    script.base = base.to_string();
    env.forge.script(1, script);
    env
}

fn finish_stage(env: &Env, repo: &str) {
    env.engine.start_stage("u1").unwrap();
    env.engine
        .signal(
            "u1",
            "build",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: repo.to_string(),
                number: 1,
            }),
        )
        .unwrap();
}

fn push(env: &Env, sha: &str) {
    env.engine
        .record_push("u1", "ev-1", REPO, "work/u1", sha, PushOutcome::Pushed)
        .unwrap();
}

/// The unit is parked, its reason holds every part in `names`, and the forge merged nothing.
fn assert_parked(env: &Env, step: MergeStep, names: &[&str]) -> String {
    let MergeStep::Parked { unit, reason } = step else {
        panic!("expected a park, got {step:?}");
    };
    assert_eq!(unit, "u1");
    for part in names {
        assert!(reason.contains(part), "{part:?} missing from {reason:?}");
    }
    assert_eq!(
        env.status_of("u1"),
        (Status::Parked, Some(reason.clone())),
        "the unit holds the reason"
    );
    assert!(env.forge.merged().is_empty(), "no PR was merged");
    assert!(
        env.forge.merge_calls.lock().unwrap().is_empty(),
        "no merge call was made"
    );
    reason
}

// ---------------------------------------------------------------- the base

#[test]
fn a_pr_aimed_at_the_default_branch_parks_the_unit_and_names_both_bases() {
    let env = run_with(guarded_workflow(REPO, BASE, false), "main");
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["main", BASE]);
    // The ledger line holds repo, branch, SHA and the outcome that names both.
    let ledger = env.ledger_text();
    assert!(
        ledger.contains("merge of unit u1: repo=synthetic/repo branch=work/u1 sha=sha-u1 outcome="),
        "{ledger}"
    );
    assert!(ledger.contains("main") && ledger.contains(BASE), "{ledger}");
    // And the unit holds the order: nothing after it merges.
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Waiting { .. }
    ));
}

#[test]
fn a_pr_aimed_at_the_declared_base_merges() {
    let env = run_with(guarded_workflow(REPO, BASE, false), BASE);
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Merged { .. }), "{step:?}");
    assert_eq!(env.forge.merged(), vec![1]);
    assert_eq!(env.status_of("u1").0, Status::Done);
}

// ---------------------------------------------------------------- the repo

#[test]
fn a_pr_for_another_repo_parks_the_unit_and_the_forge_is_not_called() {
    let env = run_with(guarded_workflow(REPO, BASE, false), BASE);
    finish_stage(&env, "attacker/fork");
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["attacker/fork", REPO]);
    assert!(
        env.forge.status_reads.lock().unwrap().is_empty(),
        "a PR of another repo is not even read"
    );
}

#[test]
fn a_forge_that_reports_another_repo_parks_the_unit() {
    let env = run_with(guarded_workflow(REPO, BASE, false), BASE);
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&1)
        .unwrap()
        .reported_repo = Some("other/place".to_string());
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["other/place", REPO]);
}

// ---------------------------------------------------------------- the placeholders

#[test]
fn the_declared_repo_and_base_render_their_placeholders() {
    let mut params = BTreeMap::new();
    params.insert("repo".to_string(), REPO.to_string());
    params.insert("spine_branch".to_string(), BASE.to_string());
    let wf = guarded_workflow("{param.repo}", "{param.spine_branch}", false);

    let env = run_with_params(wf.clone(), BASE, params.clone());
    finish_stage(&env, REPO);
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Merged { .. }
    ));

    // A PR aimed at another base is still caught after rendering.
    let env = run_with_params(wf, "main", params);
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["main", BASE]);
}

#[test]
fn a_declaration_that_cannot_render_parks_the_unit() {
    let wf = guarded_workflow("{param.repo}", BASE, false);
    let env = run_with(wf, BASE);
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["repo"]);
}

// ---------------------------------------------------------------- the head check

#[test]
fn a_head_with_no_recorded_push_parks_the_unit() {
    let env = run_with(guarded_workflow(REPO, BASE, true), BASE);
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["sha-u1", "no recorded push"]);
}

#[test]
fn a_head_that_differs_from_the_recorded_push_parks_the_unit() {
    let env = run_with(guarded_workflow(REPO, BASE, true), BASE);
    finish_stage(&env, REPO);
    push(&env, "sha-other");
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["sha-u1", "no recorded push"]);
}

#[test]
fn a_failed_push_does_not_count_as_a_recorded_push() {
    let env = run_with(guarded_workflow(REPO, BASE, true), BASE);
    finish_stage(&env, REPO);
    env.engine
        .record_push(
            "u1",
            "ev-1",
            REPO,
            "work/u1",
            "sha-u1",
            PushOutcome::Failed("rejected".to_string()),
        )
        .unwrap();
    let step = env.engine.merge_next().unwrap();
    assert_parked(&env, step, &["no recorded push"]);
}

#[test]
fn a_head_that_matches_the_recorded_push_merges() {
    let env = run_with(guarded_workflow(REPO, BASE, true), BASE);
    finish_stage(&env, REPO);
    push(&env, "sha-u1");
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Merged { .. }), "{step:?}");
    assert_eq!(env.forge.merged(), vec![1]);
}

#[test]
fn a_track_with_head_check_false_merges_with_no_recorded_push() {
    let env = run_with(guarded_workflow(REPO, BASE, false), BASE);
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Merged { .. }), "{step:?}");
    assert_eq!(env.forge.merged(), vec![1]);
}

// ---------------------------------------------------------------- the pinned head

#[test]
fn the_merge_call_pins_the_head_the_engine_read() {
    let env = run_with(guarded_workflow(REPO, BASE, false), BASE);
    finish_stage(&env, REPO);
    env.engine.merge_next().unwrap();
    assert_eq!(
        *env.forge.merge_calls.lock().unwrap(),
        vec![(1, "sha-u1".to_string())]
    );
}

#[test]
fn a_head_that_moves_between_the_read_and_the_merge_is_refused_and_the_unit_parks() {
    let env = run_with(guarded_workflow(REPO, BASE, false), BASE);
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&1)
        .unwrap()
        .head_after_read = Some("sha-moved".to_string());
    finish_stage(&env, REPO);
    let step = env.engine.merge_next().unwrap();
    let MergeStep::Parked { reason, .. } = &step else {
        panic!("expected a park, got {step:?}");
    };
    assert!(reason.starts_with("merge refused:"), "{reason}");
    assert!(
        reason.contains("sha-u1") && reason.contains("sha-moved"),
        "{reason}"
    );
    assert_eq!(env.status_of("u1").0, Status::Parked);
    // The forge was asked once, with the head that was read, and merged nothing.
    assert_eq!(
        *env.forge.merge_calls.lock().unwrap(),
        vec![(1, "sha-u1".to_string())]
    );
    assert!(env.forge.merged().is_empty());
    assert!(env.ledger_text().contains(
        "merge of unit u1: repo=synthetic/repo branch=work/u1 sha=sha-moved outcome=merge refused:"
    ));
    // The unit stays parked and holds the order; a second call merges nothing.
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Waiting { .. }
    ));
    assert!(env.forge.merged().is_empty());
}

#[test]
fn a_refusal_with_no_branch_in_the_effect_still_records_the_branch_that_was_read() {
    // A forge may leave the branch of a merge effect empty. The ledger line still holds it.
    let env = run_with(guarded_workflow(REPO, BASE, false), BASE);
    {
        let mut scripts = env.forge.scripts.lock().unwrap();
        let script = scripts.get_mut(&1).unwrap();
        script.merge = ForgeOutcome::Refused("because".to_string());
        script.blank_effect_branch = true;
    }
    finish_stage(&env, REPO);
    env.engine.merge_next().unwrap();
    assert!(env.ledger_text().contains("branch=work/u1"));
}
