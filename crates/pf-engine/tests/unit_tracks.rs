//! AC-4: tracks. A workflow holds named tracks. A unit's track comes from the source it came in
//! on, and the unit runs that track's stages. The merge order counts only units on a track with
//! a merge rule.

mod common;

use common::{FakeUnitSource, PrScript, TrackedSource, env, two_track_workflow, unit, workflow};
use pf_engine::launch::LaunchStep;
use pf_engine::merge::{MergeStep, Next, next_in_order};
use pf_engine::port::forge::PrRef;
use pf_engine::unit::Signal;
use pf_engine::{EngineError, Status};

fn done(path: &str) -> Signal {
    Signal::DoneFile {
        path: path.to_string(),
    }
}

fn pr(n: u64) -> Option<PrRef> {
    Some(PrRef {
        repo: "owner/repo".to_string(),
        number: n,
    })
}

fn launched_stage(step: LaunchStep) -> String {
    match step {
        LaunchStep::Launched { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_unit_from_each_source_runs_its_own_stages() {
    let e = env(two_track_workflow());
    e.engine
        .ingest(&mut TrackedSource {
            track: "lane".into(),
            records: vec![unit("L1", "lane", 1)],
        })
        .unwrap();
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("I1", "r1", 1)],
        })
        .unwrap();

    // The engine stores each unit's track.
    let view = e.engine.view().unwrap();
    let track_of = |id: &str| view.unit_detail(view.node(id).unwrap()).unwrap().track;
    assert_eq!(track_of("L1"), "lane");
    assert_eq!(track_of("I1"), "item");

    // Each unit launches the first stage of its own track.
    assert_eq!(
        launched_stage(e.engine.start_stage("L1").unwrap()),
        "L1/fill#1"
    );
    assert_eq!(
        launched_stage(e.engine.start_stage("I1").unwrap()),
        "I1/build#1"
    );
    let plans = e.agent.launches.lock().unwrap().clone();
    assert_eq!(plans[0].stage, "fill");
    assert_eq!(plans[0].role, "fill-role");
    assert_eq!(plans[1].stage, "build");

    // The lane unit has one stage and no merge rule: it is done with its stage.
    e.engine
        .signal("L1", "fill", done("lane.done"), None)
        .unwrap();
    assert_eq!(e.status_of("L1").0, Status::Done);
    assert_eq!(
        e.engine.start_stage("L1").unwrap_err().to_string(),
        "command rejected: unit L1 is done, not running"
    );

    // The item unit has a second stage.
    e.engine
        .signal("I1", "build", done("build.done"), None)
        .unwrap();
    assert_eq!(e.status_of("I1").0, Status::Running);
    assert_eq!(
        launched_stage(e.engine.start_stage("I1").unwrap()),
        "I1/check#1"
    );
    e.engine
        .signal("I1", "check", done("check.done"), pr(1))
        .unwrap();
    // All stages are done, and the item track merges by rule: the unit waits for its merge.
    assert_eq!(e.status_of("I1").0, Status::Running);
    assert!(matches!(
        e.engine.start_stage("I1").unwrap(),
        LaunchStep::NothingToLaunch
    ));
}

#[test]
fn the_merge_order_counts_only_units_on_a_track_with_a_merge_rule() {
    let e = env(two_track_workflow());
    // The lane unit comes in first, in a group of its own, and it stays running.
    e.engine
        .ingest(&mut TrackedSource {
            track: "lane".into(),
            records: vec![unit("L1", "lane", 1)],
        })
        .unwrap();
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("I1", "r1", 1), unit("I2", "r1", 2)],
        })
        .unwrap();
    e.engine.start_stage("L1").unwrap();
    for id in ["I1", "I2"] {
        e.engine.start_stage(id).unwrap();
        e.engine.signal(id, "build", done("b"), None).unwrap();
        e.engine.start_stage(id).unwrap();
    }
    e.forge.script(1, PrScript::green("work/i1", "sha1"));
    e.forge.script(2, PrScript::green("work/i2", "sha2"));
    e.engine.signal("I1", "check", done("c"), pr(1)).unwrap();
    e.engine.signal("I2", "check", done("c"), pr(2)).unwrap();

    // The lane unit is still running, and it does not hold the order.
    assert_eq!(e.status_of("L1").0, Status::Running);
    let view = e.engine.view().unwrap();
    assert_eq!(
        next_in_order(&view, e.engine.workflow()),
        Next::Ready("I1".to_string())
    );
    assert!(matches!(
        e.engine.merge_next().unwrap(),
        MergeStep::Merged { unit, .. } if unit == "I1"
    ));
    assert!(matches!(
        e.engine.merge_next().unwrap(),
        MergeStep::Merged { unit, .. } if unit == "I2"
    ));
    assert!(matches!(e.engine.merge_next().unwrap(), MergeStep::Idle));
    assert_eq!(e.forge.merged(), vec![1, 2]);
    assert_eq!(e.status_of("I1").0, Status::Done);
    assert_eq!(e.status_of("L1").0, Status::Running);
}

