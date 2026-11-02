//! AC-10: a seat's push reaches the ledger at the push boundary. `record_push` admits the push and
//! writes one ledger line with repo, branch, SHA and outcome. The head check compares a PR head
//! with the last recorded push.

mod common;

use common::{Env, FakeUnitSource, env, unit, workflow};
use pf_engine::Status;
use pf_engine::admission::{CommandBody, EngineError, PushOutcome};
use pf_engine::merge::{PushRecord, head_check};

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn run() -> Env {
    let e = env(workflow(&["build"], true));
    e.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1), unit("u2", "g1", 2)],
        })
        .unwrap();
    e.engine.start_stage("u1").unwrap();
    e
}

fn push_lines(e: &Env) -> Vec<String> {
    e.ledger_text()
        .lines()
        .filter(|l| l.contains(" push "))
        .map(str::to_string)
        .collect()
}

fn failed(why: &str) -> PushOutcome {
    PushOutcome::Failed(why.to_string())
}

#[test]
fn a_successful_push_writes_one_ledger_line_with_repo_branch_sha_and_outcome() {
    let e = run();
    let a = e
        .engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    assert!(!a.replayed);
    assert_eq!(a.row.kind, "push");
    assert_eq!(a.row.unit_id.as_deref(), Some("u1"));
    let lines = push_lines(&e);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert!(line.contains("repo=owner/repo"), "{line}");
    assert!(line.contains("branch=work/u1"), "{line}");
    assert!(line.contains(&format!("sha={SHA_A}")), "{line}");
    assert!(line.contains("outcome=pushed"), "{line}");
    assert!(line.contains(" u1 "), "the unit is named: {line}");
}

#[test]
fn a_failed_push_writes_one_ledger_line_with_the_reason() {
    let e = run();
    e.engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            failed("rejected: non-fast-forward"),
        )
        .unwrap();
    let lines = push_lines(&e);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains("outcome=failed: rejected: non-fast-forward"),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains("repo=owner/repo") && lines[0].contains("branch=work/u1"));
    assert!(lines[0].contains(&format!("sha={SHA_A}")));
}

#[test]
fn a_repeat_of_one_event_id_is_a_replay_and_writes_no_second_line() {
    let e = run();
    e.engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    let again = e
        .engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    assert!(again.replayed);
    assert_eq!(push_lines(&e).len(), 1);
    // The same event ID with other content is a conflict, not a second line.
    let err = e
        .engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_B,
            PushOutcome::Pushed,
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::Conflict { .. }), "{err}");
    assert_eq!(push_lines(&e).len(), 1);
}

#[test]
fn two_failed_pushes_with_different_ids_write_two_lines() {
    let e = run();
    for id in ["ev-1", "ev-2"] {
        e.engine
            .record_push(
                "u1",
                id,
                "owner/repo",
                "work/u1",
                SHA_A,
                failed("network down"),
            )
            .unwrap();
    }
    assert_eq!(push_lines(&e).len(), 2);
    // Even with the same fields, a different event ID is a different push.
    e.engine
        .record_push(
            "u1",
            "ev-3",
            "owner/repo",
            "work/u1",
            SHA_A,
            failed("network down"),
        )
        .unwrap();
    assert_eq!(push_lines(&e).len(), 3);
}

