//! AC-3: a unit can park, be cleared, run and park again. Every command ID that is unique per unit
//! carries the unit's park generation (`park:<unit>:<gen>`, `merge:<unit>:<gen>`,
//! `merge-result:<unit>:<gen>`), so a resume uses new IDs and a replay of an old one changes
//! nothing. Every park opens a request, and a merge that did not land goes through Park.

mod common;

use common::*;
use pf_engine::claim::sha256_hex;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::{Checks, ForgeOutcome, PrRef};
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus, Store};
use pf_engine::unit::{Signal, UnitDetail};
use pf_engine::{Command, CommandBody, EndStatus, EngineError, Status};

fn requests(env: &Env) -> Vec<RequestRow> {
    env.engine.requests().unwrap()
}

fn detail(env: &Env, id: &str) -> UnitDetail {
    let view = env.engine.view().unwrap();
    view.unit_detail(view.node(id).unwrap()).unwrap()
}

fn claim(env: &Env) -> u64 {
    env.engine
        .claim("op@host", &sha256_hex(b"a token"))
        .unwrap()
        .generation
}

fn admitted(env: &Env, command_id: &str) -> bool {
    env.store.admission("run-1", command_id).unwrap().is_some()
}

fn open_pr(env: &Env, unit: &str, stage: &str, number: u64) {
    env.engine
        .signal(
            unit,
            stage,
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number,
            }),
        )
        .unwrap();
}

