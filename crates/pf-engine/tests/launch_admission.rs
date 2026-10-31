//! AC-4: each admitted launch writes one record with run, unit, role, model and attempt; a
//! repeated launch command admits no second launch.

mod common;

use common::*;
use pf_engine::port::store::{AdmissionOutcome, AdmissionRow, Store};
use pf_engine::{Command, CommandBody, EndStatus, Status};

fn launch(id: &str, unit: &str, attempt: u32) -> Command {
    Command {
        id: id.to_string(),
        body: CommandBody::Launch {
            unit_id: unit.to_string(),
            stage: "build".to_string(),
            role: "build-role".to_string(),
            model: "model-a".to_string(),
            attempt,
        },
    }
}

fn env_with_units(ids: &[&str]) -> Env {
    let env = env(workflow(&["build"], false));
    let records = ids
        .iter()
        .enumerate()
        .map(|(i, id)| unit(id, "g1", u32::try_from(i).unwrap() + 1))
        .collect();
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();
    env
}

fn launch_records(env: &Env) -> Vec<AdmissionRow> {
    env.store
        .admissions("run-1")
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "launch" && a.outcome == AdmissionOutcome::Admitted)
        .collect()
}

#[test]
fn an_admitted_launch_writes_one_record_with_run_unit_role_model_and_attempt() {
    let env = env_with_units(&["u1", "u2"]);
    env.engine.submit(launch("l-u1", "u1", 1)).unwrap();
    env.engine.submit(launch("l-u2", "u2", 1)).unwrap();

    let records = launch_records(&env);
    assert_eq!(records.len(), 2);
    let first = &records[0];
    assert_eq!(first.run_id, "run-1");
    assert_eq!(first.command_id, "l-u1");
    assert_eq!(first.unit_id.as_deref(), Some("u1"));
    assert_eq!(first.role.as_deref(), Some("build-role"));
    assert_eq!(first.model.as_deref(), Some("model-a"));
    assert_eq!(first.attempt, Some(1));
    assert_eq!(records[1].unit_id.as_deref(), Some("u2"));

    // The launch also made its stage node and its session.
    let stage = env.store.node("run-1", "u1/build#1").unwrap();
    assert_eq!(stage.status, Status::Running);
    assert_eq!(stage.role.as_deref(), Some("build-role"));
    assert_eq!(stage.model.as_deref(), Some("model-a"));
    assert_eq!(stage.attempt, 1);
    assert_eq!(env.store.sessions("run-1").unwrap().len(), 2);
}

#[test]
fn a_repeated_launch_command_admits_no_second_launch() {
    let env = env_with_units(&["u1"]);
    let first = env.engine.submit(launch("l-u1", "u1", 1)).unwrap();
    for _ in 0..3 {
        let again = env.engine.submit(launch("l-u1", "u1", 1)).unwrap();
        assert!(again.replayed);
        assert_eq!(again.row, first.row);
    }
    assert_eq!(launch_records(&env).len(), 1);
    assert_eq!(env.store.sessions("run-1").unwrap().len(), 1);
}

#[test]
fn a_new_attempt_writes_a_new_record_with_the_next_attempt_number() {
    let env = env_with_units(&["u1"]);
    env.engine.submit(launch("l-1", "u1", 1)).unwrap();
    // The first attempt must end before the unit can launch again.
    env.engine
        .end("u1/build#1", EndStatus::Failed, "agent crashed")
        .unwrap();
    env.engine.submit(launch("l-2", "u1", 2)).unwrap();

    let records = launch_records(&env);
    let attempts: Vec<_> = records.iter().map(|r| r.attempt).collect();
    assert_eq!(attempts, vec![Some(1), Some(2)]);
    assert!(env.store.node("run-1", "u1/build#2").is_some());
}

#[test]
fn a_launch_for_the_wrong_attempt_or_stage_is_rejected_and_records_nothing() {
    let env = env_with_units(&["u1"]);
    assert!(env.engine.submit(launch("l-bad", "u1", 2)).is_err());
    let mut wrong_stage = launch("l-stage", "u1", 1);
    if let CommandBody::Launch { stage, .. } = &mut wrong_stage.body {
        *stage = "review".to_string();
    }
    assert!(env.engine.submit(wrong_stage).is_err());
    assert!(env.engine.submit(launch("l-none", "nope", 1)).is_err());
    assert!(launch_records(&env).is_empty());
}
