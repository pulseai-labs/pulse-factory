//! AC-1 of `r2.s11.w2`: an act names an option (`docs/contracts/requests.md`, "Acting on an
//! option"). `answer`, `clear` and `ack` take an option ID in place of their own parameters, and the
//! engine reads what the option does from the stored request, never from the caller. The stored
//! answer names the option, who wrote it and whether it was the recommended one.

mod common;

use common::*;
use pf_engine::claim::{AnswerAct, sha256_hex, stored_option};
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::{Checks, Mergeable, PrRef};
use pf_engine::port::store::{Cause, RequestKind, RequestRow, RequestStatus, Store};
use pf_engine::unit::Signal;
use pf_engine::{EndStatus, EngineError, MergeRule, StageDef, Status, Workflow};
use serde_json::{Value, json};

const SHA: &str = "1111111111111111111111111111111111111111";

/// A track `item` with the stages `implement` and `verify`, then a merge.
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

fn claim(env: &Env, who: &str) -> u64 {
    env.engine
        .claim(who, &sha256_hex(who.as_bytes()))
        .unwrap()
        .generation
}

fn requests(env: &Env) -> Vec<RequestRow> {
    env.engine.requests().unwrap()
}

fn request_of(env: &Env, unit: &str) -> RequestRow {
    requests(env)
        .into_iter()
        .rev()
        .find(|r| r.unit_id.as_deref() == Some(unit))
        .unwrap_or_else(|| panic!("no request for {unit}"))
}

fn body_of(r: &RequestRow) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

fn answer_of(r: &RequestRow) -> Value {
    serde_json::from_str(r.answer.as_deref().expect("the request holds an answer")).unwrap()
}

fn rejected<T: std::fmt::Debug>(r: Result<T, EngineError>) -> String {
    match r {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("the act must be refused, got {other:?}"),
    }
}

/// Park `unit` on a stop file with two options. `s2` is the recommended one.
fn stop(env: &Env, unit: &str) -> RequestRow {
    env.engine.start_stage(unit).unwrap();
    env.engine
        .park_with(
            unit,
            RequestKind::Stop,
            Cause::StopFile,
            "the seat wrote a stop file",
            Some(&json!({
                "stop_text": "which way?",
                "stop_options": [
                    { "text": "go on", "recommended": false },
                    { "text": "stop here", "recommended": true },
                ],
            })),
        )
        .unwrap();
    end_session(env, unit);
    request_of(env, unit)
}

/// The seat of a parked unit is gone: an act that resumes the unit needs its session ended.
fn end_session(env: &Env, unit: &str) {
    env.engine
        .end(&format!("{unit}/implement#1"), EndStatus::Failed, "ended")
        .unwrap();
}

/// Park `unit` at the cap of its `implement` outcome, which sends it to `verify`.
fn cap(env: &Env, unit: &str) -> RequestRow {
    env.engine.start_stage(unit).unwrap();
    env.engine
        .park_with(
            unit,
            RequestKind::Park,
            Cause::OutcomeCap,
            "the outcome cap",
            Some(&json!({ "cap_to": "verify", "cap_max": 2 })),
        )
        .unwrap();
    end_session(env, unit);
    request_of(env, unit)
}

fn stall(env: &Env, unit: &str) -> RequestRow {
    env.engine.start_stage(unit).unwrap();
    env.engine
        .park_as(unit, RequestKind::Stall, Cause::Stalled, "stalled")
        .unwrap();
    end_session(env, unit);
    request_of(env, unit)
}

fn unit_at(env: &Env, unit: &str) -> (Status, Option<String>, u32) {
    let view = env.engine.view().unwrap();
    let node = view.node(unit).unwrap();
    let detail = view.unit_detail(node).unwrap();
    (node.status, node.reason.clone(), detail.stages_done)
}

fn ledger_lines(env: &Env, needle: &str) -> Vec<String> {
    env.ledger_text()
        .lines()
        .filter(|l| l.contains(needle))
        .map(str::to_string)
        .collect()
}

