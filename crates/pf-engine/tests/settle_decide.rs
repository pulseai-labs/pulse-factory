//! AC-1 of `r2.s3.w2`: the engine's `settle` decision (ADR-0005, amended; `docs/contracts/requests.md`,
//! "Settle"). `Engine::settle` runs under the engine's lock: the replay first, then the request, the
//! line, the cause's acts and the line record, and one `submit`. `decide_settle` checks the
//! revision, the halt, `--to`, the launch bound and the grant's precondition again, and one commit
//! closes the request, resumes the unit and carries the precondition.

mod common;

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use pf_engine::Cause;
use pf_engine::admission::canonical_content;
use pf_engine::claim::{Act, SettleAsk, SettleError, SettleFacts, acts_of, sha256_hex};
use pf_engine::launch::launches_so_far;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::{Checks, PrRef};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Effect, NewAdmission, NewClearGrant,
    ORCHESTRATOR_AGENT, RequestKind, RequestRow, RequestStatus, Store,
};
use pf_engine::port::unit_source::UnitRecord;
use pf_engine::unit::{Signal, UnitDetail};
use pf_engine::{Command, CommandBody, EndStatus, EngineError, MergeRule, Status};
use serde_json::{Value, json};

/// The forge's repo and default branch, as the controller passes them.
const FORGE: (&str, &str) = ("synthetic/repo", "main");

const BOUND_TEXT: &str = "have reached its armed launch count";

fn rig_workflow() -> pf_engine::Workflow {
    let mut wf = item_workflow(&["build", "review"], true);
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.rule = MergeRule::GreenChecksInDeclaredOrder;
    def.repo = "synthetic/repo".to_string();
    // Not the forge's default branch, so a red check is below the line.
    def.base = "spine".to_string();
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

/// An engine with these units on the track `item`, the project P-1 with a live grant at generation
/// 1, and the run's launch count set to `bound` when one is given.
fn rig(units: &[&str], bound: Option<u64>) -> Env {
    let env = env_with_params(rig_workflow(), BTreeMap::new());
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
    let grant = Effect::GrantClear(NewClearGrant {
        project_id: "P-1".to_string(),
        generation: 1,
        agent_id: ORCHESTRATOR_AGENT.to_string(),
        granted_by: "cli".to_string(),
        granted_at: "t".to_string(),
    });
    store_commit(&env, "grant", vec![grant]);
    if let Some(bound) = bound {
        set_bound(&env, bound);
    }
    env
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

fn set_bound(env: &Env, bound: u64) {
    store_commit(
        env,
        "bound",
        vec![Effect::SetRunLaunchBound {
            run_id: "run-1".to_string(),
            bound,
        }],
    );
}

fn revoke_grant(env: &Env) {
    store_commit(
        env,
        "revoke",
        vec![Effect::RevokeClearGrant {
            project_id: "P-1".to_string(),
            generation: 1,
            revoked_by: "cli".to_string(),
            revoked_at: "t".to_string(),
        }],
    );
}

fn detail(env: &Env, id: &str) -> UnitDetail {
    let view = env.engine.view().unwrap();
    view.unit_detail(view.node(id).unwrap()).unwrap()
}

fn status(env: &Env, id: &str) -> Status {
    env.store.node("run-1", id).unwrap().status
}

fn request_of(env: &Env, unit: &str) -> RequestRow {
    env.store
        .all_requests()
        .into_iter()
        .rfind(|r| r.unit_id.as_deref() == Some(unit))
        .unwrap_or_else(|| panic!("no request for {unit}"))
}

fn admitted(env: &Env, command_id: &str) -> bool {
    env.store.admission("run-1", command_id).unwrap().is_some()
}

/// Launch the unit's next stage, as a tick does.
fn launch(env: &Env, unit: &str) {
    env.engine.start_stage(unit).unwrap();
}

/// End the running stage of `unit` with `signal`, as a seat's done signal does.
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

/// Park the unit with a stall in its first stage, and end the seat's stage, as the controller does.
/// The request is below the line.
fn stall(env: &Env, unit: &str) -> RequestRow {
    launch(env, unit);
    env.engine
        .park_with(
            unit,
            RequestKind::Stall,
            Cause::Stalled,
            "stalled: no output",
            None,
        )
        .unwrap();
    env.engine
        .end(
            &format!("{unit}/build#1"),
            EndStatus::Failed,
            "stalled: no output",
        )
        .unwrap();
    request_of(env, unit)
}

/// Park the unit with a stall in its second stage.
fn stall_in_review(env: &Env, unit: &str) -> RequestRow {
    launch(env, unit);
    done(env, unit, "build");
    launch(env, unit);
    env.engine
        .park_with(
            unit,
            RequestKind::Stall,
            Cause::Stalled,
            "stalled: no output",
            None,
        )
        .unwrap();
    env.engine
        .end(
            &format!("{unit}/review#1"),
            EndStatus::Failed,
            "stalled: no output",
        )
        .unwrap();
    request_of(env, unit)
}

/// Park the unit with a lost seat.
fn lost(env: &Env, unit: &str) -> RequestRow {
    launch(env, unit);
    env.engine
        .park_with(
            unit,
            RequestKind::SeatLost,
            Cause::SeatLost,
            "the seat is gone",
            None,
        )
        .unwrap();
    env.engine
        .end(
            &format!("{unit}/build#1"),
            EndStatus::Failed,
            "the seat is gone",
        )
        .unwrap();
    request_of(env, unit)
}

/// A unit parked at its merge with red checks: both stages done and a PR (number 7) recorded.
fn red_at_merge(env: &Env, unit: &str) -> RequestRow {
    launch(env, unit);
    done(env, unit, "build");
    launch(env, unit);
    env.engine
        .signal(
            unit,
            "review",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number: 7,
            }),
        )
        .unwrap();
    let mut script = PrScript::green("work/u1", "1111111111111111111111111111111111111111");
    script.checks = Checks::Red;
    env.forge.script(7, script);
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    request_of(env, unit)
}

