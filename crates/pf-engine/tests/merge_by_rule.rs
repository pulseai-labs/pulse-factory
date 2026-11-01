//! AC-8: with a fake forge, unit PRs merge in the declared order on green checks, and each merge
//! writes a ledger line with repo, branch, SHA and outcome. A red check, a conflict and a merge
//! that did not land each park the unit with its reason, and later units wait.

mod common;

use common::*;
use pf_engine::Status;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::{Checks, ForgeOutcome, Mergeable, PrRef};
use pf_engine::unit::Signal;

/// Admit the units, give each a PR number from its place in the list, and script a green PR.
fn run_with(units: &[(&str, &str, u32)]) -> Env {
    let env = env(workflow(&["build"], true));
    let records = units.iter().map(|(id, g, o)| unit(id, g, *o)).collect();
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();
    for (i, (id, _, _)) in units.iter().enumerate() {
        env.forge.script(
            i as u64 + 1,
            PrScript::green(&format!("work/{id}"), &format!("sha-{id}")),
        );
    }
    env
}

/// Run the unit's stage to its end, and name its PR.
fn finish_stage(env: &Env, id: &str, pr_number: u64) {
    env.engine.start_stage(id).unwrap();
    env.engine
        .signal(
            id,
            "build",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number: pr_number,
            }),
        )
        .unwrap();
}

#[test]
fn unit_prs_merge_in_the_declared_order_and_each_merge_writes_a_ledger_line() {
    // u3 is in the group that came first. u1 and u2 follow in their own order.
    let env = run_with(&[("u3", "g2", 1), ("u1", "g1", 1), ("u2", "g1", 2)]);
    // Finish in the wrong order: u2 and u1 first, u3 last.
    finish_stage(&env, "u2", 3);
    finish_stage(&env, "u1", 2);
    let step = env.engine.merge_next().unwrap();
    assert!(
        matches!(&step, MergeStep::Waiting { unit, .. } if unit == "u3"),
        "u3 holds the order: {step:?}"
    );
    assert!(env.forge.merged().is_empty());

    finish_stage(&env, "u3", 1);
    let mut merged = Vec::new();
    loop {
        match env.engine.merge_next().unwrap() {
            MergeStep::Merged { unit, effect } => {
                assert_eq!(effect.repo, "synthetic/repo");
                assert_eq!(effect.branch, format!("work/{unit}"));
                assert_eq!(effect.sha, format!("sha-{unit}"));
                assert_eq!(effect.outcome, ForgeOutcome::Landed);
                merged.push(unit);
            }
            MergeStep::Idle => break,
            other => panic!("unexpected step {other:?}"),
        }
    }
    assert_eq!(merged, vec!["u3", "u1", "u2"]);
    // The forge saw the PRs in the same order: u3 has PR 1, u1 has PR 2, u2 has PR 3.
    assert_eq!(env.forge.merged(), vec![1, 2, 3]);

    let ledger = env.ledger_text();
    for id in ["u3", "u1", "u2"] {
        let wanted = format!(
            "merge of unit {id}: repo=synthetic/repo branch=work/{id} sha=sha-{id} outcome=landed"
        );
        assert!(ledger.contains(&wanted), "missing {wanted:?} in\n{ledger}");
        assert_eq!(
            env.status_of(id),
            (Status::Done, Some(format!("merged at sha-{id}")))
        );
    }
    // The ledger lines are in the merge order.
    let at = |id: &str| ledger.find(&format!("merge of unit {id}:")).unwrap();
    assert!(at("u3") < at("u1") && at("u1") < at("u2"));
}

#[test]
fn a_red_check_parks_the_unit_and_later_units_wait() {
    let env = run_with(&[("u1", "g1", 1), ("u2", "g1", 2)]);
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&1)
        .unwrap()
        .checks = Checks::Red;
    finish_stage(&env, "u1", 1);
    finish_stage(&env, "u2", 2);

    let step = env.engine.merge_next().unwrap();
    assert_eq!(
        step,
        MergeStep::Parked {
            unit: "u1".to_string(),
            reason: "checks red".to_string()
        }
    );
    assert_eq!(
        env.status_of("u1"),
        (Status::Parked, Some("checks red".to_string()))
    );
    assert!(env.forge.merged().is_empty(), "a red PR must not be merged");
    assert!(env.ledger_text().contains(
        "merge of unit u1: repo=synthetic/repo branch=work/u1 sha=sha-u1 outcome=checks red"
    ));

    let later = env.engine.merge_next().unwrap();
    assert!(
        matches!(&later, MergeStep::Waiting { unit, .. } if unit == "u1"),
        "{later:?}"
    );
    assert!(env.forge.merged().is_empty(), "u2 must wait behind u1");
    assert_eq!(env.status_of("u2"), (Status::Running, None));
}

