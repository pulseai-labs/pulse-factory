//! AC-1 of `r2.s3.w3`: the acts of the clear grant that have an effect (ADR-0005, amended;
//! `docs/contracts/requests.md`, "Settle"). `rerun_check` and the re-attach of `resume` commit first
//! and run their effect after, and one `settle-result` command stores what the effect did.
//! `resolve_conflict` sends the unit to its track's conflict stage. A settle with no result holds
//! its unit: nothing launches or merges it.

mod common;

use std::collections::BTreeMap;

use common::*;
use pf_engine::admission::body_of_content;
use pf_engine::claim::{
    Act, KEY_DECISION, SettleAsk, SettleError, SettleFacts, SettleOutcome, pending_settle,
    result_command_id, settle_command_id,
};
use pf_engine::launch::launches_so_far;
use pf_engine::ledger::FsLedger;
use pf_engine::merge::MergeStep;
use pf_engine::port::agent::{NO_SESSION_HOST, RebindOutcome};
use pf_engine::port::forge::{Checks, Mergeable, PrRef};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Effect, NewAdmission, NewClearGrant,
    ORCHESTRATOR_AGENT, RequestKind, RequestRow, RequestStatus, Store,
};
use pf_engine::port::unit_source::UnitRecord;
use pf_engine::unit::Signal;
use pf_engine::workflow::OnConflict;
use pf_engine::{Cause, Command, CommandBody, Engine, MergeRule, Status};
use serde_json::Value;

const FORGE: (&str, &str) = ("synthetic/repo", "main");
const HEAD: &str = "1111111111111111111111111111111111111111";
const OTHER_HEAD: &str = "2222222222222222222222222222222222222222";

/// The `item` track: `build`, `review`, and the conflict stage `resolve`. `base` is the merge's base.
fn rig_workflow(base: &str, conflict: bool) -> pf_engine::Workflow {
    let mut wf = item_workflow(&["build", "review"], true);
    if conflict {
        let mut resolve = pf_engine::StageDef::new("resolve", "close-role", "model-a");
        resolve.entered_by_conflict = true;
        wf.tracks[0].stages.push(resolve);
    }
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.rule = MergeRule::GreenChecksInDeclaredOrder;
    def.repo = "synthetic/repo".to_string();
    def.base = base.to_string();
    if conflict {
        def.on_conflict = Some(OnConflict {
            stage: "resolve".to_string(),
            then: "review".to_string(),
        });
        def.guarded = vec!["docs/**".to_string()];
    }
    wf
}

fn commit_of(scope: &str, id: &str, effects: Vec<Effect>) -> Commit {
    Commit {
        run_id: scope.to_string(),
        command_id: id.to_string(),
        content: id.to_string(),
        admission: NewAdmission {
            kind: "probe".to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t".to_string(),
        },
        effects,
        ledger: None,
    }
}

fn store_commit(env: &Env, id: &str, effects: Vec<Effect>) {
    let applied = env
        .store
        .commit(commit_of(CONTROLLER_SCOPE, id, effects))
        .unwrap();
    assert!(
        matches!(applied, pf_engine::port::store::Committed::Applied { .. }),
        "{applied:?}"
    );
}

fn rig_with(wf: pf_engine::Workflow, units: &[&str], bound: Option<u64>) -> Env {
    let env = env_with_params(wf, BTreeMap::new());
    let records = units
        .iter()
        .enumerate()
        .map(|(i, id)| UnitRecord::new(*id, "g1", u32::try_from(i).unwrap(), BTreeMap::new()))
        .collect();
    env.engine
        .ingest(&mut TrackedSource {
            track: "item".to_string(),
            records,
        })
        .unwrap();
    env.store.add_project("P-1");
    store_commit(
        &env,
        "grant",
        vec![Effect::GrantClear(NewClearGrant {
            project_id: "P-1".to_string(),
            generation: 1,
            agent_id: ORCHESTRATOR_AGENT.to_string(),
            granted_by: "cli".to_string(),
            granted_at: "t".to_string(),
        })],
    );
    if let Some(bound) = bound {
        store_commit(
            &env,
            "bound",
            vec![Effect::SetRunLaunchBound {
                run_id: "run-1".to_string(),
                bound,
            }],
        );
    }
    env
}

fn rig(units: &[&str], bound: Option<u64>) -> Env {
    rig_with(rig_workflow("spine", true), units, bound)
}

/// An engine opened again on the same store, as a controller restart does.
fn reopen(env: &Env, wf: pf_engine::Workflow) -> Engine {
    let engine = Engine::new(
        config(env.dir.path(), wf),
        env.store.clone(),
        env.agent.clone(),
        env.forge.clone(),
        counting_clock(),
        Box::new(FsLedger::new(env.dir.path().join("ledger.md"))),
    )
    .unwrap();
    engine.start_up().unwrap();
    engine
}

fn request_of(env: &Env, unit: &str) -> RequestRow {
    env.store
        .all_requests()
        .into_iter()
        .rfind(|r| r.unit_id.as_deref() == Some(unit))
        .unwrap_or_else(|| panic!("no request for {unit}"))
}

fn cause_of(request: &RequestRow) -> String {
    let body: Value = serde_json::from_str(&request.body).unwrap();
    body[KEY_DECISION]["cause"].as_str().unwrap().to_string()
}