fn facts() -> SettleFacts {
    SettleFacts {
        reason: "the seat stalled; try the stage again".to_string(),
        session: "S-1".to_string(),
        project: "P-1".to_string(),
        generation: 1,
        agent: ORCHESTRATOR_AGENT.to_string(),
        ..SettleFacts::default()
    }
}

fn ask(request: &RequestRow, act: Act, to: Option<&str>) -> SettleAsk {
    SettleAsk {
        request_id: request.id.clone(),
        revision: request.revision,
        act,
        to_stage: to.map(str::to_string),
        facts: facts(),
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

fn moved(result: Result<pf_engine::Admission, SettleError>) -> String {
    match result {
        Err(SettleError::Moved(why)) => why,
        other => panic!("the settle must be refused as moved, got {other:?}"),
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

fn settle_lines(env: &Env) -> Vec<String> {
    env.ledger_text()
        .lines()
        .filter(|l| l.contains(" settle R-"))
        .map(str::to_string)
        .collect()
}

fn claim(env: &Env) -> u64 {
    env.engine
        .claim("op@host", &sha256_hex(b"a token"))
        .unwrap()
        .generation
}

fn bound_of(env: &Env) -> Option<u64> {
    env.store.run("run-1").unwrap().unwrap().launch_bound
}

// ---------------------------------------------------------------- the acts

#[test]
fn acts_of_gives_the_row_of_step_1_for_every_cause_and_exactly_five_admit_an_act() {
    use Act::*;
    let mut admit = 0;
    for cause in Cause::ALL {
        let expected: &[Act] = match cause {
            Cause::Stalled => &[Clear, Redispatch],
            Cause::SeatLost | Cause::HostUnreachable => &[Resume],
            Cause::MergeChecksRed => &[RerunCheck, Redispatch],
            Cause::MergeConflict => &[ResolveConflict],
            _ => &[],
        };
        assert_eq!(acts_of(cause), expected, "{cause:?}");
        if !expected.is_empty() {
            admit += 1;
        }
    }
    assert_eq!(admit, 5);
}

#[test]
fn the_acts_name_themselves_in_the_order_of_the_adr_and_four_of_them_launch() {
    let names: Vec<&str> = Act::ALL.iter().map(|a| a.as_str()).collect();
    assert_eq!(
        names,
        [
            "clear",
            "resume",
            "rerun_check",
            "redispatch",
            "resolve_conflict",
            "close_stale"
        ]
    );
    for act in Act::ALL {
        assert_eq!(Act::parse(act.as_str()), Some(act));
    }
    assert_eq!(Act::parse("Clear"), None);
    let launching: Vec<&str> = Act::ALL
        .iter()
        .filter(|a| a.launches())
        .map(|a| a.as_str())
        .collect();
    assert_eq!(
        launching,
        ["clear", "resume", "redispatch", "resolve_conflict"]
    );
}

// ---------------------------------------------------------------- the acts' effects

#[test]
fn clear_on_a_stall_resumes_the_unit_closes_the_request_and_keeps_the_counters() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    // The unit has been sent on once by an outcome: a settle resets nothing.
    let mut node = env.store.node("run-1", "u1").unwrap();
    let mut before = detail(&env, "u1");
    before.visits.insert("build#0".to_string(), 1);
    node.detail = before.to_text();
    store_commit(&env, "visits", vec![Effect::PutNode(node)]);
    let generation_before = detail(&env, "u1").park_generation;

    let admission = settle(&env, ask(&request, Act::Clear, None)).unwrap();
    assert!(!admission.replayed);
    assert_eq!(admission.row.kind, "settle");
    assert_eq!(admission.row.command_id, "settle:R-1:1");
    assert_eq!(admission.row.unit_id.as_deref(), Some("u1"));

    let closed = request_of(&env, "u1");
    assert_eq!(closed.status, RequestStatus::Answered);
    assert_eq!(closed.answer_generation, None, "the answer is the claim's");
    let answer = answer_of(&env, "R-1");
    assert_eq!(answer["verb"], "settle");
    assert_eq!(answer["act"], "clear");
    assert_eq!(answer.get("to_stage"), None);
    assert_eq!(answer["reason"], "the seat stalled; try the stage again");
    assert_eq!(
        answer["grant"],
        json!({"project": "P-1", "generation": 1, "agent": "orchestrator", "session": "S-1"})
    );
    assert_eq!(
        answer["line"],
        json!({"class": "below", "cause": "stalled"})
    );

    assert_eq!(status(&env, "u1"), Status::Running);
    let after = detail(&env, "u1");
    assert_eq!(after.stages_done, 0, "it resumes at the stage it parked in");
    assert_eq!(after.visits, before.visits, "a settle keeps the counters");
    assert_eq!(after.park_generation, generation_before + 1);
    let lines = settle_lines(&env);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with(
            "settle R-1@1 by grant P-1#1 session S-1: act=clear reason=the seat stalled; try the stage again"
        ),
        "{}",
        lines[0]
    );

    // The unit launches again and parks again, with a new `park` command.
    launch(&env, "u1");
    env.engine
        .park_with(
            "u1",
            RequestKind::Stall,
            Cause::Stalled,
            "stalled again",
            None,
        )
        .unwrap();
    assert!(admitted(&env, "park:u1:0"));
    assert!(admitted(&env, "park:u1:1"));
    assert_eq!(request_of(&env, "u1").id, "R-2");
}

#[test]
fn resume_on_a_lost_seat_does_the_same_at_the_parked_stage() {
    let env = rig(&["u1"], Some(10));
    let request = lost(&env, "u1");
    settle(&env, ask(&request, Act::Resume, None)).unwrap();
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Answered);
    assert_eq!(answer_of(&env, "R-1")["act"], "resume");
    assert_eq!(
        answer_of(&env, "R-1")["line"],
        json!({"class": "below", "cause": "seat_lost"})
    );
    assert_eq!(status(&env, "u1"), Status::Running);
    assert_eq!(detail(&env, "u1").stages_done, 0);
}

// ---------------------------------------------------------------- --to

#[test]
fn redispatch_takes_a_stage_at_or_before_the_parked_one_and_never_a_later_one() {
    // Parked in the first stage: the parked stage is admitted, a later stage is not.
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let why = refused(settle(&env, ask(&request, Act::Redispatch, Some("review"))));
    assert_eq!(
        why,
        "--to review is after the stage the unit parked in (build): a settle never skips a stage"
    );
    assert!(!admitted(&env, "settle:R-1:1"));
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(status(&env, "u1"), Status::Parked);
    settle(&env, ask(&request, Act::Redispatch, Some("build"))).unwrap();
    assert_eq!(detail(&env, "u1").stages_done, 0);
    let answer = answer_of(&env, "R-1");
    assert_eq!(answer["act"], "redispatch");
    assert_eq!(answer["to_stage"], "build");

    // Parked in the second stage: an earlier stage is admitted.
    let env = rig(&["u2"], Some(10));
    let request = stall_in_review(&env, "u2");
    settle(&env, ask(&request, Act::Redispatch, Some("build"))).unwrap();
    assert_eq!(detail(&env, "u2").stages_done, 0);

    // Parked in the second stage: the parked stage itself is admitted too.
    let env = rig(&["u3"], Some(10));
    let request = stall_in_review(&env, "u3");
    settle(&env, ask(&request, Act::Redispatch, Some("review"))).unwrap();
    assert_eq!(detail(&env, "u3").stages_done, 1);
}

#[test]
fn a_stage_the_track_lacks_is_refused_with_the_tracks_stages() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let why = refused(settle(&env, ask(&request, Act::Redispatch, Some("deploy"))));
    assert_eq!(
        why,
        "\"deploy\" is not a stage of track item (its stages: build, review)"
    );
    assert!(!admitted(&env, "settle:R-1:1"));
}