#[test]
fn last_successful_push_skips_a_later_failed_push() {
    let e = run();
    assert_eq!(e.engine.last_successful_push("u1").unwrap(), None);
    e.engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    e.engine
        .record_push(
            "u1",
            "ev-2",
            "owner/repo",
            "work/u1",
            SHA_B,
            PushOutcome::Pushed,
        )
        .unwrap();
    e.engine
        .record_push(
            "u1",
            "ev-3",
            "owner/repo",
            "work/u1",
            "cccc",
            failed("rejected"),
        )
        .unwrap();
    let last = e.engine.last_successful_push("u1").unwrap().unwrap();
    assert_eq!(last.sha, SHA_B);
    assert_eq!(last.repo, "owner/repo");
    assert_eq!(last.branch, "work/u1");
    // Another unit's pushes do not count.
    assert_eq!(e.engine.last_successful_push("u2").unwrap(), None);
    e.engine
        .record_push(
            "u2",
            "ev-4",
            "owner/repo",
            "work/u2",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    assert_eq!(
        e.engine.last_successful_push("u1").unwrap().unwrap().sha,
        SHA_B
    );
    // An unknown unit has none.
    assert_eq!(e.engine.last_successful_push("nope").unwrap(), None);
}

#[test]
fn the_head_check_passes_on_a_match_and_refuses_the_rest() {
    let push = PushRecord {
        repo: "owner/repo".into(),
        branch: "work/u1".into(),
        sha: SHA_A.into(),
    };
    assert_eq!(head_check(SHA_A, Some(&push)), Ok(()));
    assert_eq!(head_check(&SHA_A.to_uppercase(), Some(&push)), Ok(()));
    assert_eq!(
        head_check(SHA_B, Some(&push)),
        Err(format!("PR head {SHA_B} has no recorded push"))
    );
    assert_eq!(
        head_check(SHA_B, None),
        Err(format!("PR head {SHA_B} has no recorded push"))
    );
    assert!(head_check("", Some(&push)).is_err());
}

#[test]
fn the_head_check_works_on_what_the_engine_read_back() {
    let e = run();
    e.engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    let last = e.engine.last_successful_push("u1").unwrap();
    assert!(head_check(SHA_A, last.as_ref()).is_ok());
    assert!(head_check(SHA_B, last.as_ref()).is_err());
}

#[test]
fn after_halt_a_push_effect_is_still_recorded() {
    let e = run();
    e.engine.halt("operator stop").unwrap();
    let a = e
        .engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    assert!(!a.replayed);
    assert_eq!(
        a.row.outcome,
        pf_engine::port::store::AdmissionOutcome::Admitted
    );
    assert_eq!(push_lines(&e).len(), 1);
    assert_eq!(
        e.engine.last_successful_push("u1").unwrap().unwrap().sha,
        SHA_A
    );
}

#[test]
fn a_push_is_recorded_for_a_unit_that_is_no_longer_running() {
    let e = run();
    e.engine.park("u1", "needs a person").unwrap();
    e.engine
        .record_push(
            "u1",
            "ev-1",
            "owner/repo",
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    assert_eq!(
        e.status_of("u1").0,
        Status::Parked,
        "a push does not change the unit"
    );
    assert_eq!(push_lines(&e).len(), 1);
}

#[test]
fn a_push_for_an_unknown_unit_is_rejected() {
    let e = run();
    let err = e
        .engine
        .record_push(
            "ghost",
            "ev-1",
            "owner/repo",
            "b",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::Rejected(_)), "{err}");
    assert!(push_lines(&e).is_empty());
}

#[test]
fn a_token_in_repo_branch_or_failure_reason_is_absent_from_the_ledger_line() {
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    let e = run();
    e.engine
        .record_push(
            "u1",
            "ev-1",
            &format!("https://x:{token}@example.com/owner/repo"),
            &format!("work/{token}"),
            SHA_A,
            failed(&format!("remote said token {token} is bad")),
        )
        .unwrap();
    let ledger = e.ledger_text();
    assert!(!ledger.contains(token), "{ledger}");
    assert!(!ledger.contains("0123456789abcdef"), "{ledger}");
    assert_eq!(push_lines(&e).len(), 1);
    // The stored command holds none either.
    let stored = e.engine.view().unwrap().admissions;
    let push = stored.iter().find(|a| a.kind == "push").unwrap();
    assert!(!push.content.contains(token), "{}", push.content);
}

#[test]
fn every_ledger_line_passes_redact_not_only_pushes() {
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    let e = run();
    // A park reason is outside text, and it reaches the ledger.
    e.engine.park("u1", &format!("seat said {token}")).unwrap();
    let ledger = e.ledger_text();
    assert!(!ledger.contains(token), "{ledger}");
    assert!(ledger.contains("unit u1 parked"), "{ledger}");
}

#[test]
fn the_push_command_is_part_of_the_command_body() {
    let body = CommandBody::Push {
        unit_id: "u1".into(),
        event_id: "ev".into(),
        repo: "r".into(),
        branch: "b".into(),
        sha: "s".into(),
        outcome: PushOutcome::Pushed,
    };
    assert_eq!(body.kind(), "push");
    let json = serde_json::to_string(&body).unwrap();
    let back: CommandBody = serde_json::from_str(&json).unwrap();
    assert_eq!(back, body);
}