// ------------------------------------------------------------------ answer

#[test]
fn an_answer_with_a_seats_option_stores_the_option_and_its_text_and_resumes_the_unit() {
    let env = item_env(&["u1"]);
    let r = stop(&env, "u1");
    let g = claim(&env, "op");
    env.engine
        .answer_act(&r.id, r.revision, g, &AnswerAct::Option("s2"))
        .unwrap();
    let after = request_of(&env, "u1");
    assert_eq!(after.status, RequestStatus::Answered);
    assert_eq!(
        answer_of(&after),
        json!({
            "verb": "answer",
            "text": "stop here",
            "option": { "id": "s2", "by": "seat", "recommended": true },
        })
    );
    assert_eq!(unit_at(&env, "u1").0, Status::Running);

    // The other option is not the recommended one.
    let env = item_env(&["u1"]);
    let r = stop(&env, "u1");
    let g = claim(&env, "op");
    env.engine
        .answer_act(&r.id, r.revision, g, &AnswerAct::Option("s1"))
        .unwrap();
    assert_eq!(
        answer_of(&request_of(&env, "u1"))["option"],
        json!({ "id": "s1", "by": "seat", "recommended": false })
    );
}

#[test]
fn a_typed_answer_is_stored_as_it_was_and_holds_no_option_key() {
    let env = item_env(&["u1"]);
    let r = stop(&env, "u1");
    let g = claim(&env, "op");
    env.engine
        .answer_request(&r.id, r.revision, g, "ship it")
        .unwrap();
    let after = request_of(&env, "u1");
    assert_eq!(
        after.answer.as_deref().unwrap(),
        json!({ "verb": "answer", "text": "ship it" }).to_string()
    );
    let lines = ledger_lines(&env, "answer R-1@1");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with(&format!("answer R-1@1 by claim {g}: ship it")),
        "{}",
        lines[0]
    );
}

#[test]
fn an_answer_with_a_stage_resumes_at_that_stage_and_a_stage_outside_the_track_is_refused() {
    let env = item_env(&["u1"]);
    let r = stall(&env, "u1");
    let g = claim(&env, "op");
    let why = rejected(env.engine.answer_act(
        &r.id,
        r.revision,
        g,
        &AnswerAct::Text {
            text: "go to nowhere",
            to_stage: Some("nowhere"),
        },
    ));
    assert!(why.contains("not a stage of track item"), "{why}");
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);

    env.engine
        .answer_act(
            &r.id,
            r.revision,
            g,
            &AnswerAct::Text {
                text: "take verify now",
                to_stage: Some("verify"),
            },
        )
        .unwrap();
    let after = request_of(&env, "u1");
    assert_eq!(
        answer_of(&after),
        json!({ "verb": "answer", "text": "take verify now", "to_stage": "verify" })
    );
    assert_eq!(unit_at(&env, "u1").2, 1, "the unit resumes at verify");
    let lines = ledger_lines(&env, "answer R-1@1");
    assert!(
        lines[0].ends_with(&format!(
            "answer R-1@1 by claim {g}: take verify now --to verify"
        )),
        "{}",
        lines[0]
    );
}

#[test]
fn an_option_whose_answer_effect_holds_a_stage_resumes_there() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    env.engine
        .park_as("u1", RequestKind::Park, Cause::ParkFile, "a park file")
        .unwrap();
    let parked = request_of(&env, "u1");
    // A block a workflow wrote: its option answers with a text and names the stage.
    let mut hand = body_of(&parked);
    hand["decision"]["options"] = json!([{
        "id": "w1",
        "label": "Take verify",
        "detail": "Answer and go to verify.",
        "effect": { "verb": "answer", "text": "go to verify", "to": "verify" },
        "by": "workflow",
    }]);
    end_session(&env, "u1");
    let row = seed_block(&env, "u1", &parked, RequestKind::Park, &hand);
    let g = claim(&env, "op");
    env.engine
        .answer_act(&row.id, row.revision, g, &AnswerAct::Option("w1"))
        .unwrap();
    let after = request_of(&env, "u1");
    assert_eq!(
        answer_of(&after),
        json!({
            "verb": "answer",
            "text": "go to verify",
            "to_stage": "verify",
            "option": { "id": "w1", "by": "workflow", "recommended": false },
        })
    );
    assert_eq!(unit_at(&env, "u1").2, 1);
    let lines = ledger_lines(&env, "option w1");
    assert!(
        lines[0].ends_with(&format!(
            "answer {}@{} by claim {g}: option w1 by workflow: go to verify --to verify",
            row.id, row.revision
        )),
        "{}",
        lines[0]
    );
}