#[test]
fn a_redispatch_with_no_to_and_a_clear_with_a_to_are_refused() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let why = refused(settle(&env, ask(&request, Act::Redispatch, None)));
    assert!(why.contains("redispatch needs --to"), "{why}");
    let why = refused(settle(&env, ask(&request, Act::Clear, Some("build"))));
    assert!(why.contains("--to belongs to redispatch only"), "{why}");
    assert!(!admitted(&env, "settle:R-1:1"));
    // Neither refusal used the command ID: the right act is admitted on the same revision.
    settle(&env, ask(&request, Act::Clear, None)).unwrap();
}

#[test]
fn a_redispatch_to_the_first_stage_of_a_unit_parked_at_its_merge_runs_the_review_before_the_merge()
{
    let env = rig(&["u1"], Some(10));
    let request = red_at_merge(&env, "u1");
    assert_eq!(request.stage, "merge");
    assert_eq!(detail(&env, "u1").stages_done, 2);
    settle(&env, ask(&request, Act::Redispatch, Some("build"))).unwrap();
    assert_eq!(
        detail(&env, "u1").stages_done,
        0,
        "no stage keeps a done mark"
    );
    // The merge rule does not run while stages are left.
    let step = env.engine.merge_next().unwrap();
    assert!(
        matches!(step, MergeStep::Waiting { ref why, .. } if why.contains("has stages left")),
        "{step:?}"
    );
    launch(&env, "u1");
    done(&env, "u1", "build");
    // The review stage launches again, as a new attempt, before the merge.
    launch(&env, "u1");
    assert!(admitted(&env, "launch:u1:review:2"));
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Waiting { .. }), "{step:?}");
    env.engine
        .signal(
            "u1",
            "review",
            Signal::PrState {
                state: "open".to_string(),
            },
            None,
        )
        .unwrap();
    // Only now does the merge rule run: the checks are still red, so it parks the unit again.
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    assert_eq!(request_of(&env, "u1").stage, "merge");
}