#[test]
fn a_unit_parks_is_cleared_runs_and_parks_again() {
    let env = env(item_workflow(&["implement", "verify"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine
        .park_as("u1", RequestKind::Stall, "stalled: no output")
        .unwrap();
    env.engine
        .end("u1/implement#1", EndStatus::Failed, "stalled: no output")
        .unwrap();
    assert_eq!(detail(&env, "u1").park_generation, 0);
    assert!(admitted(&env, "park:u1:0"));

    let generation = claim(&env);
    env.engine
        .clear_unit("u1", None, generation, "R-1")
        .unwrap();
    assert!(admitted(&env, "clear:u1:0"));
    assert_eq!(env.status_of("u1"), (Status::Running, None));
    assert_eq!(detail(&env, "u1").park_generation, 1);
    assert_eq!(detail(&env, "u1").stages_done, 0);
    let first = &requests(&env)[0];
    assert_eq!(first.status, RequestStatus::Answered);
    assert_eq!(first.answer_generation, Some(generation));
    assert_eq!(
        first.answer.as_deref(),
        Some(r#"{"to_stage":"implement","verb":"clear"}"#)
    );
    assert!(
        env.ledger_text()
            .contains("answer R-1@1 by claim 1: clear u1 --to implement"),
        "{}",
        env.ledger_text()
    );

    // The next launch is the second attempt of the same stage, under a new command ID.
    env.engine.start_stage("u1").unwrap();
    assert!(admitted(&env, "launch:u1:implement:2"));
    assert_eq!(env.agent.launch_count(), 2);

    // The unit parks again. The second park has its own ID and opens a second request.
    env.engine
        .park_as("u1", RequestKind::Stop, "stop file")
        .unwrap();
    assert!(admitted(&env, "park:u1:1"));
    let all = requests(&env);
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].id, "R-2");
    assert_eq!(all[1].kind, RequestKind::Stop);
    assert_eq!(all[1].status, RequestStatus::Open);
    assert_eq!(
        all[0].status,
        RequestStatus::Answered,
        "the first stays answered"
    );
    assert_eq!(env.status_of("u1").0, Status::Parked);
}

#[test]
fn a_replay_at_an_old_generation_returns_the_stored_receipt_and_changes_nothing() {
    let env = env(item_workflow(&["implement"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    let first = env.engine.park("u1", "stalled").unwrap();
    env.engine
        .end("u1/implement#1", EndStatus::Failed, "stalled")
        .unwrap();
    let generation = claim(&env);
    env.engine
        .clear_unit("u1", None, generation, "R-1")
        .unwrap();
    assert_eq!(env.status_of("u1").0, Status::Running);

    let replay = env
        .engine
        .submit(Command {
            id: "park:u1:0".to_string(),
            body: CommandBody::Park {
                unit_id: "u1".to_string(),
                reason: "stalled".to_string(),
                request_kind: RequestKind::Park,
            },
        })
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.row, first.row);
    assert_eq!(
        env.status_of("u1").0,
        Status::Running,
        "the replay parked nothing"
    );
    assert_eq!(requests(&env).len(), 1, "and opened nothing");

    // A second clear of the same request finds the unit running: nothing resumes twice.
    let again = env.engine.clear_unit("u1", None, generation, "R-1");
    assert!(
        matches!(&again, Err(EngineError::Rejected(why)) if why.contains("not parked")),
        "{again:?}"
    );
    // Replaying the stored clear command itself returns its receipt.
    let stored = env.store.admission("run-1", "clear:u1:0").unwrap().unwrap();
    let replay = env
        .engine
        .submit(Command {
            id: "clear:u1:0".to_string(),
            body: pf_engine::admission::body_of_content(&stored.content).unwrap(),
        })
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(detail(&env, "u1").park_generation, 1, "no second resume");
}

#[test]
fn a_cleared_unit_merges_with_the_next_generation_and_a_replay_of_the_old_merge_changes_nothing() {
    let env = env(workflow(&["build"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    // The first merge is admitted and the forge reports a conflict.
    let mut script = PrScript::green("work/u1", "sha-u1");
    script.merge = ForgeOutcome::Conflict;
    env.forge.script(1, script);
    env.engine.start_stage("u1").unwrap();
    open_pr(&env, "u1", "build", 1);

    let step = env.engine.merge_next().unwrap();
    assert_eq!(
        step,
        MergeStep::Parked {
            unit: "u1".to_string(),
            reason: "merge conflict".to_string()
        }
    );
    assert!(admitted(&env, "merge:u1:0"));
    assert!(admitted(&env, "merge-result:u1:0"));
    assert!(admitted(&env, "park:u1:0"));
    let r = &requests(&env)[0];
    assert_eq!(
        (r.kind, r.stage.as_str()),
        (RequestKind::MergeFailed, "merge")
    );
    let keeps_pr = detail(&env, "u1").pr;
    assert!(keeps_pr.is_some());

    // The claim clears the unit. Its PR stays, and the cause is fixed.
    let generation = claim(&env);
    env.engine
        .clear_unit("u1", None, generation, "R-1")
        .unwrap();
    assert_eq!(
        detail(&env, "u1").pr,
        keeps_pr,
        "a cleared unit keeps its PR"
    );
    assert_eq!(detail(&env, "u1").stages_done, 1, "every stage stays done");
    assert!(
        env.ledger_text().contains("clear u1 --to merge"),
        "{}",
        env.ledger_text()
    );
    env.forge.scripts.lock().unwrap().get_mut(&1).unwrap().merge = ForgeOutcome::Merged;

    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Merged { .. }), "{step:?}");
    assert!(admitted(&env, "merge:u1:1"));
    assert!(admitted(&env, "merge-result:u1:1"));
    assert_eq!(
        env.status_of("u1"),
        (Status::Done, Some("merged at sha-u1".to_string()))
    );

    // A replay of the old merge returns its receipt and changes nothing.
    let lines_before = env.ledger_text().lines().count();
    let admissions_before = env.engine.view().unwrap().admissions.len();
    let replay = env
        .engine
        .submit(Command {
            id: "merge:u1:0".to_string(),
            body: CommandBody::Merge {
                unit_id: "u1".to_string(),
            },
        })
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        env.engine.view().unwrap().admissions.len(),
        admissions_before
    );
    assert_eq!(env.ledger_text().lines().count(), lines_before);
    assert_eq!(env.status_of("u1").0, Status::Done);
}

#[test]
fn a_merge_that_did_not_land_goes_through_park_and_opens_a_merge_failed_request() {
    let env = env(workflow(&["build"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    let mut script = PrScript::green("work/u1", "sha-u1");
    script.checks = Checks::Red;
    env.forge.script(1, script);
    env.engine.start_stage("u1").unwrap();
    open_pr(&env, "u1", "build", 1);

    env.engine.merge_next().unwrap();
    // The merge result carries no status change of its own: the Park command parked the unit.
    let view = env.engine.view().unwrap();
    let park = view
        .admissions
        .iter()
        .find(|a| a.command_id == "park:u1:0")
        .unwrap();
    assert_eq!(park.kind, "park");
    let result = view
        .admissions
        .iter()
        .find(|a| a.command_id == "merge-result:u1:0")
        .unwrap();
    assert!(
        park.seq > result.seq,
        "the result is recorded, then the park"
    );
    let r = &requests(&env)[0];
    assert_eq!(r.kind, RequestKind::MergeFailed);
    assert_eq!(r.reason, "checks red");
    assert_eq!(
        env.status_of("u1"),
        (Status::Parked, Some("checks red".to_string()))
    );
}

#[test]
fn a_stop_between_the_merge_result_and_the_park_is_finished_by_the_next_merge_step() {
    let env = env(workflow(&["build"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    let mut script = PrScript::green("work/u1", "sha-u1");
    script.checks = Checks::Red;
    env.forge.script(1, script);
    env.engine.start_stage("u1").unwrap();
    open_pr(&env, "u1", "build", 1);
    // The result was recorded with another text, and the controller stopped before the park.
    env.engine
        .submit(Command {
            id: "merge-result:u1:0".to_string(),
            body: CommandBody::MergeResult {
                unit_id: "u1".to_string(),
                repo: "synthetic/repo".to_string(),
                branch: "work/u1".to_string(),
                sha: "sha-u1".to_string(),
                outcome: "forge error: timeout".to_string(),
                landed: false,
            },
        })
        .unwrap();
    assert_eq!(env.status_of("u1").0, Status::Running);

    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    assert_eq!(env.status_of("u1").0, Status::Parked);
    assert_eq!(requests(&env)[0].kind, RequestKind::MergeFailed);
}

#[test]
fn clear_to_an_earlier_stage_runs_it_again_and_keeps_the_attempt_count() {
    let env = env(item_workflow(&["implement", "verify"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine
        .signal("u1", "implement", Signal::TurnEnd, None)
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine
        .park_as("u1", RequestKind::RedGate, "the gate is red")
        .unwrap();
    env.engine
        .end("u1/verify#1", EndStatus::Failed, "the gate is red")
        .unwrap();
    let r = &requests(&env)[0];
    assert_eq!(r.stage, "verify");
    assert_eq!(detail(&env, "u1").stages_done, 1);

    let generation = claim(&env);
    env.engine
        .clear_unit("u1", Some("implement"), generation, "R-1")
        .unwrap();
    assert_eq!(detail(&env, "u1").stages_done, 0);
    env.engine.start_stage("u1").unwrap();
    assert!(admitted(&env, "launch:u1:implement:2"));
    assert!(env.ledger_text().contains("clear u1 --to implement"));
}

#[test]
fn clear_refuses_what_it_must_and_changes_nothing() {
    let env = env(item_workflow(&["implement", "verify"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1), unit("u2", "g1", 2)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stalled").unwrap();
    let rejected = |r: Result<pf_engine::Admission, EngineError>| match r {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("expected a rejection, got {other:?}"),
    };

    // No claim yet.
    let why = rejected(env.engine.clear_unit("u1", None, 1, "R-1"));
    assert!(why.contains("no claim has been made"), "{why}");
    let generation = claim(&env);

    // The session of the parked unit still runs: the controller ends it first.
    let why = rejected(env.engine.clear_unit("u1", None, generation, "R-1"));
    assert!(why.contains("running session"), "{why}");
    env.engine
        .end("u1/implement#1", EndStatus::Failed, "stalled")
        .unwrap();

    // A stage of another track, a stage that does not exist, and the merge as a target.
    for bad in ["nowhere", "merge"] {
        let why = rejected(env.engine.clear_unit("u1", Some(bad), generation, "R-1"));
        assert!(
            why.contains(&format!("{bad:?}")) && why.contains("implement, verify"),
            "{why}"
        );
    }
    // A request that is not the unit's open one.
    let why = rejected(env.engine.clear_unit("u1", None, generation, "R-9"));
    assert!(why.contains("R-1") && why.contains("R-9"), "{why}");
    // A unit that is not parked.
    let why = rejected(env.engine.clear_unit("u2", None, generation, "R-1"));
    assert!(why.contains("not parked"), "{why}");
    let why = rejected(env.engine.clear_unit("nope", None, generation, "R-1"));
    assert!(why.contains("unknown"), "{why}");

    assert_eq!(env.status_of("u1").0, Status::Parked);
    assert_eq!(requests(&env)[0].status, RequestStatus::Open);
    assert_eq!(detail(&env, "u1").park_generation, 0);
}

#[test]
fn a_halted_run_accepts_the_clear_and_resumes_nothing() {
    let env = env(item_workflow(&["implement"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stalled").unwrap();
    env.engine
        .end("u1/implement#1", EndStatus::Failed, "stalled")
        .unwrap();
    env.engine.halt("operator stop").unwrap();
    let generation = claim(&env);
    env.engine
        .clear_unit("u1", None, generation, "R-1")
        .unwrap();

    // The clear is recorded: the unit runs again in the store and carries the halt reason.
    assert_eq!(
        env.status_of("u1"),
        (Status::Running, Some("run halted".to_string()))
    );
    assert_eq!(requests(&env)[0].status, RequestStatus::Answered);
    // The launch is refused by the halt: the clear started nothing.
    let launches = env.agent.launch_count();
    let step = env.engine.start_stage("u1").unwrap();
    assert!(
        matches!(step, pf_engine::launch::LaunchStep::Refused { .. }),
        "{step:?}"
    );
    assert_eq!(env.agent.launch_count(), launches);
}