/// Replace the body of a parked unit's open request: withdraw the request, open one that holds
/// `body` as given. `OpenRequest` stores a body as it is, so a block of any shape can be tested.
fn seed_block(
    env: &Env,
    unit: &str,
    parked: &RequestRow,
    kind: RequestKind,
    body: &Value,
) -> RequestRow {
    env.engine
        .withdraw_request(&parked.id, "replaced by a hand-made block")
        .unwrap();
    env.engine
        .open_request(Some(unit), kind, &parked.stage, &parked.reason, body)
        .unwrap();
    request_of(env, unit)
}

#[test]
fn an_option_with_a_text_or_a_stage_of_its_own_is_refused_and_changes_nothing() {
    let env = item_env(&["u1"]);
    let r = stop(&env, "u1");
    let g = claim(&env, "op");
    // The engine API cannot carry both, so the stored command is what is checked: a command that
    // carries both is refused by the engine's decision.
    use pf_engine::{Command, CommandBody};
    for answer in [
        json!({ "verb": "answer", "option": "s1", "text": "my own" }),
        json!({ "verb": "answer", "option": "s1", "to_stage": "verify" }),
    ] {
        let why = rejected(env.engine.submit(Command {
            id: format!("answer:{}:{}", r.id, r.revision),
            body: CommandBody::AnswerRequest {
                id: r.id.clone(),
                revision: r.revision,
                generation: g,
                answer,
                hook_failed: None,
            },
        }));
        assert!(
            why.contains("an act with an option carries no parameters of its own"),
            "{why}"
        );
    }
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(unit_at(&env, "u1").0, Status::Parked);
}

#[test]
fn an_unknown_option_an_option_at_another_revision_and_an_option_with_no_block_are_refused() {
    let env = item_env(&["u1", "u2"]);
    let r = stop(&env, "u1");
    let g = claim(&env, "op");
    let why = rejected(
        env.engine
            .answer_act(&r.id, r.revision, g, &AnswerAct::Option("s9")),
    );
    assert_eq!(
        why,
        format!(
            "request {} holds no option \"s9\" at revision {}",
            r.id, r.revision
        )
    );
    // An ID with a character outside a-z, 0-9, _ and - is refused before a read.
    let why = rejected(
        env.engine
            .answer_act(&r.id, r.revision, g, &AnswerAct::Option("S1")),
    );
    assert!(why.contains("option ID"), "{why}");
    let why = rejected(
        env.engine
            .answer_act(&r.id, r.revision, g, &AnswerAct::Option("")),
    );
    assert!(why.contains("option ID"), "{why}");
    let long = "a".repeat(33);
    let why = rejected(
        env.engine
            .answer_act(&r.id, r.revision, g, &AnswerAct::Option(&long)),
    );
    assert!(why.contains("option ID"), "{why}");
    // Another revision.
    let why = rejected(
        env.engine
            .answer_act(&r.id, r.revision + 1, g, &AnswerAct::Option("s1")),
    );
    assert!(why.contains("revision"), "{why}");
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);

    // A request with no block: opened by hand.
    env.engine.start_stage("u2").unwrap();
    env.engine
        .park_as("u2", RequestKind::Stall, Cause::Stalled, "stalled")
        .unwrap();
    let parked = request_of(&env, "u2");
    let mut bare = body_of(&parked);
    bare.as_object_mut().unwrap().remove("decision");
    let row = seed_block(&env, "u2", &parked, RequestKind::Stall, &bare);
    let why =
        rejected(
            env.engine
                .answer_act(&row.id, row.revision, g, &AnswerAct::Option("resume")),
        );
    assert!(why.contains("holds no decision block"), "{why}");
    assert_eq!(request_of(&env, "u2").status, RequestStatus::Open);
    // A block of another version is not read.
    let mut v2 = body_of(&parked);
    v2["decision"]["v"] = json!(2);
    let row = seed_block(&env, "u2", &request_of(&env, "u2"), RequestKind::Park, &v2);
    let why =
        rejected(
            env.engine
                .answer_act(&row.id, row.revision, g, &AnswerAct::Option("resume")),
        );
    assert!(why.contains("version 2"), "{why}");
    // A request with no unit takes no option.
    let run_level = {
        env.engine
            .open_request(None, RequestKind::Stop, "", "run level", &body_of(&parked))
            .unwrap();
        requests(&env)
            .into_iter()
            .find(|r| r.unit_id.is_none())
            .unwrap()
    };
    let why = rejected(env.engine.answer_act(
        &run_level.id,
        run_level.revision,
        g,
        &AnswerAct::Option("resume"),
    ));
    assert!(why.contains("names no unit"), "{why}");
    let why = rejected(env.engine.answer_act(
        &run_level.id,
        run_level.revision,
        g,
        &AnswerAct::Text {
            text: "x",
            to_stage: Some("verify"),
        },
    ));
    assert!(why.contains("names no unit"), "{why}");
}