// ---------------------------------------------------------------- the halt

#[test]
fn a_halted_run_refuses_clear_resume_and_redispatch_and_changes_nothing() {
    let env = rig(&["a", "b", "c"], Some(10));
    let ra = stall(&env, "a");
    let rb = lost(&env, "b");
    let rc = stall(&env, "c");
    env.engine.halt("operator stopped the run").unwrap();
    let admissions = env.store.admissions("run-1").unwrap().len();
    let nodes_before: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|u| env.store.node("run-1", u).unwrap())
        .collect();
    for (request, act, to) in [
        (&ra, Act::Clear, None),
        (&rb, Act::Resume, None),
        (&rc, Act::Redispatch, Some("build")),
    ] {
        let why = refused(settle(&env, ask(request, act, to)));
        assert_eq!(
            why,
            "run run-1 is halted: a halted run refuses every settle"
        );
        assert!(!admitted(&env, &format!("settle:{}:1", request.id)));
        assert_eq!(request_of_id(&env, &request.id).status, RequestStatus::Open);
    }
    assert_eq!(env.store.admissions("run-1").unwrap().len(), admissions);
    let nodes_after: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|u| env.store.node("run-1", u).unwrap())
        .collect();
    assert_eq!(nodes_before, nodes_after);
}

fn request_of_id(env: &Env, id: &str) -> RequestRow {
    env.store
        .all_requests()
        .into_iter()
        .find(|r| r.id == id)
        .unwrap()
}

// ---------------------------------------------------------------- the refusal texts

#[test]
fn a_request_revised_before_the_settle_is_refused_as_moved_with_the_revision_text() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let body: Value = serde_json::from_str(&request.body).unwrap();
    env.engine.revise_request("R-1", 1, &body).unwrap();
    let why = moved(settle(&env, ask(&request, Act::Clear, None)));
    assert_eq!(why, "request R-1 is at revision 2, not 1");
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(status(&env, "u1"), Status::Parked);
    assert!(!admitted(&env, "settle:R-1:1"));
}

#[test]
fn a_request_the_claim_closed_before_the_settle_is_refused_as_moved_with_the_closed_text() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let generation = claim(&env);
    env.engine
        .clear_unit("u1", None, generation, "R-1")
        .unwrap();
    let before = env.store.node("run-1", "u1").unwrap();
    let why = moved(settle(&env, ask(&request, Act::Clear, None)));
    assert_eq!(why, "request R-1 is not open any more");
    assert_eq!(env.store.node("run-1", "u1").unwrap(), before);
    assert!(!admitted(&env, "settle:R-1:1"));
}

#[test]
fn an_unknown_request_is_refused() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let mut ask = ask(&request, Act::Clear, None);
    ask.request_id = "R-99".to_string();
    let why = refused(settle(&env, ask));
    assert_eq!(why, "request R-99 is unknown in this run");
}

fn command_of(request: &RequestRow, act: Act, to: Option<&str>) -> Command {
    Command {
        id: format!("settle:{}:{}", request.id, request.revision),
        body: CommandBody::Settle {
            request_id: request.id.clone(),
            revision: request.revision,
            act,
            to_stage: to.map(str::to_string),
            facts: facts(),
        },
    }
}

#[test]
fn the_decisions_own_guard_refuses_a_moved_revision_and_a_closed_request_through_submit() {
    let env = rig(&["u1", "u2"], Some(10));
    let r1 = stall(&env, "u1");
    let r2 = stall(&env, "u2");
    // A moved revision.
    let body: Value = serde_json::from_str(&r1.body).unwrap();
    env.engine.revise_request(&r1.id, 1, &body).unwrap();
    let before = env.store.admissions("run-1").unwrap().len();
    let err = env
        .engine
        .submit(command_of(&r1, Act::Clear, None))
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Rejected("request R-1 is at revision 2, not 1".to_string())
    );
    // A closed request.
    let generation = claim(&env);
    env.engine
        .clear_unit("u2", None, generation, &r2.id)
        .unwrap();
    let before = before + 2;
    let err = env
        .engine
        .submit(command_of(&r2, Act::Clear, None))
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Rejected("request R-2 is not open any more".to_string())
    );
    assert_eq!(env.store.admissions("run-1").unwrap().len(), before);
    assert!(!admitted(&env, "settle:R-1:1"));
    assert!(!admitted(&env, "settle:R-2:1"));
}

