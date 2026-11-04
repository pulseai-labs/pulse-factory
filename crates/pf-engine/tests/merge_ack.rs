//! AC-2: the merge rule `claim_ack`. A unit that is ready with green, clean checks parks with a
//! `merge_ack` request that names the PR, its base and its head SHA. The PR merges only when the
//! claim holder acks that request at its current revision, and the merge pins the acked SHA.
//! Every refusal changes no state, and a merge that does not land parks with `merge_failed`.

mod common;

use common::*;
use pf_engine::claim::sha256_hex;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::{Checks, ForgeOutcome, Mergeable, PrRef};
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus};
use pf_engine::unit::Signal;
use pf_engine::{EngineError, MergeRule, StageDef, Status, Workflow};
use serde_json::Value;

const SHA: &str = "1111111111111111111111111111111111111111";
const MOVED: &str = "2222222222222222222222222222222222222222";

fn spine_workflow() -> Workflow {
    let mut wf = Workflow::single_track(
        "spine-flow",
        vec![StageDef::new("spine_close", "close-role", "model-a")],
        Some(MergeRule::ClaimAck),
        pf_engine::capacity::Settings::default(),
    );
    wf.tracks[0].name = "spine".to_string();
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.repo = "synthetic/repo".to_string();
    def.base = "main".to_string();
    def.head_branch = "spine/s1".to_string();
    wf
}

fn green_script(sha: &str) -> PrScript {
    PrScript {
        base: "main".to_string(),
        ..PrScript::green("spine/s1", sha)
    }
}

/// An engine with the unit `spine` done with its one stage, its PR (number 7) named and green.
fn ready() -> Env {
    let env = env(spine_workflow());
    env.engine
        .ingest(&mut TrackedSource {
            track: "spine".to_string(),
            records: vec![unit("spine", "spine", 1)],
        })
        .unwrap();
    env.forge.script(7, green_script(SHA));
    env.engine.start_stage("spine").unwrap();
    env.engine
        .signal(
            "spine",
            "spine_close",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number: 7,
            }),
        )
        .unwrap();
    env
}

/// Take the claim, and return its generation.
fn claim(env: &Env, who: &str) -> u64 {
    env.engine
        .claim(who, &sha256_hex(who.as_bytes()))
        .unwrap()
        .generation
}

fn requests(env: &Env) -> Vec<RequestRow> {
    env.store.all_requests()
}

fn body_of(r: &RequestRow) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

fn rejected(r: Result<MergeStep, EngineError>) -> String {
    match r {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("the ack must be refused, got {other:?}"),
    }
}

/// Open the ack request, and return it.
fn open_ack(env: &Env) -> RequestRow {
    let step = env.engine.merge_next().unwrap();
    assert!(
        matches!(&step, MergeStep::AckRequested { unit, .. } if unit == "spine"),
        "{step:?}"
    );
    let all = requests(env);
    assert_eq!(all.len(), 1, "{all:?}");
    all[0].clone()
}

fn script_mut(env: &Env, f: impl FnOnce(&mut PrScript)) {
    f(env.forge.scripts.lock().unwrap().get_mut(&7).unwrap());
}

#[test]
fn a_ready_unit_with_green_clean_checks_opens_one_merge_ack_request() {
    let env = ready();
    let r = open_ack(&env);
    assert_eq!(r.kind, RequestKind::MergeAck);
    assert_eq!(r.status, RequestStatus::Open);
    assert_eq!(r.revision, 1);
    assert_eq!(r.unit_id.as_deref(), Some("spine"));
    assert_eq!(r.stage, "merge");
    assert_eq!(
        r.reason,
        "spine merge waits on the claim's ack: synthetic/repo#7 spine/s1 -> main at 1111111"
    );
    let body = body_of(&r);
    assert_eq!(body["pr"], "synthetic/repo#7");
    assert_eq!(body["repo"], "synthetic/repo");
    assert_eq!(body["base"], "main");
    assert_eq!(body["head_branch"], "spine/s1");
    assert_eq!(body["head_sha"], SHA);
    assert_eq!(body["stage"], "merge");
    // The unit parks in the same commit, and nothing merged.
    assert_eq!(
        env.status_of("spine"),
        (Status::Parked, Some(r.reason.clone()))
    );
    assert!(env.forge.merged().is_empty());
    assert!(
        env.ledger_text().contains(&format!(
            "merge of unit spine: repo=synthetic/repo branch=spine/s1 sha={SHA} outcome=waits on the claim's ack"
        )),
        "{}",
        env.ledger_text()
    );

    // A replay opens no second request: the unit is parked and holds the order.
    let again = env.engine.merge_next().unwrap();
    assert!(matches!(again, MergeStep::Waiting { .. }), "{again:?}");
    assert_eq!(requests(&env).len(), 1);
    assert!(env.forge.merged().is_empty());
}

