//! AC-2: stage outcomes in the engine. A stage may send the unit back to a stage of its own
//! track, up to a cap. The engine counts the visits of each entry with the unit, parks a unit
//! whose entry would pass its cap, skips a stage that only an outcome enters, and lets `clear`
//! grant a new budget.

mod common;

use common::*;
use pf_engine::claim::sha256_hex;
use pf_engine::launch::LaunchStep;
use pf_engine::merge::{MergeStep, Next, next_in_order};
use pf_engine::port::forge::PrRef;
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus};
use pf_engine::unit::{Signal, UnitDetail};
use pf_engine::workflow::{MergeDef, OutcomeDef, Track};
use pf_engine::{EngineError, MergeRule, StageDef, Status, Workflow};

/// The shape of the item track in the shipped package: `build`, then `review` and `fix`.
/// `review` sends a unit to `fix` (entry 0, at most 2 times) or to itself (entry 1, at most 2).
/// `fix` is entered only by an outcome, and sends the unit back to `review` (entry 0, at most 2).
fn loop_workflow() -> Workflow {
    let mut build = StageDef::new("build", "build-role", "model-a");
    build.outcomes = Vec::new();
    let mut review = StageDef::new("review", "review-role", "model-a");
    review.outcomes = vec![
        OutcomeDef {
            to: "fix".to_string(),
            max: 2,
        },
        OutcomeDef {
            to: "review".to_string(),
            max: 2,
        },
    ];
    let mut fix = StageDef::new("fix", "fix-role", "model-a");
    fix.entered_by_outcome = true;
    fix.outcomes = vec![OutcomeDef {
        to: "review".to_string(),
        max: 2,
    }];
    let mut wf = Workflow::single_track(
        "loop-flow",
        vec![build, review, fix],
        Some(MergeRule::GreenChecksInDeclaredOrder),
        pf_engine::capacity::Settings::default(),
    );
    wf.tracks[0].name = "item".to_string();
    wf
}

fn done(path: &str) -> Signal {
    Signal::DoneFile {
        path: path.to_string(),
    }
}

fn pr(n: u64) -> Option<PrRef> {
    Some(PrRef {
        repo: "synthetic/repo".to_string(),
        number: n,
    })
}

fn detail(env: &Env, id: &str) -> UnitDetail {
    let view = env.engine.view().unwrap();
    view.unit_detail(view.node(id).unwrap()).unwrap()
}

fn open_request(env: &Env, unit: &str) -> RequestRow {
    env.engine
        .view()
        .unwrap()
        .open_request_of(unit)
        .cloned()
        .unwrap_or_else(|| panic!("no open request for {unit}"))
}

