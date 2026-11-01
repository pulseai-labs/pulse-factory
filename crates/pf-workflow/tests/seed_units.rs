//! AC-5: seed units. A seed is a unit the workflow file declares. It is admitted at the start of a
//! run, and a `repeat` re-admits it as `<id>.2`, `<id>.3` while the work it queued finishes.

#[path = "../../pf-engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;

use common::{Env, TrackedSource, env, unit};
use pf_engine::capacity::Settings;
use pf_engine::launch::LaunchStep;
use pf_engine::unit::Signal;
use pf_engine::{StageDef, Status, Track, Workflow};
use pf_workflow::file::{RepeatDef, SeedDef};
use pf_workflow::seeds::{SeedAction, SeedStep, advance_seed, decide, seed_unit_id, start_seeds};

/// Two tracks. `lane` has one stage. `item` has one stage and no merge rule, so a finished item
/// unit is `done`.
fn workflow() -> Workflow {
    let stage = |s: &str| StageDef::new(s, &format!("{s}-role"), "model-a");
    Workflow {
        name: "seeded".to_string(),
        tracks: vec![
            Track {
                name: "lane".to_string(),
                stages: vec![stage("fill")],
                merge: None,
            },
            Track {
                name: "item".to_string(),
                stages: vec![stage("build")],
                merge: None,
            },
        ],
        settings: Settings::default(),
        roots: Vec::new(),
    }
}

fn seed(max: u32) -> SeedDef {
    SeedDef {
        id: "lane".to_string(),
        group: "lane-group".to_string(),
        order: 1,
        track: "lane".to_string(),
        vars: BTreeMap::from([("mode".to_string(), "fill".to_string())]),
        repeat: Some(RepeatDef {
            when_units_done: "item".to_string(),
            until_file: "{run.dir}/all.done".to_string(),
            max,
        }),
    }
}

fn done(path: &str) -> Signal {
    Signal::DoneFile {
        path: path.to_string(),
    }
}

/// Run the unit's only stage to its end.
fn finish(e: &Env, id: &str, stage: &str) {
    assert!(
        matches!(
            e.engine.start_stage(id).unwrap(),
            LaunchStep::Launched { .. }
        ),
        "{id}"
    );
    e.engine.signal(id, stage, done("x"), None).unwrap();
}

fn add_item(e: &Env, id: &str) {
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit(id, "r1", 1)],
        })
        .unwrap();
}

fn step(e: &Env, seed: &SeedDef, until_file_exists: bool) -> SeedStep {
    decide(seed, &e.engine.view().unwrap(), until_file_exists)
}

#[test]
fn a_seed_is_admitted_once_at_the_start_and_again_is_a_no_op() {
    let e = env(workflow());
    let seeds = [seed(3)];
    let first = start_seeds(&e.engine, &seeds).unwrap();
    assert_eq!(first.len(), 1);
    assert!(!first[0].replayed);
    let view = e.engine.view().unwrap();
    let node = view.node("lane").expect("the seed unit exists");
    let detail = view.unit_detail(node).unwrap();
    assert_eq!(detail.track, "lane");
    assert_eq!(detail.record.group, "lane-group");
    assert_eq!(detail.record.order, 1);
    assert_eq!(detail.record.vars["mode"], "fill");

    let ledger_before = e.ledger_text();
    let again = start_seeds(&e.engine, &seeds).unwrap();
    assert!(again[0].replayed, "the same content is a replay");
    assert_eq!(
        e.ledger_text(),
        ledger_before,
        "a replay writes no ledger line"
    );
    assert_eq!(e.engine.view().unwrap().units().count(), 1);

    // The unit runs its own track's stage.
    assert!(matches!(
        e.engine.start_stage("lane").unwrap(),
        LaunchStep::Launched { .. }
    ));
    assert_eq!(e.agent.launches.lock().unwrap()[0].stage, "fill");
}