#[test]
fn pending_checks_wait_and_open_nothing() {
    let env = ready();
    script_mut(&env, |s| s.checks = Checks::Pending);
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Waiting { .. }), "{step:?}");
    assert!(requests(&env).is_empty());
    assert_eq!(env.status_of("spine"), (Status::Running, None));
    // The checks turn green, and the next call opens the request.
    script_mut(&env, |s| s.checks = Checks::Green);
    open_ack(&env);
}

#[test]
fn red_checks_a_conflict_a_wrong_base_and_a_wrong_head_branch_park_with_merge_failed() {
    type Break = fn(&mut PrScript);
    let cases: [(&str, Break, &str); 4] = [
        ("red", |s| s.checks = Checks::Red, "checks red"),
        (
            "conflict",
            |s| s.mergeable = Mergeable::Conflict,
            "merge conflict",
        ),
        (
            "base",
            |s| s.base = "develop".to_string(),
            "PR base develop differs from the declared base main",
        ),
        (
            "head branch",
            |s| s.branch = "work/other".to_string(),
            "PR head branch work/other differs from the declared head branch spine/s1",
        ),
    ];
    for (name, break_it, outcome) in cases {
        let env = ready();
        script_mut(&env, break_it);
        let step = env.engine.merge_next().unwrap();
        assert!(matches!(step, MergeStep::Parked { .. }), "{name}: {step:?}");
        let all = requests(&env);
        assert_eq!(all.len(), 1, "{name}");
        assert_eq!(all[0].kind, RequestKind::MergeFailed, "{name}");
        assert_eq!(all[0].reason, outcome, "{name}");
        assert_eq!(all[0].stage, "merge", "{name}");
        assert!(env.forge.merged().is_empty(), "{name}");
        assert!(
            env.ledger_text().contains(&format!("outcome={outcome}")),
            "{name}: {}",
            env.ledger_text()
        );
    }
}