#[test]
fn the_reader_of_a_stored_option_reads_by_from_the_cause_when_the_block_has_none() {
    let env = item_env(&["u1", "u2"]);
    let stopped = stop(&env, "u1");
    let mut body = body_of(&stopped);
    for option in body["decision"]["options"].as_array_mut().unwrap() {
        option.as_object_mut().unwrap().remove("by");
    }
    let row = RequestRow {
        body: body.to_string(),
        ..stopped
    };
    let option = stored_option(&row, "s2").unwrap();
    assert_eq!(
        option.by, "seat",
        "a stop_file block of round 1 has no `by`"
    );
    assert!(option.recommended);

    let stalled = stall(&env, "u2");
    let mut body = body_of(&stalled);
    for option in body["decision"]["options"].as_array_mut().unwrap() {
        option.as_object_mut().unwrap().remove("by");
    }
    let row = RequestRow {
        body: body.to_string(),
        ..stalled
    };
    let option = stored_option(&row, "resume").unwrap();
    assert_eq!(option.by, "controller");
    assert!(option.recommended, "a stall recommends resume");
}

#[test]
fn the_builder_writes_by_for_each_option_a_seats_for_a_stop_and_the_controllers_for_the_rest() {
    let env = item_env(&["u1", "u2", "u3"]);
    let stopped = stop(&env, "u1");
    for option in body_of(&stopped)["decision"]["options"].as_array().unwrap() {
        assert_eq!(option["by"], "seat");
    }
    let capped = cap(&env, "u2");
    for option in body_of(&capped)["decision"]["options"].as_array().unwrap() {
        assert_eq!(option["by"], "controller");
    }
    let stalled = stall(&env, "u3");
    assert_eq!(
        body_of(&stalled)["decision"]["options"][0]["by"],
        "controller"
    );
}

// ------------------------------------------------------------------ clear

