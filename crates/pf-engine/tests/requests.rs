//! AC-2: every halt opens a request in the commit that parks the unit (ADR-0006). Each kind of
//! the closed list opens from its source in code. A request is revised, answered, withdrawn and
//! replayed under the admission rules (ADR-0009), and a claim fences the claim before it.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use pf_engine::claim::sha256_hex;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::PrRef;
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus, Store};
use pf_engine::unit::Signal;
use pf_engine::{Command, CommandBody, EndStatus, EngineError, Status};
use serde_json::{Value, json};

/// A workflow with the stages `implement` and `verify`, then a merge.
fn item_env(units: &[&str]) -> Env {
    let env = env(item_workflow(&["implement", "verify"], true));
    let records = units
        .iter()
        .enumerate()
        .map(|(i, id)| unit(id, "g1", u32::try_from(i).unwrap()))
        .collect();
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();
    env
}

fn requests(env: &Env) -> Vec<RequestRow> {
    env.engine.requests().unwrap()
}

fn body_of(r: &RequestRow) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

fn token_hash(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

/// Launch the unit's stage, park it, and end the stage's session, as the driver does.
fn park_ended(env: &Env, id: &str, kind: RequestKind) {
    env.engine.start_stage(id).unwrap();
    env.engine.park_as(id, kind, "stalled").unwrap();
    env.engine
        .end(&format!("{id}/implement#1"), EndStatus::Failed, "stalled")
        .unwrap();
}

fn rejected(result: Result<pf_engine::Admission, EngineError>) -> String {
    match result {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn each_kind_opens_its_request_in_the_commit_that_parks_the_unit() {
    let kinds: Vec<RequestKind> = RequestKind::ALL
        .into_iter()
        .filter(|k| *k != RequestKind::MergeAck)
        .collect();
    let ids: Vec<String> = (0..kinds.len()).map(|i| format!("u{i}")).collect();
    let env = item_env(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    for (i, kind) in kinds.iter().enumerate() {
        let id = &ids[i];
        env.engine.start_stage(id).unwrap();
        env.engine
            .park_as(id, *kind, &format!("because of {}", kind.as_str()))
            .unwrap();

        // One commit: the unit is parked and its request is open.
        assert_eq!(
            env.status_of(id),
            (
                Status::Parked,
                Some(format!("because of {}", kind.as_str()))
            )
        );
        let r = requests(&env)
            .into_iter()
            .find(|r| r.unit_id.as_deref() == Some(id))
            .unwrap();
        assert_eq!(r.id, format!("R-{}", i + 1), "one sequence for the store");
        assert_eq!(r.revision, 1);
        assert_eq!(r.kind, *kind);
        assert_eq!(r.status, RequestStatus::Open);
        assert_eq!(r.run_id, "run-1");
        assert_eq!(r.stage, "implement");
        assert_eq!(r.reason, format!("because of {}", kind.as_str()));
        assert!(r.answered_at.is_none() && r.answer.is_none() && r.answer_generation.is_none());
        let body = body_of(&r);
        assert_eq!(body["reason"], r.reason);
        match kind {
            // Their structure comes with the answers that use it: the reason only.
            RequestKind::Gaps | RequestKind::Stop => {
                assert_eq!(body.as_object().unwrap().len(), 1, "{body}");
            }
            _ => {
                assert_eq!(body["stage"], "implement");
                assert_eq!(body["park_generation"], 0);
            }
        }
    }
    assert_eq!(requests(&env).len(), kinds.len());
}

#[test]
fn a_generic_park_opens_a_park_request() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stop file said wait").unwrap();
    let r = &requests(&env)[0];
    assert_eq!(r.kind, RequestKind::Park);
    assert_eq!(r.unit_id.as_deref(), Some("u1"));
}

#[test]
fn a_unit_parked_between_stages_names_the_next_stage() {
    let env = item_env(&["u1"]);
    // Nothing launched: the unit waits for its first launch.
    env.engine.park("u1", "a hook failed").unwrap();
    assert_eq!(requests(&env)[0].stage, "implement");
}

#[test]
fn a_merge_that_did_not_land_opens_a_merge_failed_request() {
    let env = env(workflow(&["build"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    let mut script = PrScript::green("work/u1", "sha-u1");
    script.landed = false;
    env.forge.script(1, script);
    env.engine.start_stage("u1").unwrap();
    env.engine
        .signal(
            "u1",
            "build",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number: 1,
            }),
        )
        .unwrap();

    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    let r = &requests(&env)[0];
    assert_eq!(r.kind, RequestKind::MergeFailed);
    assert_eq!(r.stage, "merge");
    assert_eq!(r.reason, "merge did not land");
    assert_eq!(r.status, RequestStatus::Open);
    assert_eq!(
        env.status_of("u1"),
        (Status::Parked, Some("merge did not land".to_string()))
    );
    // The merge result is still on the ledger with repo, branch, SHA and outcome.
    assert!(env.ledger_text().contains(
        "merge of unit u1: repo=synthetic/repo branch=work/u1 sha=sha-u1 outcome=merge did not land"
    ));
}

#[test]
fn reconcile_opens_a_seat_lost_request() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.agent.kill("proc-1");
    let report = env.engine.reconcile().unwrap();
    assert_eq!(report.parked, vec!["u1"]);
    let r = &requests(&env)[0];
    assert_eq!(r.kind, RequestKind::SeatLost);
    assert_eq!(r.reason, "seat lost across a controller restart");
    assert_eq!(r.stage, "implement");
}

#[test]
fn a_merge_ack_request_is_not_a_unit_park() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    let why = rejected(env.engine.park_as("u1", RequestKind::MergeAck, "ack"));
    assert!(why.contains("merge_ack"), "{why}");
    assert_eq!(env.status_of("u1").0, Status::Running);
    assert!(requests(&env).is_empty());
}

#[test]
fn a_store_failure_between_the_park_and_its_request_leaves_neither() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.store.fail_on_open_request.store(true, Ordering::SeqCst);
    let err = env.engine.park("u1", "stalled").unwrap_err();
    assert!(matches!(err, EngineError::Degraded(_)), "{err:?}");
    assert_eq!(env.status_of("u1").0, Status::Running, "no parked unit");
    assert!(env.store.all_requests().is_empty(), "no request");
    assert!(
        env.store.admission("run-1", "park:u1:0").unwrap().is_none(),
        "no receipt"
    );
}

#[test]
fn a_unit_has_at_most_one_open_request() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stalled").unwrap();
    let why = rejected(env.engine.open_request(
        Some("u1"),
        RequestKind::Stop,
        "implement",
        "another halt",
        &json!({}),
    ));
    assert!(why.contains("R-1") && why.contains("open already"), "{why}");
    assert_eq!(requests(&env).len(), 1);
}