fn admitted(env: &Env, command_id: &str) -> bool {
    env.store.admission("run-1", command_id).unwrap().is_some()
}

fn count_kind(env: &Env, kind: &str) -> usize {
    env.store
        .admissions("run-1")
        .unwrap()
        .iter()
        .filter(|a| a.kind == kind)
        .count()
}

fn status(env: &Env, id: &str) -> Status {
    env.store.node("run-1", id).unwrap().status
}

fn done(env: &Env, unit: &str, stage: &str) {
    env.engine
        .signal(
            unit,
            stage,
            Signal::DoneFile {
                path: "done".to_string(),
            },
            None,
        )
        .unwrap();
}

fn pr(number: u64) -> PrRef {
    PrRef {
        repo: "synthetic/repo".to_string(),
        number,
    }
}

/// A unit at its merge with a PR (number 7), the forge's checks as `checks` and `mergeable`, and the
/// unit parked by the merge step. The failed checks of PR 7 are `ci`, which this forge can run again.
fn parked_at_merge(env: &Env, unit: &str, checks: Checks, mergeable: Mergeable) -> RequestRow {
    env.engine.start_stage(unit).unwrap();
    done(env, unit, "build");
    env.engine.start_stage(unit).unwrap();
    env.engine
        .signal(
            unit,
            "review",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(pr(7)),
        )
        .unwrap();
    let mut script = PrScript::green("work/u1", HEAD);
    script.checks = checks;
    script.mergeable = mergeable;
    env.forge.script(7, script);
    env.forge.script_failed(7, HEAD, &[("ci", None)]);
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    request_of(env, unit)
}

fn red_at_merge(env: &Env, unit: &str) -> RequestRow {
    parked_at_merge(env, unit, Checks::Red, Mergeable::Clean)
}

fn facts(limit: Option<u32>) -> SettleFacts {
    SettleFacts {
        reason: "the check is flaky; run it again".to_string(),
        session: "S-1".to_string(),
        project: "P-1".to_string(),
        generation: 1,
        agent: ORCHESTRATOR_AGENT.to_string(),
        rerun_limit: limit,
        ..SettleFacts::default()
    }
}

fn ask(request: &RequestRow, act: Act, limit: Option<u32>) -> SettleAsk {
    SettleAsk {
        request_id: request.id.clone(),
        revision: request.revision,
        act,
        to_stage: None,
        facts: facts(limit),
    }
}

fn settle(env: &Env, ask: SettleAsk) -> Result<pf_engine::Admission, SettleError> {
    env.engine.settle(ask, Some(FORGE))
}

fn refused(result: Result<pf_engine::Admission, SettleError>) -> String {
    match result {
        Err(SettleError::Refused(why)) => why,
        other => panic!("the settle must be refused, got {other:?}"),
    }
}

fn answer_of(env: &Env, request: &str) -> Value {
    let row = env
        .store
        .all_requests()
        .into_iter()
        .find(|r| r.id == request)
        .unwrap();
    serde_json::from_str(row.answer.as_deref().expect("the request has an answer")).unwrap()
}

fn lines_with(env: &Env, needle: &str) -> Vec<String> {
    env.ledger_text()
        .lines()
        .filter(|l| l.contains(needle))
        .map(str::to_string)
        .collect()
}

fn result_of(env: &Env, request: &str, revision: u32) -> (SettleOutcome, String) {
    let row = env
        .store
        .admission("run-1", &result_command_id(request, revision))
        .unwrap()
        .unwrap_or_else(|| panic!("no result for {request}@{revision}"));
    match body_of_content(&row.content).unwrap() {
        CommandBody::SettleResult {
            outcome, detail, ..
        } => (outcome, detail),
        other => panic!("not a result: {other:?}"),
    }
}

// ---------------------------------------------------------------------------- rerun_check

#[test]
fn rerun_check_resumes_at_the_merge_and_its_effect_re_runs_on_the_stored_head() {
    let env = rig(&["u1"], None);
    let request = red_at_merge(&env, "u1");
    assert_eq!(request.stage, "merge");

    let admission = settle(&env, ask(&request, Act::RerunCheck, Some(1))).unwrap();
    assert!(!admission.replayed);
    // The answer stores the head the engine read, and w2's line carries it.
    let answer = answer_of(&env, &request.id);
    assert_eq!(answer["verb"], "settle");
    assert_eq!(answer["act"], "rerun_check");
    assert_eq!(answer["head_sha"], HEAD);
    let line = lines_with(&env, " settle R-")
        .into_iter()
        .find(|l| l.contains("act=rerun_check"))
        .unwrap();
    assert!(
        line.contains(&format!("act=rerun_check head={HEAD}")),
        "{line}"
    );
    assert_eq!(status(&env, "u1"), Status::Running);
    // The effect has not run: the settle is pending, and the unit does not merge.
    assert_eq!(env.forge.reruns().len(), 0);
    let pending = env.engine.pending_settle_of("u1").unwrap().unwrap();
    assert_eq!(pending.name(), format!("{}@1", request.id));
    match env.engine.merge_next().unwrap() {
        MergeStep::Waiting { why, .. } => {
            assert!(why.contains("waits for its effect's result"), "{why}")
        }
        other => panic!("a pending unit must wait: {other:?}"),
    }

    let result = env.engine.settle_effect(&request.id, 1).unwrap();
    assert!(!result.replayed);
    assert_eq!(env.forge.reruns(), vec![(7, HEAD.to_string())]);
    let (outcome, _) = result_of(&env, &request.id, 1);
    assert_eq!(outcome, SettleOutcome::Ran);
    // The `git-effects` line: the repo, the PR, the SHA and the checks, written when the forge answered.
    let ran = lines_with(&env, "rerun_check ran");
    assert_eq!(ran.len(), 1, "{ran:?}");
    assert!(
        ran[0].contains(&format!(
            "settle-result {}@1: rerun_check ran repo=synthetic/repo pr=7 sha={HEAD} checks=ci",
            request.id
        )),
        "{}",
        ran[0]
    );
    assert!(env.engine.pending_settle_of("u1").unwrap().is_none());

    // Pending checks wait; green checks merge.
    match env.engine.merge_next().unwrap() {
        MergeStep::Waiting { why, .. } => assert_eq!(why, "checks are pending"),
        other => panic!("{other:?}"),
    }
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&7)
        .unwrap()
        .checks = Checks::Green;
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Merged { .. }
    ));
}