#[test]
fn a_parked_lane_unit_does_not_block_a_merge_and_a_parked_item_unit_does() {
    let e = env(two_track_workflow());
    e.engine
        .ingest(&mut TrackedSource {
            track: "lane".into(),
            records: vec![unit("L1", "lane", 1)],
        })
        .unwrap();
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("I1", "r1", 1), unit("I2", "r1", 2)],
        })
        .unwrap();
    e.engine.start_stage("L1").unwrap();
    e.engine.park("L1", "lane seat stopped").unwrap();
    for id in ["I1", "I2"] {
        e.engine.start_stage(id).unwrap();
        e.engine.signal(id, "build", done("b"), None).unwrap();
        e.engine.start_stage(id).unwrap();
    }
    e.forge.script(2, PrScript::green("work/i2", "sha2"));
    e.engine.signal("I2", "check", done("c"), pr(2)).unwrap();

    // I1 is still running its second stage: it holds I2 back. L1, parked, does not matter.
    assert!(matches!(
        e.engine.merge_next().unwrap(),
        MergeStep::Waiting { unit, .. } if unit == "I1"
    ));
    // Park I1: a parked unit on the merge track blocks the order.
    e.engine.park("I1", "needs a person").unwrap();
    let view = e.engine.view().unwrap();
    assert_eq!(
        next_in_order(&view, e.engine.workflow()),
        Next::Blocked("I1".to_string())
    );
}

#[test]
fn a_workflow_with_no_merge_track_has_nothing_to_merge() {
    let mut wf = two_track_workflow();
    wf.tracks[1].merge = None;
    let e = env(wf);
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("I1", "r1", 1)],
        })
        .unwrap();
    assert!(matches!(e.engine.merge_next().unwrap(), MergeStep::Idle));
    // Without a merge rule, the last stage ends the unit.
    e.engine.start_stage("I1").unwrap();
    e.engine.signal("I1", "build", done("b"), None).unwrap();
    e.engine.start_stage("I1").unwrap();
    e.engine.signal("I1", "check", done("c"), None).unwrap();
    assert_eq!(e.status_of("I1").0, Status::Done);
}

#[test]
fn a_one_track_workflow_behaves_as_before() {
    let e = env(workflow(&["build"], true));
    // A source that names no track joins the only track.
    let report = e
        .engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1), unit("u2", "g1", 2)],
        })
        .unwrap();
    assert_eq!(report.admitted, ["u1", "u2"]);
    let view = e.engine.view().unwrap();
    let detail = view.unit_detail(view.node("u1").unwrap()).unwrap();
    assert_eq!(detail.track, "main");
    e.engine.start_stage("u1").unwrap();
    e.engine.signal("u1", "build", done("b"), pr(1)).unwrap();
    e.forge.script(1, PrScript::green("work/u1", "sha1"));
    assert!(matches!(
        e.engine.merge_next().unwrap(),
        MergeStep::Merged { .. }
    ));
    // A source that names the track works too, and a replay is a replay.
    let again = e
        .engine
        .ingest(&mut TrackedSource {
            track: "main".into(),
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    assert_eq!(again.replayed, ["u1"]);
}

#[test]
fn an_unknown_track_or_a_missing_track_is_refused() {
    let e = env(two_track_workflow());
    let err = e
        .engine
        .ingest(&mut TrackedSource {
            track: "nowhere".into(),
            records: vec![unit("x", "g", 1)],
        })
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::Rejected(m) if m.contains("nowhere")),
        "{err}"
    );
    // Two tracks and a source with no track: the engine cannot choose.
    let err = e
        .engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("x", "g", 1)],
        })
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::Rejected(m) if m.contains("names no track")),
        "{err}"
    );
    assert!(e.engine.view().unwrap().node("x").is_none());
}

#[test]
fn the_same_unit_id_on_another_track_is_a_conflict() {
    let e = env(two_track_workflow());
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("x", "g", 1)],
        })
        .unwrap();
    let err = e
        .engine
        .ingest(&mut TrackedSource {
            track: "lane".into(),
            records: vec![unit("x", "g", 1)],
        })
        .unwrap_err();
    assert!(matches!(err, EngineError::Conflict { .. }), "{err}");
}

#[test]
fn the_unit_record_has_no_track_field() {
    let json = unit("u1", "g1", 1).to_json();
    assert!(!json.contains("track"), "{json}");
    let with_track = r#"{"version":1,"id":"u","group":"g","order":1,"vars":{},"track":"item"}"#;
    assert!(pf_engine::port::unit_source::UnitRecord::from_json(with_track).is_err());
}