#[test]
fn a_clear_with_again_resumes_at_the_capped_stage_and_go_on_resumes_where_the_unit_parked() {
    let env = item_env(&["u1", "u2"]);
    let r = cap(&env, "u1");
    let g = claim(&env, "op");
    env.engine
        .clear_unit_option("u1", "again", r.revision, g, &r.id)
        .unwrap();
    let after = request_of(&env, "u1");
    assert_eq!(
        answer_of(&after),
        json!({
            "verb": "clear",
            "to_stage": "verify",
            "option": { "id": "again", "by": "controller", "recommended": false },
        })
    );
    assert_eq!(unit_at(&env, "u1").2, 1);
    let lines = ledger_lines(&env, "option again");
    assert!(
        lines[0].ends_with(&format!(
            "answer {}@{} by claim {g}: option again by controller: clear u1 --to verify",
            r.id, r.revision
        )),
        "{}",
        lines[0]
    );

    let r = cap(&env, "u2");
    env.engine
        .clear_unit_option("u2", "go_on", r.revision, g, &r.id)
        .unwrap();
    assert_eq!(
        answer_of(&request_of(&env, "u2"))["to_stage"],
        "implement",
        "go_on resumes at the stage the unit parked in"
    );
    assert_eq!(unit_at(&env, "u2").2, 0);
}

#[test]
fn a_clear_with_an_option_needs_the_current_revision_and_no_stage_of_its_own() {
    let env = item_env(&["u1"]);
    let r = cap(&env, "u1");
    let g = claim(&env, "op");
    let why = rejected(
        env.engine
            .clear_unit_option("u1", "again", r.revision + 1, g, &r.id),
    );
    assert!(
        why.contains(&format!(
            "is at revision {}, not {}",
            r.revision,
            r.revision + 1
        )),
        "{why}"
    );
    use pf_engine::{Command, CommandBody};
    let park_generation = {
        let view = env.engine.view().unwrap();
        view.unit_detail(view.node("u1").unwrap())
            .unwrap()
            .park_generation
    };
    let command = |to_stage: Option<&str>, revision: Option<u32>| Command {
        id: format!("clear:u1:{park_generation}"),
        body: CommandBody::ClearUnit {
            unit_id: "u1".to_string(),
            to_stage: to_stage.map(str::to_string),
            generation: g,
            request_id: r.id.clone(),
            option: Some("again".to_string()),
            revision,
        },
    };
    let why = rejected(env.engine.submit(command(Some("verify"), Some(r.revision))));
    assert!(why.contains("carries no parameters of its own"), "{why}");
    let why = rejected(env.engine.submit(command(None, None)));
    assert!(why.contains("revision"), "{why}");
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
}

#[test]
fn an_option_taken_with_the_wrong_verb_is_refused_and_names_the_right_one() {
    let env = item_env(&["u1", "u2"]);
    let capped = cap(&env, "u1");
    let g = claim(&env, "op");
    // A resume option taken as an answer.
    let why = rejected(env.engine.answer_act(
        &capped.id,
        capped.revision,
        g,
        &AnswerAct::Option("again"),
    ));
    assert_eq!(
        why,
        format!(
            "option \"again\" of request {} resumes the unit: take it with `clear`",
            capped.id
        )
    );
    // An answer option taken as a clear.
    let stopped = stop(&env, "u2");
    let why = rejected(
        env.engine
            .clear_unit_option("u2", "s1", stopped.revision, g, &stopped.id),
    );
    assert_eq!(
        why,
        format!(
            "option \"s1\" of request {} answers with a text: take it with `answer`",
            stopped.id
        )
    );
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(request_of(&env, "u2").status, RequestStatus::Open);
}

// ------------------------------------------------------------------ ack

fn spine_workflow() -> Workflow {
    let mut wf = Workflow::single_track(
        "spine-flow",
        vec![StageDef::new("spine_close", "close-role", "model-a")],
        Some(MergeRule::ClaimAck),
        pf_engine::capacity::Settings::default(),
    );
    wf.tracks[0].name = "spine".to_string();
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.repo = "synthetic/repo".to_string();
    def.base = "main".to_string();
    def.head_branch = "spine/s1".to_string();
    wf
}