#[test]
fn a_failed_check_the_forge_cannot_run_again_is_refused_before_the_commit() {
    let env = rig(&["u1"], None);
    let request = red_at_merge(&env, "u1");
    env.forge.script_failed(
        7,
        HEAD,
        &[
            ("ci", None),
            ("outside", Some("a commit status that an outside CI wrote")),
        ],
    );
    let why = refused(settle(&env, ask(&request, Act::RerunCheck, Some(1))));
    assert_eq!(
        why,
        "the failed check outside of PR synthetic/repo#7 cannot be run again by this forge: a commit status that an outside CI wrote"
    );
    assert!(!admitted(&env, &settle_command_id(&request.id, 1)));
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(env.forge.reruns().len(), 0);
}

#[test]
fn a_pr_with_no_failed_check_and_a_failed_read_are_refused() {
    let env = rig(&["u1"], None);
    let request = red_at_merge(&env, "u1");
    env.forge.script_failed(7, HEAD, &[]);
    assert_eq!(
        refused(settle(&env, ask(&request, Act::RerunCheck, Some(1)))),
        "PR synthetic/repo#7 has no failed check to run again"
    );
    *env.forge.failed_error.lock().unwrap() = Some("boom".to_string());
    let why = refused(settle(&env, ask(&request, Act::RerunCheck, Some(1))));
    assert!(why.starts_with("the PR's checks cannot be read: "), "{why}");
    assert!(!admitted(&env, &settle_command_id(&request.id, 1)));
}

#[test]
fn a_head_that_moved_after_the_read_re_runs_nothing_and_parks_the_unit_again() {
    let env = rig(&["u1"], None);
    let request = red_at_merge(&env, "u1");
    *env.forge.head_after_failed_read.lock().unwrap() = Some(OTHER_HEAD.to_string());
    settle(&env, ask(&request, Act::RerunCheck, Some(1))).unwrap();
    env.engine.settle_effect(&request.id, 1).unwrap();

    let (outcome, detail) = result_of(&env, &request.id, 1);
    assert_eq!(outcome, SettleOutcome::Failed);
    assert!(detail.contains("not_ran=head: moved to"), "{detail}");
    assert_eq!(status(&env, "u1"), Status::Parked);
    let next = request_of(&env, "u1");
    assert_ne!(next.id, request.id);
    assert_eq!(cause_of(&next), "merge_checks_red");
    assert_eq!(next.kind, RequestKind::MergeFailed);
    assert!(
        next.reason
            .contains(&format!("the re-run of settle {}@1 failed", request.id)),
        "{}",
        next.reason
    );
    let lines = lines_with(&env, "rerun_check failed");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines_with(&env, "rerun_check ran").is_empty());
    // The act counted: a second re-run on the same head is refused at the limit.
    let again = refused(settle(&env, ask(&next, Act::RerunCheck, Some(1))));
    assert!(again.contains("admitted 1 times for head"), "{again}");
}

#[test]
fn a_forge_error_stores_failed_with_an_empty_ran_list_and_no_ran_line() {
    let env = rig(&["u1"], None);
    let request = red_at_merge(&env, "u1");
    settle(&env, ask(&request, Act::RerunCheck, Some(1))).unwrap();
    *env.forge.rerun_error.lock().unwrap() = Some("the forge is down".to_string());
    env.engine.settle_effect(&request.id, 1).unwrap();

    let (outcome, detail) = result_of(&env, &request.id, 1);
    assert_eq!(outcome, SettleOutcome::Failed);
    assert!(detail.contains("ran= not_ran=forge: "), "{detail}");
    let failed = lines_with(&env, "rerun_check failed");
    assert_eq!(failed.len(), 1);
    assert!(failed[0].contains(&format!("sha={HEAD}")), "{}", failed[0]);
    assert!(failed[0].contains("unit u1 parked"), "{}", failed[0]);
    assert!(lines_with(&env, "rerun_check ran").is_empty());
    assert_eq!(status(&env, "u1"), Status::Parked);
    assert_eq!(cause_of(&request_of(&env, "u1")), "merge_checks_red");
}

