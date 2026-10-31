//! AC-6: with a fake unit source (two unit records), a fake agent and a fake store, the engine
//! turns the records into units, starts a stage, takes a done signal and records `done`. A
//! launch the fake agent refuses parks that unit through `park` with its reason, and the other
//! unit goes on. Every admitted transition has its command ID in the record
//! (`run-state-admission` audit trail).

mod common;

use common::*;
use pf_engine::Status;
use pf_engine::launch::LaunchStep;
use pf_engine::port::store::{AdmissionOutcome, Store};
use pf_engine::unit::Signal;

#[test]
fn the_engine_runs_two_units_on_fake_ports() {
    let env = env(workflow(&["build"], false));
    let mut source = FakeUnitSource {
        records: vec![unit("u1", "g1", 1), unit("u2", "g1", 2)],
    };

    // The records become units.
    let report = env.engine.ingest(&mut source).unwrap();
    assert_eq!(report.admitted, vec!["u1", "u2"]);
    assert!(report.replayed.is_empty());
    assert_eq!(env.status_of("u1").0, Status::Running);
    assert_eq!(env.status_of("u2").0, Status::Running);
    // Reading the same records again admits nothing new.
    let again = env.engine.ingest(&mut source).unwrap();
    assert!(again.admitted.is_empty());
    assert_eq!(again.replayed, vec!["u1", "u2"]);

    // The agent refuses the launch for u2 only.
    env.agent.refuse("u2");
    let first = env.engine.start_stage("u1").unwrap();
    assert_eq!(
        first,
        LaunchStep::Launched {
            session_id: "u1/build#1".to_string(),
            process_ref: "proc-1".to_string()
        }
    );
    let second = env.engine.start_stage("u2").unwrap();
    let LaunchStep::Parked { reason } = second else {
        panic!("expected the refused launch to park u2, got {second:?}");
    };
    assert_eq!(reason, "launch refused: agent version is not qualified");

    // u2 is parked with its reason, through `park`. Its stage node ended `failed`.
    assert_eq!(env.status_of("u2"), (Status::Parked, Some(reason.clone())));
    assert_eq!(env.status_of("u2/build#1").0, Status::Failed);
    // u1 goes on: the plan the agent received carries its role, model and the adapter edits.
    let plans = env.agent.launches.lock().unwrap().clone();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].unit_id, "u1");
    assert_eq!(plans[0].role, "build-role");
    assert_eq!(plans[0].model, "model-a");
    assert_eq!(plans[0].attempt, 1);
    assert_eq!(
        plans[0].settings.get("ask").map(String::as_str),
        Some("off")
    );
    assert_eq!(env.status_of("u1"), (Status::Running, None));

    // A done signal ends the stage, and the unit is `done`.
    env.engine
        .signal(
            "u1",
            "build",
            Signal::DoneFile {
                path: "u1.done".to_string(),
            },
            None,
        )
        .unwrap();
    assert_eq!(env.status_of("u1/build#1").0, Status::Done);
    assert_eq!(
        env.status_of("u1"),
        (Status::Done, Some("all stages done".to_string()))
    );

    // Audit trail: every admitted transition is a command with its ID in the record.
    let admissions = env.store.admissions("run-1").unwrap();
    let ids: Vec<&str> = admissions.iter().map(|a| a.command_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "open-run:run-1",
            "unit:u1",
            "unit:u2",
            "launch:u1:build:1",
            "attach:u1/build#1",
            "launch:u2:build:1",
            "park:u2",
            "end:u2/build#1",
            "signal:u1/build#1",
        ]
    );
    assert!(
        admissions
            .iter()
            .all(|a| a.outcome == AdmissionOutcome::Admitted && !a.content.is_empty())
    );
    let seqs: Vec<i64> = admissions.iter().map(|a| a.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    // Every stored command carries its own ID in its content.
    for a in &admissions {
        assert!(a.content.contains(&format!("\"id\":\"{}\"", a.command_id)));
    }
}

#[test]
fn a_nonzero_exit_code_ends_the_stage_and_the_unit_failed() {
    let env = env(workflow(&["build"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine
        .signal("u1", "build", Signal::ExitCode { code: 3 }, None)
        .unwrap();
    assert_eq!(env.status_of("u1/build#1").0, Status::Failed);
    let (status, reason) = env.status_of("u1");
    assert_eq!(status, Status::Failed);
    assert_eq!(reason.as_deref(), Some("stage build failed: exit code 3"));
}

#[test]
fn a_unit_moves_through_its_stages_in_order() {
    let env = env(workflow(&["build", "review"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    // The stage is running, so a second start does nothing.
    assert_eq!(
        env.engine.start_stage("u1").unwrap(),
        LaunchStep::AlreadyRunning
    );
    env.engine
        .signal("u1", "build", Signal::TurnEnd, None)
        .unwrap();
    assert_eq!(env.status_of("u1"), (Status::Running, None));
    let step = env.engine.start_stage("u1").unwrap();
    assert!(
        matches!(&step, LaunchStep::Launched { session_id, .. } if session_id == "u1/review#1"),
        "{step:?}"
    );
    env.engine
        .signal(
            "u1",
            "review",
            Signal::PrState {
                state: "merged".into(),
            },
            None,
        )
        .unwrap();
    assert_eq!(env.status_of("u1").0, Status::Done);
    assert_eq!(
        env.engine.start_stage("u1").unwrap_err().to_string(),
        "command rejected: unit u1 is done, not running"
    );
}