fn ready_to_ack() -> (Env, RequestRow) {
    let env = env(spine_workflow());
    env.engine
        .ingest(&mut TrackedSource {
            track: "spine".to_string(),
            records: vec![unit("spine", "spine", 1)],
        })
        .unwrap();
    env.forge.script(
        7,
        PrScript {
            base: "main".to_string(),
            checks: Checks::Green,
            mergeable: Mergeable::Clean,
            ..PrScript::green("spine/s1", SHA)
        },
    );
    env.engine.start_stage("spine").unwrap();
    env.engine
        .signal(
            "spine",
            "spine_close",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number: 7,
            }),
        )
        .unwrap();
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::AckRequested { .. }), "{step:?}");
    let r = env.engine.requests().unwrap().remove(0);
    (env, r)
}

#[test]
fn an_ack_with_approve_stores_the_option_beside_the_head_and_writes_its_line() {
    let (env, r) = ready_to_ack();
    let g = claim(&env, "op");
    let step = env
        .engine
        .ack_merge_option(&r.id, r.revision, g, Some("approve"))
        .unwrap();
    assert!(matches!(step, MergeStep::Merged { .. }), "{step:?}");
    let after = env.engine.requests().unwrap().remove(0);
    assert_eq!(
        answer_of(&after),
        json!({
            "verb": "ack",
            "head_sha": SHA,
            "option": { "id": "approve", "by": "controller", "recommended": false },
        })
    );
    let lines = ledger_lines(&env, "ack R-1@1");
    assert!(lines[0].ends_with(" option=approve"), "{}", lines[0]);
    assert!(lines[0].contains(&format!("sha={SHA}")), "{}", lines[0]);
}

#[test]
fn an_ack_with_no_option_is_as_it_was() {
    let (env, r) = ready_to_ack();
    let g = claim(&env, "op");
    env.engine.ack_merge(&r.id, r.revision, g).unwrap();
    let after = env.engine.requests().unwrap().remove(0);
    assert_eq!(
        after.answer.as_deref().unwrap(),
        json!({ "verb": "ack", "head_sha": SHA }).to_string()
    );
    let lines = ledger_lines(&env, "ack R-1@1");
    assert!(!lines[0].contains("option"), "{}", lines[0]);
    // The stored command holds no option key.
    let content = env
        .store
        .admission("run-1", "ack:R-1:1")
        .unwrap()
        .unwrap()
        .content;
    assert!(!content.contains("option"), "{content}");
}

#[test]
fn an_ack_option_is_checked_before_the_pr_is_read_and_a_refusal_writes_its_line() {
    let (env, r) = ready_to_ack();
    let g = claim(&env, "op");
    // The PR changed the base: a refusal of the PR would say so. The option refusal comes first.
    env.forge.scripts.lock().unwrap().get_mut(&7).unwrap().base = "other".to_string();
    let why = rejected(
        env.engine
            .ack_merge_option(&r.id, r.revision, g, Some("nope")),
    );
    assert!(why.contains("holds no option \"nope\""), "{why}");
    let lines = ledger_lines(&env, "ack R-1@1");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("refused") && l.contains("nope")),
        "{lines:?}"
    );
    assert_eq!(
        env.engine.requests().unwrap()[0].status,
        RequestStatus::Open
    );
}

#[test]
fn each_effect_names_its_own_verb_when_it_is_taken_with_another() {
    let env = item_env(&["u1"]);
    let r = stop(&env, "u1");
    let answer = stored_option(&r, "s1").unwrap();
    assert_eq!(
        answer.check_verb(&r.id, "ack").unwrap_err(),
        format!(
            "option \"s1\" of request {} answers with a text: take it with `answer`",
            r.id
        )
    );
    assert!(answer.check_verb(&r.id, "answer").is_ok());
    let (_, ack_request) = ready_to_ack();
    let approve = stored_option(&ack_request, "approve").unwrap();
    assert_eq!(
        approve.check_verb(&ack_request.id, "answer").unwrap_err(),
        format!(
            "option \"approve\" of request {} acks the merge: take it with `ack`",
            ack_request.id
        )
    );
    assert!(approve.check_verb(&ack_request.id, "ack").is_ok());
}

// ------------------------------------------------------------------ halted run, flags, effects