#[test]
fn the_count_reads_the_stored_answers_and_a_new_head_starts_at_zero() {
    let env = rig(&["u1"], None);
    let first = red_at_merge(&env, "u1");
    settle(&env, ask(&first, Act::RerunCheck, Some(1))).unwrap();
    env.engine.settle_effect(&first.id, 1).unwrap();
    // The checks are red again on the same head: the unit parks again.
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&7)
        .unwrap()
        .checks = Checks::Red;
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Parked { .. }
    ));
    let second = request_of(&env, "u1");
    assert_ne!(second.id, first.id);

    let why = refused(settle(&env, ask(&second, Act::RerunCheck, Some(1))));
    assert_eq!(
        why,
        format!(
            "the act rerun_check was admitted 1 times for head {HEAD} of this unit's PR, and authority.rerun_checks_per_head is 1; redispatch, or the operator, remains"
        )
    );
    assert!(why.contains("authority.rerun_checks_per_head"));
    // An engine opened again on the same store refuses the same way.
    let engine = reopen(&env, rig_workflow("spine", true));
    let again = engine.settle(ask(&second, Act::RerunCheck, Some(1)), Some(FORGE));
    assert_eq!(refused(again), why);
    // With the limit 2 it is admitted.
    assert!(settle(&env, ask(&second, Act::RerunCheck, Some(2))).is_ok());
    env.engine.settle_effect(&second.id, 1).unwrap();
    // A new head starts at 0.
    env.forge.scripts.lock().unwrap().get_mut(&7).unwrap().sha = OTHER_HEAD.to_string();
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&7)
        .unwrap()
        .checks = Checks::Red;
    env.forge.script_failed(7, OTHER_HEAD, &[("ci", None)]);
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Parked { .. }
    ));
    let third = request_of(&env, "u1");
    assert!(settle(&env, ask(&third, Act::RerunCheck, Some(1))).is_ok());
}

#[test]
fn a_rerun_with_no_limit_names_no_head_or_limit() {
    let env = rig(&["u1"], None);
    let request = red_at_merge(&env, "u1");
    let direct = pf_engine::Command {
        id: settle_command_id(&request.id, 1),
        body: CommandBody::Settle {
            request_id: request.id.clone(),
            revision: 1,
            act: Act::RerunCheck,
            to_stage: None,
            facts: facts(Some(1)),
        },
    };
    let why = match env.engine.submit(direct) {
        Err(pf_engine::EngineError::Rejected(why)) => why,
        other => panic!("{other:?}"),
    };
    assert!(why.contains("names no head or no re-run limit"), "{why}");
}

// ---------------------------------------------------------------------------- resolve_conflict

#[test]
fn resolve_conflict_resumes_the_unit_at_the_conflict_stage_with_the_head() {
    let env = rig(&["u1"], Some(10));
    let request = parked_at_merge(&env, "u1", Checks::Green, Mergeable::Conflict);
    assert_eq!(cause_of(&request), "merge_conflict");

    settle(&env, ask(&request, Act::ResolveConflict, None)).unwrap();
    assert_eq!(answer_of(&env, &request.id)["head_sha"], HEAD);
    let line = lines_with(&env, " settle R-").pop().unwrap();
    assert!(
        line.contains(&format!("act=resolve_conflict head={HEAD}")),
        "{line}"
    );
    assert_eq!(status(&env, "u1"), Status::Running);
    // It has no effect of its own and no result: the next tick launches the conflict stage.
    assert!(env.engine.pending_settle_of("u1").unwrap().is_none());
    let step = env.engine.start_stage("u1").unwrap();
    assert!(
        matches!(step, pf_engine::launch::LaunchStep::Launched { .. }),
        "{step:?}"
    );
    assert!(admitted(&env, "launch:u1:resolve:1"));
    assert_eq!(count_kind(&env, "settle_result"), 0);
}

#[test]
fn a_track_with_no_on_conflict_refuses_resolve_conflict_and_writes_nothing() {
    let env = rig_with(rig_workflow("spine", false), &["u1"], Some(10));
    let request = parked_at_merge(&env, "u1", Checks::Green, Mergeable::Conflict);
    let before = env.store.admissions("run-1").unwrap().len();
    let why = refused(settle(&env, ask(&request, Act::ResolveConflict, None)));
    assert!(
        why.contains(
            "declares no on_conflict stage, so resolve_conflict has no stage to send the unit to"
        ),
        "{why}"
    );
    assert_eq!(env.store.admissions("run-1").unwrap().len(), before);
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
}

#[test]
fn a_read_of_the_head_that_fails_refuses_resolve_conflict() {
    let env = rig(&["u1"], Some(10));
    let request = parked_at_merge(&env, "u1", Checks::Green, Mergeable::Conflict);
    env.forge.scripts.lock().unwrap().remove(&7);
    let why = refused(settle(&env, ask(&request, Act::ResolveConflict, None)));
    assert!(why.starts_with("the PR's head cannot be read: "), "{why}");
    assert!(!admitted(&env, &settle_command_id(&request.id, 1)));
}

// ---------------------------------------------------------------------------- the default branch