#[test]
fn the_unit_ids_of_a_seed_are_the_id_then_the_id_with_a_count() {
    assert_eq!(seed_unit_id("lane", 1), "lane");
    assert_eq!(seed_unit_id("lane", 2), "lane.2");
    assert_eq!(seed_unit_id("lane", 3), "lane.3");
}

#[test]
fn it_does_not_re_admit_while_the_previous_seed_runs() {
    let e = env(workflow());
    let s = seed(3);
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    assert_eq!(step(&e, &s, false), SeedStep::Wait);
    e.engine.start_stage("lane").unwrap();
    assert_eq!(step(&e, &s, false), SeedStep::Wait);
    // Even with finished items on the track, a running seed is not re-admitted.
    add_item(&e, "i0");
    finish(&e, "i0", "build");
    assert_eq!(step(&e, &s, false), SeedStep::Wait);
}

#[test]
fn a_finished_seed_that_added_no_unit_returns_a_halt_reason() {
    let e = env(workflow());
    let s = seed(3);
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    finish(&e, "lane", "fill");
    assert_eq!(e.status_of("lane").0, Status::Done);
    // An empty track.
    assert_eq!(
        step(&e, &s, false),
        SeedStep::Halt("seed lane added no units".to_string())
    );

    // With a real engine, `halt_for` succeeds on the done seed, and no launch follows.
    let action = advance_seed(&e.engine, &s, false).unwrap();
    assert_eq!(
        action,
        SeedAction::Halted("seed lane added no units".to_string())
    );
    let halted = e.engine.view().unwrap().run.unwrap().halted_reason;
    assert_eq!(halted.as_deref(), Some("seed lane added no units"));
    // The same call again is a replay, not an error.
    let again = advance_seed(&e.engine, &s, false).unwrap();
    assert_eq!(
        again,
        SeedAction::Halted("seed lane added no units".to_string())
    );
    add_item(&e, "i1");
    match e.engine.start_stage("i1").unwrap() {
        LaunchStep::Refused { reason } => assert_eq!(reason, "run halted"),
        other => panic!("no launch may follow: {other:?}"),
    }
    assert_eq!(
        e.agent.launch_count(),
        1,
        "only the seed's own launch reached the agent"
    );
}

#[test]
fn it_re_admits_as_id_dot_2_only_when_every_condition_holds() {
    let e = env(workflow());
    let s = seed(5);
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    finish(&e, "lane", "fill");

    // The seed queued two units. One is running: no re-admit.
    add_item(&e, "i1");
    add_item(&e, "i2");
    assert_eq!(step(&e, &s, false), SeedStep::Wait);
    finish(&e, "i1", "build");
    assert_eq!(step(&e, &s, false), SeedStep::Wait, "i2 is still running");

    // A parked unit blocks it too.
    e.engine.start_stage("i2").unwrap();
    e.engine.park("i2", "needs a person").unwrap();
    assert_eq!(step(&e, &s, false), SeedStep::Wait, "i2 is parked");

    // A second run, where both items finish, checks the `until_file` and the new ID.
    let e = env(workflow());
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    finish(&e, "lane", "fill");
    add_item(&e, "i1");
    add_item(&e, "i2");
    finish(&e, "i1", "build");
    finish(&e, "i2", "build");
    // The file exists: the work is finished.
    assert_eq!(step(&e, &s, true), SeedStep::Wait);
    // The file does not exist: re-admit as `lane.2`, with the seed's own fields.
    let SeedStep::Admit { record, track } = step(&e, &s, false) else {
        panic!("admit")
    };
    assert_eq!(record.id, "lane.2");
    assert_eq!(track, "lane");
    assert_eq!(record.group, "lane-group");
    assert_eq!(record.order, 1);
    assert_eq!(record.vars["mode"], "fill");

    let action = advance_seed(&e.engine, &s, false).unwrap();
    assert_eq!(action, SeedAction::Admitted("lane.2".to_string()));
    let view = e.engine.view().unwrap();
    assert!(view.node("lane.2").is_some());
    // The new seed runs: nothing more happens, and calling again does not admit `lane.3`.
    assert_eq!(
        advance_seed(&e.engine, &s, false).unwrap(),
        SeedAction::Idle
    );
    assert!(e.engine.view().unwrap().node("lane.3").is_none());
}