#[test]
fn a_settle_command_with_no_grant_facts_is_refused_by_the_decision() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let mut command = command_of(&request, Act::Clear, None);
    if let CommandBody::Settle { facts, .. } = &mut command.body {
        *facts = SettleFacts::default();
    }
    let err = env.engine.submit(command).unwrap_err();
    assert!(
        matches!(&err, EngineError::Rejected(why) if why.contains("names no grant")),
        "{err:?}"
    );
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
}

#[test]
fn a_settle_command_with_no_cause_is_refused_and_the_class_of_no_cause_is_above() {
    let env = rig(&["u1"], Some(10));
    launch(&env, "u1");
    // A request with no decision block: its body holds none.
    env.engine
        .park_as("u1", RequestKind::Park, Cause::Stalled, "parked")
        .unwrap();
    env.engine
        .end("u1/build#1", EndStatus::Failed, "parked")
        .unwrap();
    let request = request_of(&env, "u1");
    let mut stripped = request.clone();
    let mut body: Value = serde_json::from_str(&request.body).unwrap();
    body.as_object_mut().unwrap().remove("decision");
    stripped.body = body.to_string();
    store_commit(&env, "strip", vec![Effect::PutRequest(stripped)]);
    let why = refused(settle(&env, ask(&request, Act::Clear, None)));
    assert_eq!(
        why,
        "request R-1 is above the line: only the claim may settle it"
    );
    let err = env
        .engine
        .submit(command_of(&request, Act::Clear, None))
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::Rejected(why) if why.contains("admits none")),
        "{err:?}"
    );
}

// ---------------------------------------------------------------- one step

#[test]
fn one_step_a_revise_that_waits_on_the_lock_is_refused_after_the_settle_committed() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let body: Value = serde_json::from_str(&request.body).unwrap();
    let (paused_tx, paused_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    env.engine.set_settle_pause(Some(Arc::new(move || {
        let _ = paused_tx.send(());
        let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(10));
    })));
    let engine = &env.engine;
    let (settled, revised) = std::thread::scope(|scope| {
        let settler = scope.spawn(|| engine.settle(ask(&request, Act::Clear, None), Some(FORGE)));
        // The settle holds the lock after its class read.
        paused_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = started.clone();
        let body = &body;
        let reviser = scope.spawn(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            engine.revise_request("R-1", 1, body)
        });
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::yield_now();
        }
        // Give the revise time to reach the lock, then let the settle go on.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            request_of(&env, "u1").status,
            RequestStatus::Open,
            "nothing moved while the settle holds the lock"
        );
        go_tx.send(()).unwrap();
        (settler.join().unwrap(), reviser.join().unwrap())
    });
    env.engine.set_settle_pause(None);
    settled.unwrap();
    assert_eq!(
        revised.unwrap_err(),
        EngineError::Rejected("request R-1 is answered, not open".to_string())
    );
    let closed = request_of(&env, "u1");
    assert_eq!(closed.status, RequestStatus::Answered);
    assert_eq!(closed.revision, 1, "the settle closed the revision it read");
}

// ---------------------------------------------------------------- the line

#[test]
fn a_request_above_the_line_is_refused_with_no_command_submitted() {
    let env = rig(&["u1"], Some(10));
    launch(&env, "u1");
    env.engine
        .park_with(
            "u1",
            RequestKind::Stop,
            Cause::StopFile,
            "stop: pick one",
            Some(&json!({
                "stop_text": "stop: pick one",
                "stop_options": [
                    { "text": "go on", "recommended": false },
                    { "text": "stop here", "recommended": true },
                ],
            })),
        )
        .unwrap();
    env.engine
        .end("u1/build#1", EndStatus::Failed, "stop file")
        .unwrap();
    let request = request_of(&env, "u1");
    let why = refused(settle(&env, ask(&request, Act::Clear, None)));
    assert_eq!(
        why,
        "request R-1 is above the line: only the claim may settle it"
    );
    assert!(!admitted(&env, "settle:R-1:1"));
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
}

// ---------------------------------------------------------------- the replay and the ID