#[test]
fn a_conflict_parks_the_unit_and_later_units_wait() {
    // A conflict that the forge shows before the merge, and one it reports on the merge call.
    for on_merge_call in [false, true] {
        let env = run_with(&[("u1", "g1", 1), ("u2", "g1", 2)]);
        {
            let mut scripts = env.forge.scripts.lock().unwrap();
            let s = scripts.get_mut(&1).unwrap();
            if on_merge_call {
                s.merge = ForgeOutcome::Conflict;
            } else {
                s.mergeable = Mergeable::Conflict;
            }
        }
        finish_stage(&env, "u1", 1);
        finish_stage(&env, "u2", 2);
        let step = env.engine.merge_next().unwrap();
        assert_eq!(
            step,
            MergeStep::Parked {
                unit: "u1".to_string(),
                reason: "merge conflict".to_string()
            }
        );
        assert_eq!(
            env.status_of("u1"),
            (Status::Parked, Some("merge conflict".to_string()))
        );
        assert_eq!(
            env.forge.merged(),
            if on_merge_call { vec![1] } else { vec![] }
        );
        assert!(matches!(
            env.engine.merge_next().unwrap(),
            MergeStep::Waiting { .. }
        ));
        assert_eq!(env.forge.merged().len(), usize::from(on_merge_call));
        assert!(env.ledger_text().contains("outcome=merge conflict"));
    }
}

#[test]
fn a_merge_that_did_not_land_parks_the_unit_and_later_units_wait() {
    let env = run_with(&[("u1", "g1", 1), ("u2", "g1", 2)]);
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&1)
        .unwrap()
        .landed = false;
    finish_stage(&env, "u1", 1);
    finish_stage(&env, "u2", 2);
    let step = env.engine.merge_next().unwrap();
    assert_eq!(
        step,
        MergeStep::Parked {
            unit: "u1".to_string(),
            reason: "merge did not land".to_string()
        }
    );
    assert_eq!(
        env.status_of("u1"),
        (Status::Parked, Some("merge did not land".to_string()))
    );
    assert_eq!(env.forge.merged(), vec![1]);
    assert!(env.ledger_text().contains(
        "merge of unit u1: repo=synthetic/repo branch=work/u1 sha=sha-u1 outcome=merge did not land"
    ));
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Waiting { unit, .. } if unit == "u1"
    ));
    assert_eq!(env.forge.merged(), vec![1], "u2 must wait behind u1");
}

#[test]
fn a_refused_merge_and_a_forge_error_park_the_unit() {
    let env = run_with(&[("u1", "g1", 1)]);
    env.forge.scripts.lock().unwrap().get_mut(&1).unwrap().merge =
        ForgeOutcome::Refused("branch is protected".to_string());
    finish_stage(&env, "u1", 1);
    env.engine.merge_next().unwrap();
    assert_eq!(
        env.status_of("u1"),
        (
            Status::Parked,
            Some("merge refused: branch is protected".to_string())
        )
    );

    // A PR the forge does not know is a forge error, not a silent skip.
    let env = run_with(&[("u1", "g1", 1)]);
    finish_stage(&env, "u1", 99);
    env.engine.merge_next().unwrap();
    let (status, reason) = env.status_of("u1");
    assert_eq!(status, Status::Parked);
    assert!(reason.unwrap().starts_with("forge error:"));
}

#[test]
fn pending_checks_wait_and_a_unit_without_a_pr_is_parked() {
    let env = run_with(&[("u1", "g1", 1)]);
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&1)
        .unwrap()
        .checks = Checks::Pending;
    finish_stage(&env, "u1", 1);
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Waiting { .. }), "{step:?}");
    assert_eq!(env.status_of("u1"), (Status::Running, None));
    // The checks turn green, and the next call merges.
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&1)
        .unwrap()
        .checks = Checks::Green;
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Merged { .. }
    ));

    let env = run_with(&[("u1", "g1", 1)]);
    env.engine.start_stage("u1").unwrap();
    env.engine
        .signal("u1", "build", Signal::TurnEnd, None)
        .unwrap();
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    assert_eq!(
        env.status_of("u1"),
        (
            Status::Parked,
            Some("no PR is recorded for the unit".to_string())
        )
    );
}