#[test]
fn the_second_seed_unit_needs_a_unit_that_came_in_after_it() {
    let e = env(workflow());
    let s = seed(5);
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    // The item comes in after the first seed and before it ends.
    add_item(&e, "i1");
    finish(&e, "lane", "fill");
    finish(&e, "i1", "build");
    let action = advance_seed(&e.engine, &s, false).unwrap();
    assert_eq!(action, SeedAction::Admitted("lane.2".to_string()));
    // `lane.2` is done and no unit came in after it, although i1 is done: it added no unit.
    finish(&e, "lane.2", "fill");
    assert_eq!(
        step(&e, &s, false),
        SeedStep::Halt("seed lane added no units".to_string())
    );
    // A unit that comes in after lane.2 lets the third generation come.
    add_item(&e, "i2");
    assert_eq!(step(&e, &s, false), SeedStep::Wait, "i2 is running");
    finish(&e, "i2", "build");
    assert_eq!(
        advance_seed(&e.engine, &s, false).unwrap(),
        SeedAction::Admitted("lane.3".to_string())
    );
}

#[test]
fn it_stops_at_max_and_returns_a_halt_reason_unless_the_file_exists() {
    let e = env(workflow());
    let s = seed(2);
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    finish(&e, "lane", "fill");
    add_item(&e, "i1");
    finish(&e, "i1", "build");
    assert_eq!(
        advance_seed(&e.engine, &s, false).unwrap(),
        SeedAction::Admitted("lane.2".to_string())
    );
    finish(&e, "lane.2", "fill");
    add_item(&e, "i2");
    // i2 still runs: wait, even at max.
    assert_eq!(step(&e, &s, false), SeedStep::Wait);
    finish(&e, "i2", "build");
    // Two seeds have run. The limit is 2.
    assert_eq!(
        step(&e, &s, false),
        SeedStep::Halt("seed lane reached its limit".to_string())
    );
    // The file ends the repeat before the limit counts.
    assert_eq!(step(&e, &s, true), SeedStep::Wait);

    // With a real engine, `halt_for` succeeds on the done seed, and no launch follows.
    let action = advance_seed(&e.engine, &s, false).unwrap();
    assert_eq!(
        action,
        SeedAction::Halted("seed lane reached its limit".to_string())
    );
    add_item(&e, "i3");
    assert!(matches!(
        e.engine.start_stage("i3").unwrap(),
        LaunchStep::Refused { .. }
    ));
}

#[test]
fn a_seed_without_a_repeat_runs_once() {
    let e = env(workflow());
    let mut s = seed(2);
    s.repeat = None;
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    finish(&e, "lane", "fill");
    add_item(&e, "i1");
    finish(&e, "i1", "build");
    assert_eq!(step(&e, &s, false), SeedStep::Wait);
    assert_eq!(
        advance_seed(&e.engine, &s, false).unwrap(),
        SeedAction::Idle
    );
}

#[test]
fn a_seed_that_is_not_admitted_yet_is_admitted_by_the_decision() {
    let e = env(workflow());
    let s = seed(2);
    let SeedStep::Admit { record, track } = step(&e, &s, false) else {
        panic!("admit")
    };
    assert_eq!((record.id.as_str(), track.as_str()), ("lane", "lane"));
}

#[test]
fn a_failed_seed_unit_is_not_re_admitted() {
    let e = env(workflow());
    let s = seed(3);
    start_seeds(&e.engine, std::slice::from_ref(&s)).unwrap();
    e.engine.start_stage("lane").unwrap();
    e.engine
        .signal("lane", "fill", Signal::ExitCode { code: 3 }, None)
        .unwrap();
    assert_eq!(e.status_of("lane").0, Status::Failed);
    assert_eq!(step(&e, &s, false), SeedStep::Wait);
}