#[test]
fn on_the_default_branch_rerun_check_and_resolve_conflict_are_above_and_call_no_forge() {
    for (checks, mergeable, act) in [
        (Checks::Red, Mergeable::Clean, Act::RerunCheck),
        (Checks::Green, Mergeable::Conflict, Act::ResolveConflict),
    ] {
        for rule in [MergeRule::GreenChecksInDeclaredOrder, MergeRule::ClaimAck] {
            let mut wf = rig_workflow("main", true);
            wf.tracks[0].merge.as_mut().unwrap().rule = rule;
            let env = rig_with(wf, &["u1"], Some(10));
            if rule == MergeRule::ClaimAck {
                // A claim-ack track parks a red check above the line as well.
                env.engine.start_stage("u1").unwrap();
                done(&env, "u1", "build");
                env.engine.start_stage("u1").unwrap();
                env.engine
                    .signal(
                        "u1",
                        "review",
                        Signal::PrState {
                            state: "open".to_string(),
                        },
                        Some(pr(7)),
                    )
                    .unwrap();
                let mut script = PrScript::green("work/u1", HEAD);
                script.checks = checks;
                script.mergeable = mergeable;
                env.forge.script(7, script);
                env.forge.script_failed(7, HEAD, &[("ci", None)]);
                env.engine.merge_next().unwrap();
            } else {
                parked_at_merge(&env, "u1", checks, mergeable);
            }
            let request = request_of(&env, "u1");
            let reads = env.forge.failed_reads.lock().unwrap().len();
            let status_reads = env.forge.status_reads.lock().unwrap().len();
            let before = env.store.admissions("run-1").unwrap().len();
            let why = refused(settle(&env, ask(&request, act, Some(1))));
            assert!(why.contains("is above the line"), "{rule:?} {act:?}: {why}");
            assert_eq!(env.store.admissions("run-1").unwrap().len(), before);
            assert_eq!(env.forge.failed_reads.lock().unwrap().len(), reads);
            assert_eq!(env.forge.status_reads.lock().unwrap().len(), status_reads);
        }
    }
}

// ---------------------------------------------------------------------------- the re-attach

/// A unit `u1` that tier 1 parked as `host_unreachable`: its seat `proc-1` is launched, then the
/// engine opens again and the scripted rebind answers `Unreachable`.
fn unreachable(bound: Option<u64>) -> (Env, Engine, RequestRow) {
    let env = rig(&["u1"], bound);
    env.engine.start_stage("u1").unwrap();
    env.agent.script_rebind(
        "proc-1",
        RebindOutcome::Unreachable {
            why: "the host did not answer".to_string(),
        },
    );
    let engine = reopen(&env, rig_workflow("spine", true));
    let request = request_of(&env, "u1");
    assert_eq!(cause_of(&request), "host_unreachable");
    (env, engine, request)
}

fn settle_on(
    engine: &Engine,
    request: &RequestRow,
    act: Act,
) -> Result<pf_engine::Admission, SettleError> {
    engine.settle(ask(request, act, None), Some(FORGE))
}

#[test]
fn a_host_that_answers_is_attached_with_no_launch_and_a_new_stage_attempt() {
    let (env, engine, request) = unreachable(Some(10));
    let launches_before = count_kind(&env, "launch");
    settle_on(&engine, &request, Act::Resume).unwrap();
    // Pending: no launch is admitted before the result.
    let pending = engine.pending_settle_of("u1").unwrap().unwrap();
    assert_eq!(pending.act, Act::Resume);
    let held = engine.start_stage("u1");
    assert!(held.is_err(), "{held:?}");
    assert!(
        format!("{held:?}").contains("waits for the result of settle"),
        "{held:?}"
    );
    env.agent.script_rebind("proc-1", RebindOutcome::Survived);
    engine.settle_effect(&request.id, 1).unwrap();

    assert_eq!(result_of(&env, &request.id, 1).0, SettleOutcome::Attached);
    assert_eq!(count_kind(&env, "launch"), launches_before);
    assert_eq!(status(&env, "u1"), Status::Running);
    let node = env.store.node("run-1", "u1/build#2").unwrap();
    assert_eq!(node.status, Status::Running);
    let view = engine.view().unwrap();
    let session = view.session("u1/build#2").unwrap();
    assert_eq!(session.process_ref.as_deref(), Some("proc-1"));
    assert!(!session.ended);
    // The old node and session stay ended, and the request is answered as settled.
    assert!(view.session("u1/build#1").unwrap().ended);
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Answered);
    assert!(
        lines_with(
            &env,
            "resume attached host=u1 session=u1/build#2 stage=build attempt=2"
        )
        .len()
            == 1
    );
    // The end of the stage uses the new node: `signal:u1/build#2`.
    engine
        .signal(
            "u1",
            "build",
            Signal::DoneFile {
                path: "done".to_string(),
            },
            None,
        )
        .unwrap();
    assert!(admitted(&env, "signal:u1/build#2"));
}