fn launched(step: LaunchStep) -> String {
    match step {
        LaunchStep::Launched { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    }
}

/// A started env with `u1` past `build`, and `review` not yet launched.
fn at_review() -> Env {
    let env = env(loop_workflow());
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    assert_eq!(
        launched(env.engine.start_stage("u1").unwrap()),
        "u1/build#1"
    );
    env.engine
        .signal("u1", "build", done("build.done"), pr(1))
        .unwrap();
    env
}

/// Launch `stage` for `u1` and end it with the outcome `entry`.
fn visit(env: &Env, stage: &str, entry: usize) {
    launched(env.engine.start_stage("u1").unwrap());
    env.engine
        .outcome("u1", stage, entry, done("x"), None)
        .unwrap();
}

#[test]
fn a_unit_loops_review_fix_review_and_the_counters_count_each_visit() {
    let env = at_review();
    assert_eq!(detail(&env, "u1").stages_done, 1);
    assert!(detail(&env, "u1").visits.is_empty());

    // review sends the unit to fix (entry 0).
    assert_eq!(
        launched(env.engine.start_stage("u1").unwrap()),
        "u1/review#1"
    );
    env.engine
        .outcome("u1", "review", 0, done("review.md"), None)
        .unwrap();
    let d = detail(&env, "u1");
    assert_eq!(d.stages_done, 2, "the unit stands at fix");
    assert_eq!(d.visits.get("review#0"), Some(&1));
    assert_eq!(env.status_of("u1"), (Status::Running, None));
    assert_eq!(
        env.store.node("run-1", "u1/review#1").unwrap().status,
        Status::Done
    );

    // fix sends it back to review (entry 0 of fix).
    assert_eq!(launched(env.engine.start_stage("u1").unwrap()), "u1/fix#1");
    env.engine
        .outcome("u1", "fix", 0, done("fix-done"), None)
        .unwrap();
    let d = detail(&env, "u1");
    assert_eq!(d.stages_done, 1, "the unit stands at review again");
    assert_eq!(d.visits.get("fix#0"), Some(&1));
    assert_eq!(d.visits.get("review#0"), Some(&1));

    // The second visit of review is attempt 2 of that stage.
    assert_eq!(
        launched(env.engine.start_stage("u1").unwrap()),
        "u1/review#2"
    );

    // Entry 1 of review has a counter of its own.
    env.engine
        .outcome("u1", "review", 1, done("review.md"), None)
        .unwrap();
    let d = detail(&env, "u1");
    assert_eq!(d.stages_done, 1, "review sent the unit to review");
    assert_eq!(d.visits.get("review#1"), Some(&1));
    assert_eq!(d.visits.get("review#0"), Some(&1));
    assert_eq!(
        launched(env.engine.start_stage("u1").unwrap()),
        "u1/review#3"
    );

    // The ledger names the entry and the visit.
    let ledger = env.ledger_text();
    assert!(
        ledger.contains("stage review done (done-file review.md); outcome 1 sends the unit to fix (visit 1 of 2)"),
        "{ledger}"
    );
}

#[test]
fn the_third_block_parks_with_a_park_request_naming_the_cap() {
    let env = at_review();
    visit(&env, "review", 0);
    visit(&env, "fix", 0);
    visit(&env, "review", 0);
    visit(&env, "fix", 0);
    assert_eq!(detail(&env, "u1").visits.get("review#0"), Some(&2));

    // The third block would be visit 3 of 2.
    launched(env.engine.start_stage("u1").unwrap());
    env.engine
        .outcome("u1", "review", 0, done("review.md"), None)
        .unwrap();

    let (status, reason) = env.status_of("u1");
    assert_eq!(status, Status::Parked);
    assert_eq!(reason.as_deref(), Some("review: fix cap 2 reached"));
    let request = open_request(&env, "u1");
    assert_eq!(request.kind, RequestKind::Park);
    assert_eq!(request.reason, "review: fix cap 2 reached");
    assert_eq!(request.stage, "review");
    let d = detail(&env, "u1");
    assert_eq!(
        d.visits.get("review#0"),
        Some(&2),
        "a refused visit is not counted"
    );
    assert_eq!(d.stages_done, 1, "the unit stays at review");
    // The stage that asked is over, and its session with it.
    assert_eq!(
        env.store.node("run-1", "u1/review#3").unwrap().status,
        Status::Done
    );
    let view = env.engine.view().unwrap();
    assert!(view.running_stage_of("u1").is_none());
    assert!(view.session("u1/review#3").unwrap().ended);
    assert!(
        env.ledger_text().contains("review: fix cap 2 reached"),
        "{}",
        env.ledger_text()
    );
}

#[test]
fn any_clear_resets_every_counter_of_the_unit() {
    let env = at_review();
    visit(&env, "review", 0);
    visit(&env, "fix", 0);
    visit(&env, "review", 0);
    visit(&env, "fix", 0);
    visit(&env, "review", 1);
    launched(env.engine.start_stage("u1").unwrap());
    env.engine
        .outcome("u1", "review", 0, done("review.md"), None)
        .unwrap();
    assert_eq!(env.status_of("u1").0, Status::Parked);
    assert!(!detail(&env, "u1").visits.is_empty());

    let generation = env
        .engine
        .claim("op@host", &sha256_hex(b"a token"))
        .unwrap()
        .generation;
    let request = open_request(&env, "u1");
    env.engine
        .clear_unit("u1", Some("fix"), generation, &request.id)
        .unwrap();

    let d = detail(&env, "u1");
    assert!(d.visits.is_empty(), "{:?}", d.visits);
    assert_eq!(d.stages_done, 2, "clear --to fix");
    assert_eq!(d.park_generation, 1);
    assert_eq!(env.status_of("u1").0, Status::Running);
    assert_eq!(
        env.engine.requests().unwrap()[0].status,
        RequestStatus::Answered
    );
    // fix runs, and the new budget lets review send the unit to fix again.
    assert_eq!(launched(env.engine.start_stage("u1").unwrap()), "u1/fix#3");
    env.engine
        .outcome("u1", "fix", 0, done("fix-done"), None)
        .unwrap();
    visit(&env, "review", 0);
    assert_eq!(detail(&env, "u1").visits.get("review#0"), Some(&1));
    assert_eq!(env.status_of("u1").0, Status::Running);
}

#[test]
fn a_clear_with_no_target_resumes_the_stage_that_parked_and_resets_the_counters() {
    let env = at_review();
    visit(&env, "review", 0);
    visit(&env, "fix", 0);
    visit(&env, "review", 0);
    visit(&env, "fix", 0);
    launched(env.engine.start_stage("u1").unwrap());
    env.engine
        .outcome("u1", "review", 0, done("review.md"), None)
        .unwrap();
    let generation = env
        .engine
        .claim("op@host", &sha256_hex(b"a token"))
        .unwrap()
        .generation;
    let request = open_request(&env, "u1");
    env.engine
        .clear_unit("u1", None, generation, &request.id)
        .unwrap();
    assert!(detail(&env, "u1").visits.is_empty());
    assert_eq!(detail(&env, "u1").stages_done, 1, "review");
}

#[test]
fn a_stage_entered_by_outcome_is_skipped_in_order_and_merge_ready_counts_it_done() {
    let env = at_review();
    env.forge.script(1, PrScript::green("work/u1", "aaaaaaaa"));
    launched(env.engine.start_stage("u1").unwrap());
    // No outcome holds: review is done, and the unit goes on. fix is passed over.
    env.engine
        .signal("u1", "review", done("review.md"), None)
        .unwrap();
    let d = detail(&env, "u1");
    assert_eq!(d.stages_done, 3, "build, review and the skipped fix");
    assert!(matches!(
        env.engine.start_stage("u1").unwrap(),
        LaunchStep::NothingToLaunch
    ));
    let view = env.engine.view().unwrap();
    assert_eq!(
        next_in_order(&view, env.engine.workflow()),
        Next::Ready("u1".to_string())
    );
    match env.engine.merge_next().unwrap() {
        MergeStep::Merged { unit, .. } => assert_eq!(unit, "u1"),
        other => panic!("{other:?}"),
    }
    assert_eq!(env.status_of("u1").0, Status::Done);
}

#[test]
fn a_first_stage_that_only_an_outcome_enters_is_skipped_at_admission() {
    let mut wf = loop_workflow();
    let fix = wf.tracks[0].stages.remove(2);
    wf.tracks[0].stages.insert(0, fix);
    let env = env(wf);
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    assert_eq!(detail(&env, "u1").stages_done, 1);
    assert_eq!(
        launched(env.engine.start_stage("u1").unwrap()),
        "u1/build#1"
    );
}

#[test]
fn a_replayed_outcome_command_changes_nothing() {
    let env = at_review();
    launched(env.engine.start_stage("u1").unwrap());
    let first = env
        .engine
        .outcome("u1", "review", 0, done("review.md"), None)
        .unwrap();
    assert!(!first.replayed);
    let before = detail(&env, "u1");
    let lines = env.ledger_text().lines().count();
    let admissions = env.engine.view().unwrap().admissions.len();

    let again = env
        .engine
        .outcome("u1", "review", 0, done("review.md"), None)
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.row.command_id, first.row.command_id);
    assert_eq!(detail(&env, "u1"), before);
    assert_eq!(env.ledger_text().lines().count(), lines);
    assert_eq!(env.engine.view().unwrap().admissions.len(), admissions);
    assert_eq!(detail(&env, "u1").visits.get("review#0"), Some(&1));
}