#[test]
fn an_ack_at_the_current_revision_merges_with_the_acked_sha_pinned() {
    let env = ready();
    let r = open_ack(&env);
    let generation = claim(&env, "operator");
    let step = env.engine.ack_merge(&r.id, 1, generation).unwrap();
    match &step {
        MergeStep::Merged { unit, effect } => {
            assert_eq!(unit, "spine");
            assert_eq!(effect.sha, SHA);
            assert_eq!(effect.outcome, ForgeOutcome::Landed);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        *env.forge.merge_calls.lock().unwrap(),
        vec![(7, SHA.to_string())],
        "the merge pins the acked SHA"
    );
    assert_eq!(env.forge.merged(), vec![7]);
    assert_eq!(
        env.status_of("spine"),
        (Status::Done, Some(format!("merged at {SHA}")))
    );

    let answered = requests(&env).remove(0);
    assert_eq!(answered.status, RequestStatus::Answered);
    assert_eq!(answered.answer_generation, Some(generation));
    let answer: Value = serde_json::from_str(answered.answer.as_deref().unwrap()).unwrap();
    assert_eq!(answer["verb"], "ack");
    assert_eq!(answer["head_sha"], SHA);

    let ledger = env.ledger_text();
    let ack_line = format!(
        "ack {}@1 by claim {generation}: verb=ack unit=spine pr=synthetic/repo#7 base=main branch=spine/s1 sha={SHA}",
        r.id
    );
    assert!(
        ledger.contains(&ack_line),
        "missing {ack_line:?} in\n{ledger}"
    );
    let merge_line = format!(
        "merge of unit spine: repo=synthetic/repo branch=spine/s1 sha={SHA} outcome=landed"
    );
    assert!(ledger.contains(&merge_line), "{ledger}");
    assert!(ledger.find(&ack_line) < ledger.find(&merge_line));
}

#[test]
fn a_moved_head_revises_the_request_and_refuses_the_ack() {
    let env = ready();
    let r = open_ack(&env);
    let generation = claim(&env, "operator");
    script_mut(&env, |s| s.sha = MOVED.to_string());

    let why = rejected(env.engine.ack_merge(&r.id, 1, generation));
    assert!(why.contains("revision 2"), "{why}");
    assert!(why.contains(MOVED), "{why}");
    assert!(env.forge.merged().is_empty());

    let revised = requests(&env).remove(0);
    assert_eq!(revised.status, RequestStatus::Open);
    assert_eq!(revised.revision, 2);
    assert_eq!(body_of(&revised)["head_sha"], MOVED);
    assert!(revised.reason.ends_with("at 2222222"), "{}", revised.reason);
    assert_eq!(env.status_of("spine").0, Status::Parked);

    // The old revision is stale now.
    let stale = rejected(env.engine.ack_merge(&r.id, 1, generation));
    assert!(stale.contains("revision 2"), "{stale}");
    assert!(env.forge.merged().is_empty());

    // The new revision merges the new SHA.
    let step = env.engine.ack_merge(&r.id, 2, generation).unwrap();
    assert!(matches!(step, MergeStep::Merged { .. }), "{step:?}");
    assert_eq!(
        *env.forge.merge_calls.lock().unwrap(),
        vec![(7, MOVED.to_string())]
    );
    assert!(env.ledger_text().contains(&format!("sha={MOVED}")));
}

#[test]
fn an_ack_that_the_pr_no_longer_allows_is_refused_with_its_cause_and_the_request_stays_open() {
    type Break = fn(&mut PrScript);
    let cases: [(&str, Break, &str); 5] = [
        ("red", |s| s.checks = Checks::Red, "checks red"),
        (
            "pending",
            |s| s.checks = Checks::Pending,
            "checks are pending",
        ),
        (
            "conflict",
            |s| s.mergeable = Mergeable::Conflict,
            "merge conflict",
        ),
        ("base", |s| s.base = "develop".to_string(), "base develop"),
        (
            "head branch",
            |s| s.branch = "work/other".to_string(),
            "head branch work/other",
        ),
    ];
    for (name, break_it, cause) in cases {
        let env = ready();
        let r = open_ack(&env);
        let generation = claim(&env, "operator");
        script_mut(&env, break_it);
        let why = rejected(env.engine.ack_merge(&r.id, 1, generation));
        assert!(why.contains(cause), "{name}: {why}");
        let now = requests(&env).remove(0);
        assert_eq!(
            (now.status, now.revision),
            (RequestStatus::Open, 1),
            "{name}"
        );
        assert_eq!(env.status_of("spine").0, Status::Parked, "{name}");
        assert!(env.forge.merged().is_empty(), "{name}");
    }
}

#[test]
fn an_ack_under_a_fenced_claim_is_refused_and_changes_nothing() {
    let env = ready();
    let r = open_ack(&env);
    let old = claim(&env, "first");
    let current = claim(&env, "second");
    assert_eq!((old, current), (1, 2));
    let why = rejected(env.engine.ack_merge(&r.id, 1, old));
    assert!(why.contains("fenced"), "{why}");
    let now = requests(&env).remove(0);
    assert_eq!((now.status, now.revision), (RequestStatus::Open, 1));
    assert_eq!(env.status_of("spine").0, Status::Parked);
    assert!(env.forge.merged().is_empty());
    // The current claim still works.
    let step = env.engine.ack_merge(&r.id, 1, current).unwrap();
    assert!(matches!(step, MergeStep::Merged { .. }), "{step:?}");
}

#[test]
fn an_ack_on_a_halted_run_is_refused_and_a_refusal_writes_a_ledger_line() {
    let env = ready();
    let r = open_ack(&env);
    let generation = claim(&env, "operator");
    env.engine.halt("operator stop").unwrap();
    let why = rejected(env.engine.ack_merge(&r.id, 1, generation));
    assert!(why.contains("halted"), "{why}");
    let now = requests(&env).remove(0);
    assert_eq!((now.status, now.revision), (RequestStatus::Open, 1));
    assert_eq!(env.status_of("spine").0, Status::Parked);
    assert!(env.forge.merged().is_empty());
    let ledger = env.ledger_text();
    assert!(
        ledger.contains(&format!("ack {}@1 by claim {generation} refused:", r.id)),
        "{ledger}"
    );
}

#[test]
fn an_ack_on_an_ended_run_is_refused_naming_the_state() {
    let env = ready();
    let r = open_ack(&env);
    let generation = claim(&env, "operator");
    env.engine.end_run("done early").unwrap();
    let why = rejected(env.engine.ack_merge(&r.id, 1, generation));
    assert!(why.contains("ended"), "{why}");
    assert!(env.forge.merged().is_empty());
}

#[test]
fn an_ack_on_another_kind_of_request_or_an_unknown_one_is_refused() {
    let env = ready();
    let generation = claim(&env, "operator");
    // A run-level request of another kind.
    env.engine
        .open_request(
            None,
            RequestKind::Park,
            "",
            "something waits",
            &serde_json::json!({}),
        )
        .unwrap();
    let id = requests(&env)[0].id.clone();
    let why = rejected(env.engine.ack_merge(&id, 1, generation));
    assert!(why.contains("park") && why.contains("merge_ack"), "{why}");
    assert_eq!(requests(&env)[0].status, RequestStatus::Open);
    let why = rejected(env.engine.ack_merge("R-99", 1, generation));
    assert!(why.contains("R-99") && why.contains("unknown"), "{why}");
}

#[test]
fn answer_and_clear_on_a_merge_ack_are_refused_naming_ack() {
    let env = ready();
    let r = open_ack(&env);
    let generation = claim(&env, "operator");

    let why = match env
        .engine
        .answer_request(&r.id, 1, generation, "yes, merge it")
    {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("{other:?}"),
    };
    assert!(why.contains("ack"), "{why}");
    let why = match env.engine.check_answer(&r.id, 1, generation, "yes") {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("{other:?}"),
    };
    assert!(why.contains("ack"), "{why}");
    let why = match env.engine.clear_unit("spine", None, generation, &r.id) {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("{other:?}"),
    };
    assert!(why.contains("ack"), "{why}");

    let now = requests(&env).remove(0);
    assert_eq!((now.status, now.revision), (RequestStatus::Open, 1));
    assert_eq!(env.status_of("spine").0, Status::Parked);
    assert!(env.forge.merged().is_empty());
}

#[test]
fn a_merge_that_does_not_land_parks_with_merge_failed_and_clear_reopens_a_merge_ack() {
    let env = ready();
    let r = open_ack(&env);
    let generation = claim(&env, "operator");
    script_mut(&env, |s| s.landed = false);

    let step = env.engine.ack_merge(&r.id, 1, generation).unwrap();
    match &step {
        MergeStep::Parked { unit, reason } => {
            assert_eq!(unit, "spine");
            assert_eq!(reason, "merge did not land");
        }
        other => panic!("{other:?}"),
    }
    let all = requests(&env);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[0].status, RequestStatus::Answered);
    assert_eq!(all[1].kind, RequestKind::MergeFailed);
    assert_eq!(all[1].status, RequestStatus::Open);
    assert_eq!(all[1].stage, "merge");
    assert!(env.ledger_text().contains("outcome=merge did not land"));

    // The operator clears the unit. It goes back to the merge, which opens a new `merge_ack`.
    env.engine
        .clear_unit("spine", None, generation, &all[1].id)
        .unwrap();
    assert_eq!(env.status_of("spine").0, Status::Running);
    script_mut(&env, |s| s.landed = true);
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::AckRequested { .. }), "{step:?}");
    let all = requests(&env);
    assert_eq!(all.len(), 3, "{all:?}");
    assert_eq!(all[2].kind, RequestKind::MergeAck);
    assert_eq!(all[2].status, RequestStatus::Open);
    assert_ne!(all[2].id, r.id);
}

#[test]
fn a_unit_park_cannot_open_a_merge_ack() {
    let env = ready();
    let why = match env.engine.park_as("spine", RequestKind::MergeAck, "ack") {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("{other:?}"),
    };
    assert!(why.contains("merge_ack"), "{why}");
}