#[test]
fn revise_raises_the_revision_and_an_answer_at_an_old_revision_is_refused() {
    let env = item_env(&["u1"]);
    park_ended(&env, "u1", RequestKind::Gaps);
    let hash = token_hash("t1");
    let generation = env.engine.claim("op@host", &hash).unwrap().generation;

    env.engine
        .revise_request("R-1", 1, &json!({ "reason": "three gaps", "gaps": 3 }))
        .unwrap();
    let r = &requests(&env)[0];
    assert_eq!(r.revision, 2);
    assert_eq!(body_of(r)["gaps"], 3);
    assert_eq!(r.status, RequestStatus::Open);

    // The answer read revision 1. It is refused and names the current revision.
    let why = rejected(
        env.engine
            .answer_request("R-1", 1, generation, "the ruling"),
    );
    assert!(why.contains("revision 2") && why.contains("not 1"), "{why}");
    assert_eq!(env.status_of("u1").0, Status::Parked);
    assert_eq!(requests(&env)[0].status, RequestStatus::Open);
    // A revision from the past cannot be revised again either: its command ID is used.
    let err = env
        .engine
        .revise_request("R-1", 1, &json!({ "reason": "late" }))
        .unwrap_err();
    assert!(matches!(err, EngineError::Conflict { .. }), "{err:?}");
    assert_eq!(requests(&env)[0].revision, 2);

    // The answer at revision 2 is admitted.
    env.engine
        .answer_request("R-1", 2, generation, "the ruling")
        .unwrap();
    let r = &requests(&env)[0];
    assert_eq!(r.status, RequestStatus::Answered);
    assert_eq!(r.revision, 2);
    assert_eq!(r.answer_generation, Some(generation));
    assert_eq!(
        serde_json::from_str::<Value>(r.answer.as_deref().unwrap()).unwrap(),
        json!({ "verb": "answer", "text": "the ruling" })
    );
    // An answered request is closed to a revision.
    let why = rejected(
        env.engine
            .revise_request("R-1", 2, &json!({ "reason": "again" })),
    );
    assert!(why.contains("answered"), "{why}");
}