#[test]
fn a_retry_with_another_reason_after_the_request_closed_returns_the_first_record() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let first = settle(&env, ask(&request, Act::Clear, None)).unwrap();
    assert!(!first.replayed);
    let mut retry = ask(&request, Act::Clear, None);
    retry.facts.reason = "a different reason".to_string();
    retry.facts.session = "S-9".to_string();
    let second = settle(&env, retry).unwrap();
    assert!(second.replayed);
    assert_eq!(second.row, first.row);
    assert_eq!(
        answer_of(&env, "R-1")["reason"],
        "the seat stalled; try the stage again"
    );
    assert_eq!(settle_lines(&env).len(), 1, "a replay writes no line");
    // Another act on the settled revision is a conflict.
    let other = ask(&request, Act::Redispatch, Some("build"));
    assert_eq!(settle(&env, other).unwrap_err(), SettleError::Conflict);
}

#[test]
fn only_an_admitted_settle_uses_its_command_id() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let why = refused(settle(&env, ask(&request, Act::RerunCheck, None)));
    assert_eq!(
        why,
        "the cause stalled admits clear, redispatch, not rerun_check"
    );
    let why = refused(settle(&env, ask(&request, Act::Resume, None)));
    assert_eq!(
        why,
        "the cause stalled admits clear, redispatch, not resume"
    );
    assert!(!admitted(&env, "settle:R-1:1"));
    settle(&env, ask(&request, Act::Clear, None)).unwrap();
    assert!(admitted(&env, "settle:R-1:1"));
}

#[test]
fn the_content_holds_the_act_and_to_and_nothing_else() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let base = command_of(&request, Act::Clear, None);
    let content = |c: &Command| canonical_content("run-1", c);
    let mut other_facts = base.clone();
    if let CommandBody::Settle { facts, .. } = &mut other_facts.body {
        facts.reason = "another".to_string();
        facts.session = "S-7".to_string();
        facts.generation = 9;
        facts.project = "P-9".to_string();
    }
    assert_eq!(content(&base), content(&other_facts));
    assert_eq!(
        content(&base),
        r#"{"run":"run-1","id":"settle:R-1:1","body":{"kind":"settle","request_id":"R-1","revision":1,"act":"clear"}}"#
    );
    let other_act = command_of(&request, Act::Redispatch, None);
    assert_ne!(content(&base), content(&other_act));
    let to_a = command_of(&request, Act::Redispatch, Some("build"));
    let to_b = command_of(&request, Act::Redispatch, Some("review"));
    assert_ne!(content(&other_act), content(&to_a));
    assert_ne!(content(&to_a), content(&to_b));
}

#[test]
fn two_retries_with_other_reasons_at_once_give_one_admission_and_one_replay() {
    for _ in 0..20 {
        let env = rig(&["u1"], Some(10));
        let request = stall(&env, "u1");
        let engine = &env.engine;
        let results: Vec<(String, Result<pf_engine::Admission, SettleError>)> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = ["first reason", "second reason"]
                    .into_iter()
                    .map(|reason| {
                        let mut a = ask(&request, Act::Clear, None);
                        a.facts.reason = reason.to_string();
                        scope.spawn(move || (reason.to_string(), engine.settle(a, Some(FORGE))))
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
        let ok: Vec<_> = results
            .iter()
            .map(|(reason, r)| (reason.clone(), r.as_ref().expect("never a conflict")))
            .collect();
        assert_eq!(ok.iter().filter(|(_, a)| !a.replayed).count(), 1);
        assert_eq!(ok.iter().filter(|(_, a)| a.replayed).count(), 1);
        let winner = &ok.iter().find(|(_, a)| !a.replayed).unwrap().0;
        assert_eq!(answer_of(&env, "R-1")["reason"], winner.as_str());
        let lines = settle_lines(&env);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].ends_with(&format!("reason={winner}")), "{lines:?}");
    }
}

// ---------------------------------------------------------------- the bound

#[test]
fn a_run_with_no_stored_count_refuses_every_launching_act() {
    let env = rig(&["a", "b", "c"], None);
    let ra = stall(&env, "a");
    let rb = lost(&env, "b");
    let rc = stall(&env, "c");
    assert_eq!(bound_of(&env), None);
    for (request, act, to) in [
        (&ra, Act::Clear, None),
        (&rb, Act::Resume, None),
        (&rc, Act::Redispatch, Some("build")),
    ] {
        let why = refused(settle(&env, ask(request, act, to)));
        assert_eq!(
            why,
            "run run-1 holds no armed launch count, so the grant cannot launch in it"
        );
        assert_eq!(request_of_id(&env, &request.id).status, RequestStatus::Open);
    }
    // The claim's clear is not bound: it is admitted with no count.
    let generation = claim(&env);
    env.engine.clear_unit("a", None, generation, "R-1").unwrap();
}