#[test]
fn a_gone_host_is_replaced_by_a_relaunch_through_the_normal_path() {
    let (env, engine, request) = unreachable(Some(10));
    settle_on(&engine, &request, Act::Resume).unwrap();
    env.agent.script_rebind(
        "proc-1",
        RebindOutcome::Lost {
            cause: "the host unit is gone".to_string(),
        },
    );
    engine.settle_effect(&request.id, 1).unwrap();
    assert_eq!(result_of(&env, &request.id, 1).0, SettleOutcome::Gone);
    assert!(engine.pending_settle_of("u1").unwrap().is_none());
    assert_eq!(status(&env, "u1"), Status::Running);
    // The next tick launches the stage again.
    let step = engine.start_stage("u1").unwrap();
    assert!(
        matches!(step, pf_engine::launch::LaunchStep::Launched { .. }),
        "{step:?}"
    );
    assert!(admitted(&env, "launch:u1:build:2"));
}

#[test]
fn a_host_that_still_does_not_answer_parks_the_unit_again() {
    let (env, engine, request) = unreachable(Some(10));
    settle_on(&engine, &request, Act::Resume).unwrap();
    engine.settle_effect(&request.id, 1).unwrap();
    assert_eq!(result_of(&env, &request.id, 1).0, SettleOutcome::Failed);
    assert_eq!(status(&env, "u1"), Status::Parked);
    let next = request_of(&env, "u1");
    assert_ne!(next.id, request.id);
    assert_eq!(cause_of(&next), "host_unreachable");
    assert_eq!(next.kind, RequestKind::SeatLost);
    assert!(
        next.reason
            .starts_with("host unreachable: the re-attach of settle"),
        "{}",
        next.reason
    );
    assert_eq!(lines_with(&env, "resume failed:").len(), 1);
}

#[test]
fn a_lost_answer_with_no_session_host_follows_is_alive() {
    for (alive, expected) in [
        (true, SettleOutcome::Attached),
        (false, SettleOutcome::Gone),
    ] {
        let (env, engine, request) = unreachable(Some(10));
        settle_on(&engine, &request, Act::Resume).unwrap();
        env.agent.script_rebind(
            "proc-1",
            RebindOutcome::Lost {
                cause: NO_SESSION_HOST.to_string(),
            },
        );
        if !alive {
            env.agent.kill("proc-1");
        }
        engine.settle_effect(&request.id, 1).unwrap();
        assert_eq!(result_of(&env, &request.id, 1).0, expected);
        assert!(env.agent.call_log().contains(&"alive:proc-1".to_string()));
    }
}

// ---------------------------------------------------------------------------- the replay

#[test]
fn each_new_arm_sent_again_returns_the_first_record_and_runs_no_second_effect() {
    // rerun_check
    let env = rig(&["u1"], Some(10));
    let request = red_at_merge(&env, "u1");
    let first = settle(&env, ask(&request, Act::RerunCheck, Some(1))).unwrap();
    env.engine.settle_effect(&request.id, 1).unwrap();
    let lines = std::fs::read_to_string(env.ledger_path())
        .unwrap()
        .lines()
        .count();
    let mut again = ask(&request, Act::RerunCheck, Some(1));
    again.facts.reason = "another reason".to_string();
    let second = settle(&env, again).unwrap();
    assert!(second.replayed);
    assert_eq!(second.row.seq, first.row.seq);
    assert_eq!(
        std::fs::read_to_string(env.ledger_path())
            .unwrap()
            .lines()
            .count(),
        lines
    );
    // A second effect returns the stored result and calls the forge no more.
    assert!(env.engine.settle_effect(&request.id, 1).unwrap().replayed);
    assert_eq!(env.forge.reruns().len(), 1);
    // Another act on the settled revision is a conflict.
    let other = settle(&env, ask(&request, Act::Redispatch, Some(1)));
    assert!(
        matches!(other, Err(SettleError::Conflict)) || other.is_err(),
        "{other:?}"
    );

    // resolve_conflict
    let env = rig(&["u1"], Some(10));
    let request = parked_at_merge(&env, "u1", Checks::Green, Mergeable::Conflict);
    settle(&env, ask(&request, Act::ResolveConflict, None)).unwrap();
    let again = settle(&env, ask(&request, Act::ResolveConflict, None)).unwrap();
    assert!(again.replayed);

    // resume on host_unreachable
    let (env, engine, request) = unreachable(Some(10));
    settle_on(&engine, &request, Act::Resume).unwrap();
    env.agent.script_rebind("proc-1", RebindOutcome::Survived);
    engine.settle_effect(&request.id, 1).unwrap();
    let rebinds = env
        .agent
        .call_log()
        .iter()
        .filter(|c| c.starts_with("rebind:"))
        .count();
    assert!(settle_on(&engine, &request, Act::Resume).unwrap().replayed);
    assert!(engine.settle_effect(&request.id, 1).unwrap().replayed);
    assert_eq!(
        env.agent
            .call_log()
            .iter()
            .filter(|c| c.starts_with("rebind:"))
            .count(),
        rebinds
    );
}

// ---------------------------------------------------------------------------- the result command