#[test]
fn a_halted_run_takes_no_outcome_launch() {
    let env = at_review();
    launched(env.engine.start_stage("u1").unwrap());
    env.engine.halt("the operator stopped the run").unwrap();
    let launches = env.agent.launch_count();

    // The outcome is recorded: halt never stops a running session's signals.
    env.engine
        .outcome("u1", "review", 0, done("review.md"), None)
        .unwrap();
    let d = detail(&env, "u1");
    assert_eq!(d.stages_done, 2);
    assert_eq!(d.visits.get("review#0"), Some(&1));
    assert_eq!(
        env.status_of("u1"),
        (Status::Running, Some("run halted".to_string()))
    );

    // The launch that follows is refused, and no seat starts.
    match env.engine.start_stage("u1").unwrap() {
        LaunchStep::Refused { reason } => assert_eq!(reason, "run halted"),
        other => panic!("{other:?}"),
    }
    assert_eq!(env.agent.launch_count(), launches);
}

#[test]
fn an_outcome_that_the_stage_does_not_have_or_a_failed_signal_is_rejected() {
    let env = at_review();
    launched(env.engine.start_stage("u1").unwrap());

    let e = env
        .engine
        .outcome("u1", "review", 5, done("review.md"), None)
        .unwrap_err();
    assert!(
        matches!(&e, EngineError::Rejected(m) if m.contains("outcome 5") && m.contains("review")),
        "{e}"
    );
    let e = env
        .engine
        .outcome("u1", "review", 0, Signal::ExitCode { code: 3 }, None)
        .unwrap_err();
    assert!(
        matches!(&e, EngineError::Rejected(m) if m.contains("succeeded")),
        "{e}"
    );
    // Nothing changed: the stage still runs.
    assert!(env.engine.view().unwrap().running_stage_of("u1").is_some());
    assert!(detail(&env, "u1").visits.is_empty());

    // A stage with no outcomes has none to take.
    let plain = env_plain();
    let e = plain
        .engine
        .outcome("u1", "build", 0, done("b"), None)
        .unwrap_err();
    assert!(
        matches!(&e, EngineError::Rejected(m) if m.contains("outcome 0")),
        "{e}"
    );
}