#[test]
fn at_the_count_a_launching_settle_is_refused_and_one_below_it_is_admitted() {
    // Two units have launched once each: two launches so far.
    let env = rig(&["a", "b"], Some(2));
    let ra = stall(&env, "a");
    let _rb = stall(&env, "b");
    let why = refused(settle(&env, ask(&ra, Act::Clear, None)));
    assert_eq!(
        why,
        "the run's launches so far (2, 2 admitted and 0 resumed and waiting to launch) have reached its armed launch count (2); the grant cannot launch more in this run; the operator can still clear"
    );
    assert!(why.contains(BOUND_TEXT));
    assert_eq!(request_of(&env, "a").status, RequestStatus::Open);
    assert_eq!(status(&env, "a"), Status::Parked, "the unit keeps its park");
    assert!(!admitted(&env, "settle:R-1:1"));

    let env = rig(&["a", "b"], Some(3));
    let ra = stall(&env, "a");
    let _rb = stall(&env, "b");
    settle(&env, ask(&ra, Act::Clear, None)).unwrap();
    assert_eq!(status(&env, "a"), Status::Running);
}

#[test]
fn launches_so_far_counts_every_admitted_launch_and_each_resume_waiting_to_launch() {
    let env = rig(&["a", "b", "c", "d"], Some(100));
    let ra = stall(&env, "a");
    let rb = stall(&env, "b");
    let rc = stall(&env, "c");
    let rd = stall(&env, "d");
    let count = |env: &Env| {
        let view = env.engine.view().unwrap();
        let c = launches_so_far(&view);
        (c.admitted, c.pending)
    };
    assert_eq!(count(&env), (4, 0));
    // A settle, a claim's clear and a claim's answer each add one waiting resume.
    settle(&env, ask(&ra, Act::Clear, None)).unwrap();
    assert_eq!(count(&env), (4, 1));
    let generation = claim(&env);
    env.engine
        .clear_unit("b", None, generation, &rb.id)
        .unwrap();
    assert_eq!(count(&env), (4, 2));
    env.engine
        .answer_request(&rc.id, rc.revision, generation, "go on")
        .unwrap();
    assert_eq!(count(&env), (4, 3));
    // A unit that launches moves from the second number to the first: it is counted once.
    launch(&env, "a");
    assert_eq!(count(&env), (5, 2));
    launch(&env, "b");
    launch(&env, "c");
    assert_eq!(count(&env), (7, 0));
    // A resumed unit that parks again before it launches is not counted.
    settle(&env, ask(&rd, Act::Clear, None)).unwrap();
    assert_eq!(count(&env), (7, 1));
    env.engine
        .park_with(
            "d",
            RequestKind::Stall,
            Cause::Stalled,
            "stalled again",
            None,
        )
        .unwrap();
    assert_eq!(count(&env), (7, 0));
}

#[test]
fn an_ack_adds_nothing_to_the_launches_so_far() {
    // A unit parked at a merge_ack request that the claim acked is `running` again, ready to merge:
    // the ack sends it to its merge, and the merge launches nothing. The view below is the stored
    // one with that unit running and an `ack_request` admission after its last launch.
    let mut wf = rig_workflow();
    wf.tracks[0].merge.as_mut().unwrap().rule = MergeRule::ClaimAck;
    let env = env_with_params(wf, BTreeMap::new());
    env.engine
        .ingest(&mut TrackedSource {
            track: "item".to_string(),
            records: vec![UnitRecord::new("u1", "g1", 0, BTreeMap::new())],
        })
        .unwrap();
    launch(&env, "u1");
    done(&env, "u1", "build");
    launch(&env, "u1");
    env.engine
        .signal(
            "u1",
            "review",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number: 7,
            }),
        )
        .unwrap();
    let mut script = PrScript::green("work/u1", "1111111111111111111111111111111111111111");
    script.base = "spine".to_string();
    env.forge.script(7, script);
    env.engine.merge_next().unwrap();
    let request = request_of(&env, "u1");
    assert_eq!(request.kind, RequestKind::MergeAck);
    let mut view = env.engine.view().unwrap();
    let before = launches_so_far(&view);
    assert_eq!((before.admitted, before.pending), (2, 0));
    let seq = view.admissions.iter().map(|a| a.seq).max().unwrap() + 1;
    let mut row = view.admissions[0].clone();
    row.seq = seq;
    row.command_id = format!("ack:{}:1", request.id);
    row.kind = "ack_request".to_string();
    row.unit_id = Some("u1".to_string());
    view.admissions.push(row);
    for node in &mut view.nodes {
        if node.id == "u1" {
            node.status = Status::Running;
        }
    }
    let after = launches_so_far(&view);
    assert_eq!(
        (after.admitted, after.pending),
        (2, 0),
        "an ack is not a resume"
    );
}