#[test]
fn the_answer_line_names_the_generation_the_request_and_its_revision() {
    let env = item_env(&["u1"]);
    park_ended(&env, "u1", RequestKind::Park);
    let generation = env
        .engine
        .claim("op@host", &token_hash("t1"))
        .unwrap()
        .generation;
    let long = format!("line one\nline two {}", "x".repeat(400));
    env.engine
        .answer_request("R-1", 1, generation, &long)
        .unwrap();

    let ledger = env.ledger_text();
    let line = ledger
        .lines()
        .find(|l| l.contains("answer R-1@1 by claim 1:"))
        .unwrap_or_else(|| panic!("no answer line in\n{ledger}"));
    let brief = line.split("answer R-1@1 by claim 1: ").nth(1).unwrap();
    assert_eq!(brief.chars().count(), 200, "{brief}");
    assert!(brief.starts_with("line one / line two xxx"), "{brief}");
    assert!(line.contains("claim 1 by op@host") || ledger.contains("claim 1 by op@host"));
    // The command is in the admissions table with its ID.
    assert!(
        env.store
            .admission("run-1", "answer:R-1:1")
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_secret_in_an_answer_text_stays_out_of_the_ledger() {
    let env = item_env(&["u1"]);
    park_ended(&env, "u1", RequestKind::Park);
    let generation = env
        .engine
        .claim("op@host", &token_hash("t1"))
        .unwrap()
        .generation;
    env.engine
        .answer_request(
            "R-1",
            1,
            generation,
            "use GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789 to push",
        )
        .unwrap();
    assert!(
        !env.ledger_text().contains("ghp_abcdef"),
        "{}",
        env.ledger_text()
    );
    // The text itself is stored as the operator gave it.
    assert!(
        requests(&env)[0]
            .answer
            .as_deref()
            .unwrap()
            .contains("ghp_abcdef")
    );
}

#[test]
fn a_replayed_command_returns_its_receipt_and_the_same_id_with_other_content_conflicts() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    let first = env.engine.park("u1", "stalled").unwrap();
    assert!(!first.replayed);
    let again = env.engine.park("u1", "stalled").unwrap();
    assert!(again.replayed);
    assert_eq!(again.row, first.row);
    assert_eq!(requests(&env).len(), 1, "a replay opens no second request");

    let err = env.engine.park("u1", "another reason").unwrap_err();
    assert_eq!(
        err,
        EngineError::Conflict {
            command_id: "park:u1:0".to_string()
        }
    );

    // The same holds for a request command.
    let body = json!({ "reason": "ack" });
    let a = env
        .engine
        .open_request(None, RequestKind::MergeAck, "", "spine PR waits", &body)
        .unwrap();
    let b = env
        .engine
        .open_request(None, RequestKind::MergeAck, "", "spine PR waits", &body)
        .unwrap();
    assert!(b.replayed);
    assert_eq!(a.row, b.row);
    let err = env
        .engine
        .open_request(None, RequestKind::MergeAck, "", "other text", &body)
        .unwrap_err();
    assert!(matches!(err, EngineError::Conflict { .. }), "{err:?}");
    assert_eq!(requests(&env).len(), 2);
}

#[test]
fn a_run_level_request_has_no_unit_and_ends_with_the_run() {
    let env = item_env(&["u1"]);
    env.engine
        .open_request(
            None,
            RequestKind::MergeAck,
            "",
            "the spine PR waits for your ack",
            &json!({ "pr": "acme/widgets#7" }),
        )
        .unwrap();
    let r = &requests(&env)[0];
    assert_eq!(r.unit_id, None);
    assert_eq!(r.kind, RequestKind::MergeAck);

    env.engine.end_run("replaced by run-2").unwrap();
    let r = &requests(&env)[0];
    assert_eq!(r.status, RequestStatus::Withdrawn);
    let answer: Value = serde_json::from_str(r.answer.as_deref().unwrap()).unwrap();
    assert_eq!(answer["verb"], "withdraw");
    assert_eq!(answer["reason"], "run ended: replaced by run-2");
}

#[test]
fn a_request_can_be_withdrawn_once() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stalled").unwrap();
    env.engine
        .withdraw_request("R-1", "the unit moved on")
        .unwrap();
    assert_eq!(requests(&env)[0].status, RequestStatus::Withdrawn);
    // The command ID is `withdraw:<id>`, so a repeat is a replay.
    assert!(
        env.engine
            .withdraw_request("R-1", "the unit moved on")
            .unwrap()
            .replayed
    );
    let why = rejected(env.engine.withdraw_request("R-9", "x"));
    assert!(why.contains("R-9") && why.contains("unknown"), "{why}");
}

