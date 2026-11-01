//! AC-5: after `halt`, a launch command is refused with a recorded reason. After `halt`, a
//! running unit's done signal, its end status, its usage and cost and its ledger line are all
//! still recorded. The running session receives no interrupt. A green PR waiting to merge is
//! not merged, and its unit keeps its status with the reason "run halted"; a unit due a launch
//! is the same. Neither is parked.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use pf_engine::launch::LaunchStep;
use pf_engine::merge::MergeStep;
use pf_engine::port::agent::Usage;
use pf_engine::port::forge::PrRef;
use pf_engine::port::store::{AdmissionOutcome, Store};
use pf_engine::unit::Signal;
use pf_engine::{Command, CommandBody, EndStatus, EngineError, Status};

fn pr(number: u64) -> PrRef {
    PrRef {
        repo: "synthetic/repo".to_string(),
        number,
    }
}

/// u1: its stage is done and its PR is green, so it waits to merge.
/// u2: its stage runs in a live session.
/// u3: due its first launch.
fn halted_run() -> Env {
    let env = env(workflow(&["build"], true));
    env.forge.script(1, PrScript::green("work/u1", "sha-u1"));
    let records = vec![
        unit("u1", "g1", 1),
        unit("u2", "g1", 2),
        unit("u3", "g1", 3),
    ];
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();
    assert!(matches!(
        env.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    env.engine
        .signal(
            "u1",
            "build",
            Signal::DoneFile {
                path: "u1.done".to_string(),
            },
            Some(pr(1)),
        )
        .unwrap();
    assert!(matches!(
        env.engine.start_stage("u2").unwrap(),
        LaunchStep::Launched { .. }
    ));
    env.engine.halt("operator pressed the kill switch").unwrap();
    env
}

#[test]
fn after_halt_a_launch_command_is_refused_with_a_recorded_reason() {
    let env = halted_run();
    let launches_before = env.agent.launch_count();

    let refused = env
        .engine
        .submit(Command {
            id: "launch-u3".to_string(),
            body: CommandBody::Launch {
                unit_id: "u3".to_string(),
                stage: "build".to_string(),
                role: "build-role".to_string(),
                model: "model-a".to_string(),
                attempt: 1,
            },
        })
        .unwrap();
    assert_eq!(refused.row.outcome, AdmissionOutcome::Refused);
    assert_eq!(refused.row.reason.as_deref(), Some("run halted"));
    // The refusal is stored, so a repeat returns it.
    let again = env.store.admission("run-1", "launch-u3").unwrap().unwrap();
    assert_eq!(again, refused.row);

    // The engine's own launch step is refused too, and starts no process.
    let step = env.engine.start_stage("u3").unwrap();
    assert_eq!(
        step,
        LaunchStep::Refused {
            reason: "run halted".to_string()
        }
    );
    assert_eq!(env.agent.launch_count(), launches_before);
    assert!(env.store.node("run-1", "u3/build#1").is_none());
}

#[test]
fn after_halt_a_running_units_signal_end_status_usage_and_ledger_line_are_recorded() {
    let env = halted_run();
    let ledger_before = env.ledger_text().lines().count();

    let signal = env
        .engine
        .signal(
            "u2",
            "build",
            Signal::DoneFile {
                path: "u2.done".to_string(),
            },
            None,
        )
        .unwrap();
    assert_eq!(signal.row.outcome, AdmissionOutcome::Admitted);
    assert_eq!(env.status_of("u2/build#1").0, Status::Done);

    let usage = Usage {
        input_tokens: 1200,
        output_tokens: 300,
        cache_read_tokens: 40,
        cache_write_tokens: 5,
        cost_micros: 25_000,
    };
    let recorded = env
        .engine
        .record_usage("u2/build#1", usage, "final")
        .unwrap();
    assert_eq!(recorded.row.outcome, AdmissionOutcome::Admitted);
    let session = env
        .store
        .sessions("run-1")
        .unwrap()
        .into_iter()
        .find(|s| s.id == "u2/build#1")
        .unwrap();
    assert_eq!(session.input_tokens, 1200);
    assert_eq!(session.output_tokens, 300);
    assert_eq!(session.cache_read_tokens, 40);
    assert_eq!(session.cache_write_tokens, 5);
    assert_eq!(session.cost_micros, 25_000);

    let end = env
        .engine
        .end("u2", EndStatus::Done, "work finished")
        .unwrap();
    assert_eq!(end.row.outcome, AdmissionOutcome::Admitted);
    assert_eq!(
        env.status_of("u2"),
        (Status::Done, Some("work finished".to_string()))
    );

    // The signal and the end status each wrote a ledger line, whole and in the file.
    let ledger = env.ledger_text();
    assert!(ledger.lines().count() >= ledger_before + 2, "{ledger}");
    assert!(ledger.contains("stage build done"), "{ledger}");
    assert!(ledger.contains("node u2 done: work finished"), "{ledger}");
    // The session got no interrupt.
    assert_eq!(env.agent.interrupts.load(Ordering::SeqCst), 0);
}

#[test]
fn after_halt_a_green_pr_is_not_merged_and_a_unit_due_a_launch_is_not_launched() {
    let env = halted_run();

    // Neither is parked: both keep `running` and gain the reason "run halted".
    for id in ["u1", "u3"] {
        assert_eq!(
            env.status_of(id),
            (Status::Running, Some("run halted".to_string())),
            "unit {id}"
        );
    }
    // The unit with a live session is untouched.
    assert_eq!(env.status_of("u2"), (Status::Running, None));

    let step = env.engine.merge_next().unwrap();
    assert!(
        matches!(&step, MergeStep::Held { unit, reason } if unit == "u1" && reason == "run halted"),
        "{step:?}"
    );
    assert!(
        env.forge.merged().is_empty(),
        "the forge was asked to merge"
    );
    assert_eq!(
        env.status_of("u1"),
        (Status::Running, Some("run halted".to_string()))
    );

    let merge_records: Vec<_> = env
        .store
        .admissions("run-1")
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "merge")
        .collect();
    assert_eq!(merge_records.len(), 1);
    assert_eq!(merge_records[0].outcome, AdmissionOutcome::Refused);
    assert_eq!(merge_records[0].reason.as_deref(), Some("run halted"));
    assert_eq!(env.agent.interrupts.load(Ordering::SeqCst), 0);
}

#[test]
fn halt_is_idempotent_and_a_changed_reason_is_a_conflict() {
    let env = halted_run();
    let again = env.engine.halt("operator pressed the kill switch").unwrap();
    assert!(again.replayed);
    let err = env.engine.halt("another reason").unwrap_err();
    assert!(matches!(err, EngineError::Conflict { .. }));
    assert!(
        env.ledger_text()
            .contains("run halted: operator pressed the kill switch")
    );
}