#[test]
fn the_result_command_replays_conflicts_and_is_refused_when_it_does_not_fit() {
    let env = rig(&["u1"], Some(10));
    let request = red_at_merge(&env, "u1");
    // No settle yet: a result is refused.
    let id = result_command_id(&request.id, 1);
    let body = |outcome: SettleOutcome| CommandBody::SettleResult {
        request_id: request.id.clone(),
        revision: 1,
        outcome,
        detail: "repo=synthetic/repo pr=7 sha=x checks=ci".to_string(),
        process_ref: None,
    };
    let early = env.engine.submit(Command {
        id: id.clone(),
        body: body(SettleOutcome::Ran),
    });
    assert!(early.is_err(), "{early:?}");

    settle(&env, ask(&request, Act::RerunCheck, Some(1))).unwrap();
    // `attached` fits no re-run.
    let misfit = env.engine.submit(Command {
        id: id.clone(),
        body: body(SettleOutcome::Attached),
    });
    assert!(
        format!("{misfit:?}").contains("does not fit the effect"),
        "{misfit:?}"
    );
    // A detail of more than 300 characters is refused.
    let long = env.engine.submit(Command {
        id: id.clone(),
        body: CommandBody::SettleResult {
            request_id: request.id.clone(),
            revision: 1,
            outcome: SettleOutcome::Ran,
            detail: "x".repeat(301),
            process_ref: None,
        },
    });
    assert!(long.is_err());

    assert!(pending_settle(&env.engine.view().unwrap(), "u1").is_some());
    let first = env
        .engine
        .submit(Command {
            id: id.clone(),
            body: body(SettleOutcome::Ran),
        })
        .unwrap();
    assert!(!first.replayed);
    let replay = env
        .engine
        .submit(Command {
            id: id.clone(),
            body: body(SettleOutcome::Ran),
        })
        .unwrap();
    assert!(replay.replayed);
    let conflict = env.engine.submit(Command {
        id,
        body: body(SettleOutcome::Failed),
    });
    assert!(matches!(
        conflict,
        Err(pf_engine::EngineError::Conflict { .. })
    ));
}

// ---------------------------------------------------------------------------- the reconcile

#[test]
fn the_reconcile_stores_unknown_for_a_pending_effect_and_runs_no_effect() {
    let env = rig(&["u1", "u2"], Some(10));
    // u1: a pending re-run.
    let red = red_at_merge(&env, "u1");
    settle(&env, ask(&red, Act::RerunCheck, Some(1))).unwrap();
    // u2: a pending re-attach.
    env.engine.start_stage("u2").unwrap();
    env.agent.script_rebind(
        "proc-3",
        RebindOutcome::Unreachable {
            why: "no answer".to_string(),
        },
    );
    let first = reopen(&env, rig_workflow("spine", true));
    let lost = request_of(&env, "u2");
    assert_eq!(cause_of(&lost), "host_unreachable");
    settle_on(&first, &lost, Act::Resume).unwrap();
    let rebinds_before = env.agent.call_log().len();

    // The controller stops before either effect ran, and opens again.
    let engine = reopen(&env, rig_workflow("spine", true));
    assert_eq!(result_of(&env, &red.id, 1).0, SettleOutcome::Unknown);
    assert_eq!(result_of(&env, &lost.id, 1).0, SettleOutcome::Unknown);
    assert_eq!(status(&env, "u1"), Status::Parked);
    assert_eq!(status(&env, "u2"), Status::Parked);
    assert_eq!(cause_of(&request_of(&env, "u1")), "merge_checks_red");
    assert_eq!(cause_of(&request_of(&env, "u2")), "host_unreachable");
    assert!(
        request_of(&env, "u1")
            .reason
            .contains("is unknown after a controller restart")
    );
    assert_eq!(env.forge.reruns().len(), 0);
    assert_eq!(env.agent.call_log().len(), rebinds_before);
    assert_eq!(
        lines_with(&env, "unknown: rerun_check was not run again").len(),
        1
    );
    assert_eq!(
        lines_with(&env, "unknown: resume was not run again").len(),
        1
    );
    assert!(engine.pending_settle_of("u1").unwrap().is_none());
}

// ---------------------------------------------------------------------------- the halt

#[test]
fn a_halted_run_refuses_each_new_arm_and_writes_no_admission() {
    let env = rig(&["u1", "u2", "u3"], Some(10));
    let red = red_at_merge(&env, "u1");
    let conflict = parked_at_merge_unit(&env, "u2");
    env.engine.start_stage("u3").unwrap();
    env.agent.script_rebind(
        "proc-5",
        RebindOutcome::Unreachable {
            why: "no answer".to_string(),
        },
    );
    let engine = reopen(&env, rig_workflow("spine", true));
    let lost = request_of(&env, "u3");
    engine.halt("operator stop").unwrap();
    let before = env.store.admissions("run-1").unwrap().len();
    let failed_reads = env.forge.failed_reads.lock().unwrap().len();
    let status_reads = env.forge.status_reads.lock().unwrap().len();
    for (request, act) in [
        (&red, Act::RerunCheck),
        (&conflict, Act::ResolveConflict),
        (&lost, Act::Resume),
    ] {
        let why = refused(engine.settle(ask(request, act, Some(1)), Some(FORGE)));
        assert!(
            why.contains("a halted run refuses every settle"),
            "{act:?}: {why}"
        );
    }
    assert_eq!(env.store.admissions("run-1").unwrap().len(), before);
    // A halted run calls no forge: the halt is checked before any read.
    assert_eq!(env.forge.failed_reads.lock().unwrap().len(), failed_reads);
    assert_eq!(env.forge.status_reads.lock().unwrap().len(), status_reads);
    assert_eq!(env.forge.reruns().len(), 0);
}