#[test]
fn a_new_claim_fences_the_old_one_with_the_next_generation() {
    let env = item_env(&["u1", "u2"]);
    for id in ["u1", "u2"] {
        park_ended(&env, id, RequestKind::Park);
    }
    let h1 = token_hash("token one");
    let h2 = token_hash("token two");
    let g1 = env.engine.claim("op@desk:one", &h1).unwrap();
    assert_eq!(g1.generation, 1);
    let g2 = env.engine.claim("op@desk:two", &h2).unwrap();
    assert_eq!(g2.generation, 2);
    assert!(!g2.admission.replayed);

    let claims = env.store.all_claims();
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0].token_hash, h1);
    assert!(claims[0].fenced_at.is_some(), "claim 1 is fenced");
    assert_eq!(claims[1].holder, "op@desk:two");
    assert!(claims[1].fenced_at.is_none());
    // The claim line names the generation and the holder, and never a token.
    let ledger = env.ledger_text();
    assert!(ledger.contains("claim 1 by op@desk:one"), "{ledger}");
    assert!(ledger.contains("claim 2 by op@desk:two"), "{ledger}");
    assert!(env.store.admission("run-1", "claim:2").unwrap().is_some());

    // The old generation cannot answer. The refusal names both generations.
    let why = rejected(env.engine.answer_request("R-1", 1, 1, "late"));
    assert_eq!(why, "claim 1 was fenced by claim 2");
    assert_eq!(requests(&env)[0].status, RequestStatus::Open);
    // A generation that was never granted is refused too.
    let why = rejected(env.engine.answer_request("R-1", 1, 7, "forged"));
    assert!(why.contains("claim 7 is not the current claim"), "{why}");
    // The current one answers.
    env.engine.answer_request("R-1", 1, 2, "ruling").unwrap();
    assert_eq!(requests(&env)[0].answer_generation, Some(2));
}

#[test]
fn nobody_answers_before_the_first_claim() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stalled").unwrap();
    let why = rejected(env.engine.answer_request("R-1", 1, 1, "ruling"));
    assert!(why.contains("no claim has been made"), "{why}");
    assert_eq!(env.status_of("u1").0, Status::Parked);
}

#[test]
fn a_claim_needs_a_real_hash_and_the_next_generation() {
    let env = item_env(&["u1"]);
    let why = rejected(env.engine.claim("op", "not-a-hash").map(|g| g.admission));
    assert!(why.contains("64 hex digits"), "{why}");
    // A generation that skips ahead is stale or forged.
    let err = env
        .engine
        .submit(Command {
            id: "claim:5".to_string(),
            body: CommandBody::Claim {
                generation: 5,
                holder: "op".to_string(),
                token_hash: token_hash("x"),
            },
        })
        .unwrap_err();
    assert!(
        matches!(&err, EngineError::Rejected(why) if why.contains("next is 1")),
        "{err:?}"
    );
    assert!(env.store.all_claims().is_empty());
}

#[test]
fn an_answer_needs_text() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stalled").unwrap();
    let generation = env.engine.claim("op", &token_hash("t")).unwrap().generation;
    let why = rejected(env.engine.answer_request("R-1", 1, generation, "   "));
    assert!(why.contains("needs text"), "{why}");
    assert_eq!(env.status_of("u1").0, Status::Parked);
}