#[test]
fn the_race_two_units_room_for_one_launch_and_two_threads_admit_exactly_one() {
    for round in 0..40 {
        // Two launches so far, and room for one more.
        let env = rig(&["a", "b"], Some(3));
        let ra = stall(&env, "a");
        let rb = stall(&env, "b");
        let engine = &env.engine;
        let results: Vec<Result<pf_engine::Admission, SettleError>> = std::thread::scope(|scope| {
            let handles: Vec<_> = [&ra, &rb]
                .into_iter()
                .map(|r| {
                    let a = ask(r, Act::Clear, None);
                    scope.spawn(move || engine.settle(a, Some(FORGE)))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let admitted_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(admitted_count, 1, "round {round}: {results:?}");
        let refusal = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert!(
            matches!(refusal, SettleError::Refused(why) if why.contains(BOUND_TEXT)),
            "round {round}: {refusal:?}"
        );
        // The refused one stays open and parked.
        let open: Vec<_> = env
            .store
            .all_requests()
            .into_iter()
            .filter(|r| r.status == RequestStatus::Open)
            .collect();
        assert_eq!(open.len(), 1, "round {round}");
        // After the ticks the run's admitted launches do not pass the count.
        for unit in ["a", "b"] {
            if status(&env, unit) == Status::Running {
                launch(&env, unit);
            }
        }
        let view = env.engine.view().unwrap();
        let count = launches_so_far(&view);
        assert!(count.admitted <= 3, "round {round}: {count:?}");
        assert_eq!(count.pending, 0);
    }
}

#[test]
fn a_resumed_unit_that_parks_again_before_it_launches_gives_its_room_back() {
    let env = rig(&["a", "b"], Some(3));
    let ra = stall(&env, "a");
    let rb = stall(&env, "b");
    settle(&env, ask(&ra, Act::Clear, None)).unwrap();
    // No room is left: the second settle is refused.
    let why = refused(settle(&env, ask(&rb, Act::Clear, None)));
    assert!(why.contains(BOUND_TEXT), "{why}");
    // The first unit parks again before it launched: its room comes back.
    env.engine
        .park_with(
            "a",
            RequestKind::Stall,
            Cause::Stalled,
            "stalled before the launch",
            None,
        )
        .unwrap();
    settle(&env, ask(&rb, Act::Clear, None)).unwrap();
    assert_eq!(status(&env, "b"), Status::Running);
}

#[test]
fn the_claims_pending_clear_counts_against_a_settle_and_the_claims_clear_is_not_bound() {
    let env = rig(&["a", "b"], Some(3));
    let ra = stall(&env, "a");
    let rb = stall(&env, "b");
    let generation = claim(&env);
    env.engine
        .clear_unit("a", None, generation, &ra.id)
        .unwrap();
    // Before any tick, the settle for the second unit is refused at the bound.
    let why = refused(settle(&env, ask(&rb, Act::Clear, None)));
    assert!(why.contains(BOUND_TEXT), "{why}");
    assert_eq!(request_of(&env, "b").status, RequestStatus::Open);
    // After the ticks the run's launches do not pass the count.
    launch(&env, "a");
    let count = launches_so_far(&env.engine.view().unwrap());
    assert_eq!((count.admitted, count.pending), (3, 0));
    // The same clear at the bound is admitted: the claim's clear is not bound.
    env.engine
        .clear_unit("b", None, generation, &rb.id)
        .unwrap();
    assert_eq!(status(&env, "b"), Status::Running);
}

// ---------------------------------------------------------------- the grant and the line record

#[test]
fn the_commit_carries_the_grants_precondition_and_a_revoked_generation_writes_nothing() {
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    // The facts name a generation that is not live.
    let mut stale = ask(&request, Act::Clear, None);
    stale.facts.generation = 2;
    let why = refused(settle(&env, stale));
    assert_eq!(
        why,
        "the clear grant of project P-1 is live at generation 1, not 2"
    );
    assert!(!admitted(&env, "settle:R-1:1"));
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(status(&env, "u1"), Status::Parked);
    // A revoke that committed first rejects the whole commit.
    revoke_grant(&env);
    let why = refused(settle(&env, ask(&request, Act::Clear, None)));
    assert_eq!(why, "project P-1 holds no live clear grant");
    assert!(!admitted(&env, "settle:R-1:1"));
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(status(&env, "u1"), Status::Parked);
    assert!(settle_lines(&env).is_empty());
}

#[test]
fn the_stored_line_record_holds_the_class_the_cause_and_for_a_red_check_the_merge_inputs() {
    let env = rig(&["u1"], Some(10));
    let request = red_at_merge(&env, "u1");
    settle(&env, ask(&request, Act::Redispatch, Some("build"))).unwrap();
    assert_eq!(
        answer_of(&env, "R-1")["line"],
        json!({
            "class": "below",
            "cause": "merge_checks_red",
            "rule": "green_checks_in_declared_order",
            "base": "spine",
            "repo": "synthetic/repo",
            "default_branch": "main",
        })
    );
}

#[test]
fn a_replay_of_a_settle_by_hand_is_the_stored_record() {
    // The replay lookup is the first step of `Engine::settle`: a degraded-free engine answers it
    // from the store although the request is closed and the grant is revoked.
    let env = rig(&["u1"], Some(10));
    let request = stall(&env, "u1");
    let first = settle(&env, ask(&request, Act::Clear, None)).unwrap();
    revoke_grant(&env);
    let again = settle(&env, ask(&request, Act::Clear, None)).unwrap();
    assert!(again.replayed);
    assert_eq!(again.row, first.row);
}