/// A second unit parked at its merge with a conflict (PR 8).
fn parked_at_merge_unit(env: &Env, unit: &str) -> RequestRow {
    env.engine.start_stage(unit).unwrap();
    done(env, unit, "build");
    env.engine.start_stage(unit).unwrap();
    env.engine
        .signal(
            unit,
            "review",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(pr(8)),
        )
        .unwrap();
    let mut script = PrScript::green("work/u2", OTHER_HEAD);
    script.mergeable = Mergeable::Conflict;
    env.forge.script(8, script);
    // u1 holds the order and is parked, so the merge step waits: park u2 directly as it would.
    env.engine
        .park_as(
            unit,
            RequestKind::MergeFailed,
            Cause::MergeConflict,
            "merge conflict",
        )
        .unwrap();
    request_of(env, unit)
}

#[test]
fn a_run_halted_between_the_commit_and_the_effect_stores_failed_and_runs_nothing() {
    let env = rig(&["u1"], Some(10));
    let request = red_at_merge(&env, "u1");
    settle(&env, ask(&request, Act::RerunCheck, Some(1))).unwrap();
    env.engine.halt("operator stop").unwrap();
    env.engine.settle_effect(&request.id, 1).unwrap();
    let (outcome, detail) = result_of(&env, &request.id, 1);
    assert_eq!(outcome, SettleOutcome::Failed);
    assert_eq!(detail, "the run is halted: the effect did not run");
    assert_eq!(env.forge.reruns().len(), 0);
    assert_eq!(status(&env, "u1"), Status::Parked);
}

// ---------------------------------------------------------------------------- launches so far

#[test]
fn launches_so_far_counts_what_each_act_adds_and_where_its_count_ends() {
    // rerun_check adds nothing.
    let env = rig(&["u1"], Some(10));
    let request = red_at_merge(&env, "u1");
    let base = launches_so_far(&env.engine.view().unwrap()).so_far();
    settle(&env, ask(&request, Act::RerunCheck, Some(1))).unwrap();
    assert_eq!(launches_so_far(&env.engine.view().unwrap()).so_far(), base);

    // resolve_conflict adds 1 from its commit; after the launch the total is the same.
    let env = rig(&["u1"], Some(10));
    let request = parked_at_merge(&env, "u1", Checks::Green, Mergeable::Conflict);
    let base = launches_so_far(&env.engine.view().unwrap()).so_far();
    settle(&env, ask(&request, Act::ResolveConflict, None)).unwrap();
    assert_eq!(
        launches_so_far(&env.engine.view().unwrap()).so_far(),
        base + 1
    );
    env.engine.start_stage("u1").unwrap();
    assert_eq!(
        launches_so_far(&env.engine.view().unwrap()).so_far(),
        base + 1
    );

    // the re-attach: attached, failed and gone.
    for (scripted, expected_after, expect_launch) in [
        (RebindOutcome::Survived, 0usize, false),
        (
            RebindOutcome::Unreachable {
                why: "no".to_string(),
            },
            0,
            false,
        ),
        (
            RebindOutcome::Lost {
                cause: "gone".to_string(),
            },
            1,
            true,
        ),
    ] {
        let (env, engine, request) = unreachable(Some(10));
        let base = launches_so_far(&engine.view().unwrap()).so_far();
        settle_on(&engine, &request, Act::Resume).unwrap();
        assert_eq!(launches_so_far(&engine.view().unwrap()).so_far(), base + 1);
        env.agent.script_rebind("proc-1", scripted);
        engine.settle_effect(&request.id, 1).unwrap();
        assert_eq!(
            launches_so_far(&engine.view().unwrap()).so_far(),
            base + expected_after
        );
        if expect_launch {
            engine.start_stage("u1").unwrap();
            // The relaunch is admitted, and the total is the same: counted once.
            assert_eq!(launches_so_far(&engine.view().unwrap()).so_far(), base + 1);
        }
    }
}

#[test]
fn at_the_bound_a_re_attach_takes_the_room_and_a_failed_one_frees_it() {
    let env = rig(&["u1", "u2"], None);
    env.engine.start_stage("u1").unwrap();
    env.engine.start_stage("u2").unwrap();
    env.agent.script_rebind(
        "proc-1",
        RebindOutcome::Unreachable {
            why: "no".to_string(),
        },
    );
    env.agent.script_rebind(
        "proc-2",
        RebindOutcome::Unreachable {
            why: "no".to_string(),
        },
    );
    let engine = reopen(&env, rig_workflow("spine", true));
    let first = request_of(&env, "u1");
    let second = request_of(&env, "u2");
    // Two launches are admitted: the bound leaves room for one more.
    store_commit(
        &env,
        "bound",
        vec![Effect::SetRunLaunchBound {
            run_id: "run-1".to_string(),
            bound: 3,
        }],
    );
    settle_on(&engine, &first, Act::Resume).unwrap();
    let why = refused(settle_on(&engine, &second, Act::Resume));
    assert!(why.contains("have reached its armed launch count"), "{why}");
    // The re-attach fails: the room is free again.
    engine.settle_effect(&first.id, 1).unwrap();
    assert!(settle_on(&engine, &second, Act::Resume).is_ok());
}