fn env_plain() -> Env {
    let env = env(workflow(&["build", "check"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env
}

#[test]
fn a_workflow_with_no_outcomes_runs_as_before() {
    // The stored unit detail of such a run holds no `visits` key, so old rows read the same.
    let env = env_plain();
    let view = env.engine.view().unwrap();
    let text = &view.node("u1").unwrap().detail;
    assert!(!text.contains("visits"), "{text}");
    env.engine.signal("u1", "build", done("b"), None).unwrap();
    assert_eq!(detail(&env, "u1").stages_done, 1);
    // That stored text, with no `visits` key, is what a detail written before this item holds. It
    // still reads, and its counters are empty.
    let parsed = UnitDetail::from_text(text).unwrap();
    assert!(parsed.visits.is_empty());
}

#[test]
fn next_stage_index_passes_over_the_stages_that_only_an_outcome_enters() {
    let wf = loop_workflow();
    let track: &Track = wf.track("item").unwrap();
    assert_eq!(track.next_stage_index(0), 0);
    assert_eq!(track.next_stage_index(1), 1);
    assert_eq!(track.next_stage_index(2), 3);
    assert_eq!(track.next_stage_index(3), 3);
    assert_eq!(track.stage_index("fix"), Some(2));
    assert_eq!(track.stage_index("nope"), None);
    // A MergeDef is untouched by any of this.
    assert!(
        track
            .merge
            .as_ref()
            .is_some_and(|m: &MergeDef| !m.head_check)
    );
}