#[test]
fn an_option_act_on_a_halted_run_is_admitted_and_launches_nothing() {
    let env = item_env(&["u1"]);
    let r = stop(&env, "u1");
    let g = claim(&env, "op");
    env.engine.halt("operator stop").unwrap();
    env.engine
        .answer_act(&r.id, r.revision, g, &AnswerAct::Option("s1"))
        .unwrap();
    assert_eq!(
        unit_at(&env, "u1").0,
        Status::Running,
        "the unit runs again in the store"
    );
    assert_eq!(unit_at(&env, "u1").1.as_deref(), Some("run halted"));
    assert_eq!(env.agent.launch_count(), 1, "the answer started no launch");
}

/// An env whose stage `flagged` takes text, with the units `u1` and `u2` ingested.
fn flagged_env(flagged: usize) -> Env {
    let mut wf = item_workflow(&["implement", "verify"], true);
    wf.tracks[0].stages[flagged].takes_text = true;
    let env = env(wf);
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 0), unit("u2", "g1", 1)],
        })
        .unwrap();
    env
}

#[test]
fn a_stage_that_takes_text_makes_every_park_of_it_say_so_and_a_park_file_keeps_its_resume() {
    // `implement` takes text: a stall there says so.
    let env = flagged_env(0);
    let stalled = stall(&env, "u1");
    assert_eq!(body_of(&stalled)["decision"]["takes_text"], true);

    // A park file with no hook of its own: the fact is false, the stage's flag is true. The block
    // says a text reaches a seat and still holds the `resume` option.
    env.engine.start_stage("u2").unwrap();
    env.engine
        .park_with(
            "u2",
            RequestKind::Park,
            Cause::ParkFile,
            "a park file appeared",
            Some(&json!({ "takes_text": false })),
        )
        .unwrap();
    let block = body_of(&request_of(&env, "u2"))["decision"].clone();
    assert_eq!(block["takes_text"], true, "{block}");
    assert_eq!(block["options"][0]["id"], "resume", "{block}");

    // `verify` takes text, and the units park at `implement`: no flag, no text.
    let env = flagged_env(1);
    let stalled = stall(&env, "u1");
    assert_eq!(body_of(&stalled)["decision"]["takes_text"], false);
}

#[test]
fn only_a_merge_waits_ack_request_holds_an_option_with_the_effect_ack() {
    let causes: Vec<Cause> = Cause::ALL
        .into_iter()
        .filter(|c| *c != Cause::MergeWaitsAck)
        .collect();
    let ids: Vec<String> = (0..causes.len()).map(|i| format!("u{i}")).collect();
    let env = item_env(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    for (cause, id) in causes.iter().zip(&ids) {
        env.engine.start_stage(id).unwrap();
        let kind = match cause {
            Cause::StopFile => RequestKind::Stop,
            Cause::Stalled => RequestKind::Stall,
            Cause::SeatLost | Cause::HostUnreachable => RequestKind::SeatLost,
            Cause::MergeConflict
            | Cause::MergeChecksRed
            | Cause::MergeRefused
            | Cause::MergeError => RequestKind::MergeFailed,
            _ => RequestKind::Park,
        };
        let facts = match cause {
            Cause::OutcomeCap => Some(json!({ "cap_to": "verify", "cap_max": 2 })),
            Cause::StopFile => Some(json!({
                "stop_text": "t",
                "stop_options": [{ "text": "go on", "recommended": false }],
            })),
            _ => None,
        };
        env.engine
            .park_with(id, kind, *cause, "parked", facts.as_ref())
            .unwrap();
        let block = body_of(&request_of(&env, id))["decision"].clone();
        for option in block["options"].as_array().unwrap() {
            assert_ne!(option["effect"]["verb"], "ack", "{cause:?}: {block}");
            assert!(option["by"].is_string(), "{cause:?}: {block}");
        }
    }
    let (_, ack_request) = ready_to_ack();
    let block = body_of(&ack_request)["decision"].clone();
    assert_eq!(block["options"][0]["effect"]["verb"], "ack");
    assert_eq!(block["options"][0]["by"], "controller");
}
