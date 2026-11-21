//! AC-2 of `r2.s11.w4`: a park file's options come from the workflow. The park command carries the
//! fact `park_options` (`docs/contracts/requests.md`, "A workflow's options"), and the decision block of
//! a `park_file` request lists them as `w1` to `w4`, each `by: "workflow"`. Round 2's acts take them:
//! this file runs the engine's own calls and the fake ports.

mod common;

use common::*;
use pf_engine::claim::{
    AnswerAct, KEY_PARK_OPTIONS, OptionEffect, ParkOptionDecl, park_options_detail, sha256_hex,
    stored_option,
};
use pf_engine::port::store::{Cause, RequestKind, RequestRow, RequestStatus};
use pf_engine::{EndStatus, EngineError, Status};
use serde_json::{Value, json};

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

fn request_of(env: &Env, unit: &str) -> RequestRow {
    env.engine
        .requests()
        .unwrap()
        .into_iter()
        .rev()
        .find(|r| r.unit_id.as_deref() == Some(unit))
        .unwrap_or_else(|| panic!("no request for {unit}"))
}

fn body_of(r: &RequestRow) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

fn block_of(r: &RequestRow) -> Value {
    body_of(r)["decision"].clone()
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

fn claim(env: &Env) -> u64 {
    env.engine
        .claim("op", &sha256_hex(b"op"))
        .unwrap()
        .generation
}

fn declared(label: &str, answer: Option<&str>, to: Option<&str>, mark: bool) -> ParkOptionDecl {
    ParkOptionDecl {
        label: label.to_string(),
        answer: answer.map(str::to_string),
        to: to.map(str::to_string),
        recommended: mark,
    }
}

/// The three shapes: an answer with a stage (marked), an answer alone, and a stage alone.
fn three() -> Vec<ParkOptionDecl> {
    vec![
        declared("Go back", Some("Do it again."), Some("verify"), true),
        declared("Go on", Some("Go on."), None, false),
        declared("Skip ahead", None, Some("verify"), false),
    ]
}

/// The facts of a park: the declared options, and whether the file's own hook takes a text.
fn facts(options: &[ParkOptionDecl], takes_text: bool) -> Value {
    let mut facts = park_options_detail(options);
    facts["takes_text"] = json!(takes_text);
    facts
}

/// Park `unit` on a park file with `facts`, and end its seat's session as the driver does.
fn park(env: &Env, unit: &str, facts: Option<&Value>) -> RequestRow {
    park_as(env, unit, RequestKind::RedGate, Cause::ParkFile, facts)
}

fn park_as(
    env: &Env,
    unit: &str,
    kind: RequestKind,
    cause: Cause,
    facts: Option<&Value>,
) -> RequestRow {
    env.engine.start_stage(unit).unwrap();
    env.engine
        .park_with(unit, kind, cause, "the red gate failed", facts)
        .unwrap();
    env.engine
        .end(&format!("{unit}/implement#1"), EndStatus::Failed, "ended")
        .unwrap();
    request_of(env, unit)
}

fn unit_at(env: &Env, unit: &str) -> (Status, u32) {
    let view = env.engine.view().unwrap();
    let node = view.node(unit).unwrap();
    (node.status, view.unit_detail(node).unwrap().stages_done)
}

fn ids(block: &Value) -> Vec<String> {
    block["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect()
}

// ------------------------------------------------------------------ the block

#[test]
fn declared_options_are_the_blocks_options_in_file_order_by_the_workflow() {
    let env = item_env(&["u1"]);
    let block = block_of(&park(&env, "u1", Some(&facts(&three(), true))));
    assert_eq!(block["cause"], "park_file");
    assert_eq!(ids(&block), ["w1", "w2", "w3"]);
    assert_eq!(
        block["options"],
        json!([
            {
                "id": "w1",
                "label": "Go back",
                "detail": "Answer with the workflow's text, then resume the unit at stage verify.",
                "effect": { "verb": "answer", "text": "Do it again.", "to": "verify" },
                "by": "workflow",
            },
            {
                "id": "w2",
                "label": "Go on",
                "detail": "Answer with the workflow's text.",
                "effect": { "verb": "answer", "text": "Go on." },
                "by": "workflow",
            },
            {
                "id": "w3",
                "label": "Skip ahead",
                "detail": "Resume the unit at stage verify. The park file must be gone first, or the unit parks again.",
                "effect": { "verb": "resume", "to": "verify" },
                "by": "workflow",
            },
        ])
    );
    // The typed answer stays beside the options when a hook takes a text.
    assert_eq!(block["takes_text"], true);
}

#[test]
fn the_marked_option_is_recommended_with_the_fixed_reason_and_no_mark_gives_none() {
    let env = item_env(&["u1", "u2", "u3"]);
    let block = block_of(&park(&env, "u1", Some(&facts(&three(), true))));
    assert_eq!(
        block["recommended"],
        json!({
            "option": "w1",
            "by": "workflow",
            "why": "The workflow marked this option.",
        })
    );
    let unmarked: Vec<ParkOptionDecl> = three()
        .into_iter()
        .map(|mut o| {
            o.recommended = false;
            o
        })
        .collect();
    let block = block_of(&park(&env, "u2", Some(&facts(&unmarked, true))));
    assert!(block.get("recommended").is_none(), "{block}");
    // A mark on the second option names the second.
    let mut second = unmarked;
    second[1].recommended = true;
    let block = block_of(&park(&env, "u3", Some(&facts(&second, true))));
    assert_eq!(block["recommended"]["option"], "w2");
}

#[test]
fn declared_options_replace_the_controllers_resume_with_or_without_a_hook_fact() {
    let env = item_env(&["u1", "u2"]);
    for (unit, hook) in [("u1", true), ("u2", false)] {
        let block = block_of(&park(&env, unit, Some(&facts(&three(), hook))));
        assert_eq!(ids(&block), ["w1", "w2", "w3"], "hook {hook}");
        assert!(
            block["options"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["by"] == "workflow"),
            "{block}"
        );
        assert_eq!(block["takes_text"], hook, "takes_text follows the facts");
    }
}

#[test]
fn with_no_declared_option_the_block_is_as_it_was() {
    let env = item_env(&["u1", "u2", "u3", "u4"]);
    // No fact at all, and no hook: one `resume` by the controller, with the warning.
    let block = block_of(&park(&env, "u1", None));
    let warning = "Resume the unit at the stage it parked in. The park file must be gone first, or the unit parks again.";
    assert_eq!(
        block["options"],
        json!([{
            "id": "resume",
            "label": "Resume",
            "detail": warning,
            "effect": { "verb": "resume" },
            "by": "controller",
        }])
    );
    assert_eq!(block["takes_text"], false);
    // A fact that declares an empty list is no declaration.
    let none = json!({ "takes_text": false, KEY_PARK_OPTIONS: [] });
    assert_eq!(ids(&block_of(&park(&env, "u2", Some(&none)))), ["resume"]);
    // A hook fact and no option: the typed answer is the act, so no option.
    let hook = json!({ "takes_text": true });
    let block = block_of(&park(&env, "u3", Some(&hook)));
    assert!(block["options"].as_array().unwrap().is_empty(), "{block}");
    assert_eq!(block["takes_text"], true);
    // Entries that hold nothing to do count as no declaration either.
    let empty = json!({ "takes_text": false, KEY_PARK_OPTIONS: [{ "label": "Nothing" }] });
    assert_eq!(ids(&block_of(&park(&env, "u4", Some(&empty)))), ["resume"]);
}

#[test]
fn the_block_names_the_seat_that_wrote_the_file_with_its_role_and_unit() {
    let env = item_env(&["u1"]);
    let block = block_of(&park(&env, "u1", Some(&facts(&three(), true))));
    assert_eq!(
        block["asked_by"],
        json!({ "kind": "seat", "role": "implement-role", "unit": "u1" })
    );
    assert_eq!(block["v"], 1);
    assert!(block["summary"].as_str().unwrap().contains("u1"));
    assert_eq!(block["refs"]["unit"], "u1");
}

#[test]
fn an_entry_with_no_label_or_with_neither_text_nor_stage_is_skipped_and_ids_keep_their_place() {
    let env = item_env(&["u1"]);
    let entries = json!({
        "takes_text": true,
        KEY_PARK_OPTIONS: [
            { "answer": "no label", "recommended": true },
            { "label": "", "answer": "empty label" },
            { "label": "Nothing to do", "recommended": true },
            { "label": "Real", "answer": "A real answer." },
            { "label": 7, "answer": "label is no text" },
            { "label": "Fifth", "to": "verify" },
        ],
    });
    let block = block_of(&park(&env, "u1", Some(&entries)));
    // The first entry with a label and something to do is the fourth. Only four entries are read, so
    // the fifth and the sixth are not.
    assert_eq!(ids(&block), ["w4"], "{block}");
    // None of the skipped entries gave the mark.
    assert!(block.get("recommended").is_none(), "{block}");
}

#[test]
fn at_most_four_options_are_read() {
    let env = item_env(&["u1"]);
    let many: Vec<Value> = (1..=6)
        .map(|i| json!({ "label": format!("Option {i}"), "to": "verify" }))
        .collect();
    let facts = json!({ "takes_text": false, KEY_PARK_OPTIONS: many });
    let block = block_of(&park(&env, "u1", Some(&facts)));
    assert_eq!(ids(&block), ["w1", "w2", "w3", "w4"]);
}

#[test]
fn an_entry_that_holds_an_ack_or_an_effect_key_gives_no_ack_effect() {
    let env = item_env(&["u1"]);
    let hostile = json!({
        "takes_text": false,
        KEY_PARK_OPTIONS: [
            { "label": "Ack it", "verb": "ack", "answer": "go" },
            { "label": "Effect", "effect": { "verb": "ack" }, "to": "verify" },
            { "label": "Both", "verb": "ack", "effect": { "verb": "ack" }, "id": "approve", "by": "controller",
              "answer": "text", "to": "verify" },
        ],
    });
    let block = block_of(&park(&env, "u1", Some(&hostile)));
    let options = block["options"].as_array().unwrap();
    assert_eq!(options.len(), 3);
    for o in options {
        assert!(
            matches!(o["effect"]["verb"].as_str(), Some("answer" | "resume")),
            "{o}"
        );
        assert_eq!(o["by"], "workflow", "{o}");
    }
    assert_eq!(
        ids(&block),
        ["w1", "w2", "w3"],
        "a key of the entry sets no id"
    );
    assert_eq!(
        options[1]["effect"],
        json!({ "verb": "resume", "to": "verify" })
    );
}

#[test]
fn the_fact_under_another_cause_adds_no_option() {
    let env = item_env(&["u1", "u2", "u3"]);
    // Under a stop file the options are the seat's own: the fact is not read.
    let stop_facts = json!({
        "stop_text": "which way?",
        "stop_options": [
            { "text": "go on", "recommended": false },
            { "text": "stop here", "recommended": true },
        ],
        KEY_PARK_OPTIONS: [{ "label": "Workflow", "answer": "x", "to": "verify" }],
    });
    let block = block_of(&park_as(
        &env,
        "u1",
        RequestKind::Stop,
        Cause::StopFile,
        Some(&stop_facts),
    ));
    assert_eq!(ids(&block), ["s1", "s2"], "{block}");
    // Under a stall, the controller's `resume` stands.
    let block = block_of(&park_as(
        &env,
        "u2",
        RequestKind::Stall,
        Cause::Stalled,
        Some(&facts(&three(), true)),
    ));
    assert_eq!(ids(&block), ["resume"], "{block}");
    assert_eq!(block["options"][0]["by"], "controller");
    // Under an outcome cap, the cap's own two options stand.
    let body = body_of(&park_as(
        &env,
        "u3",
        RequestKind::Park,
        Cause::OutcomeCap,
        Some(&facts(&three(), false)),
    ));
    assert_eq!(body["decision"]["options"].as_array().unwrap().len(), 2);
    assert_eq!(body["decision"]["options"][0]["by"], "controller");
    assert_eq!(body["decision"]["options"][0]["id"], "again");
}

#[test]
fn the_body_root_holds_the_fact_and_a_fact_named_decision_cannot_replace_the_block() {
    let env = item_env(&["u1", "u2"]);
    let r = park(&env, "u1", Some(&facts(&three(), true)));
    let body = body_of(&r);
    assert_eq!(body[KEY_PARK_OPTIONS].as_array().unwrap().len(), 3);
    assert_eq!(body[KEY_PARK_OPTIONS][0]["label"], "Go back");
    assert_eq!(body[KEY_PARK_OPTIONS][0]["answer"], "Do it again.");
    assert_eq!(body[KEY_PARK_OPTIONS][0]["to"], "verify");
    assert_eq!(body[KEY_PARK_OPTIONS][0]["recommended"], true);
    assert!(
        body[KEY_PARK_OPTIONS][1].get("to").is_none(),
        "an absent stage is left out"
    );
    assert!(
        body[KEY_PARK_OPTIONS][2].get("answer").is_none(),
        "an absent text is left out"
    );

    env.engine.start_stage("u2").unwrap();
    let forged = json!({ "decision": { "v": 1, "options": [] }, KEY_PARK_OPTIONS: [] });
    let why = rejected(env.engine.park_with(
        "u2",
        RequestKind::RedGate,
        Cause::ParkFile,
        "red",
        Some(&forged),
    ));
    assert!(why.contains("decision"), "{why}");
}

// ------------------------------------------------------------------ reading an option back

#[test]
fn the_reader_of_a_stored_option_reads_w1_as_the_workflows_recommended_answer_with_its_stage() {
    let env = item_env(&["u1"]);
    let r = park(&env, "u1", Some(&facts(&three(), true)));
    let w1 = stored_option(&r, "w1").unwrap();
    assert_eq!(w1.by, "workflow");
    assert!(w1.recommended);
    assert_eq!(
        w1.effect,
        OptionEffect::Answer {
            text: "Do it again.".to_string(),
            to: Some("verify".to_string()),
        }
    );
    let w2 = stored_option(&r, "w2").unwrap();
    assert!(!w2.recommended);
    assert_eq!(
        w2.effect,
        OptionEffect::Answer {
            text: "Go on.".to_string(),
            to: None,
        }
    );
    assert_eq!(
        stored_option(&r, "w3").unwrap().effect,
        OptionEffect::Resume {
            to: Some("verify".to_string())
        }
    );
    assert_eq!(w1.ledger(), "option w1 by workflow, recommended");
}

// ------------------------------------------------------------------ the acts

#[test]
fn an_answer_with_w1_stores_the_text_the_stage_and_the_option_and_resumes_the_unit_there() {
    let env = item_env(&["u1"]);
    let r = park(&env, "u1", Some(&facts(&three(), true)));
    let g = claim(&env);
    env.engine
        .answer_act(&r.id, r.revision, g, &AnswerAct::Option("w1"))
        .unwrap();
    let after = request_of(&env, "u1");
    assert_eq!(after.status, RequestStatus::Answered);
    assert_eq!(
        answer_of(&after),
        json!({
            "verb": "answer",
            "text": "Do it again.",
            "to_stage": "verify",
            "option": { "id": "w1", "by": "workflow", "recommended": true },
        })
    );
    // The unit resumed at the stage the option names, so `implement` counts as done.
    assert_eq!(unit_at(&env, "u1"), (Status::Running, 1));
    let line = env
        .ledger_text()
        .lines()
        .find(|l| l.contains("option w1"))
        .expect("a ledger line names the option")
        .to_string();
    assert!(
        line.ends_with(&format!(
            "answer {}@{} by claim {g}: option w1 by workflow, recommended: Do it again. --to verify",
            r.id, r.revision
        )),
        "{line}"
    );
}

#[test]
fn an_answer_with_w2_resumes_at_the_stage_the_unit_parked_in() {
    let env = item_env(&["u1"]);
    let r = park(&env, "u1", Some(&facts(&three(), true)));
    let g = claim(&env);
    env.engine
        .answer_act(&r.id, r.revision, g, &AnswerAct::Option("w2"))
        .unwrap();
    assert_eq!(
        answer_of(&request_of(&env, "u1")),
        json!({
            "verb": "answer",
            "text": "Go on.",
            "option": { "id": "w2", "by": "workflow", "recommended": false },
        })
    );
    assert_eq!(unit_at(&env, "u1"), (Status::Running, 0));
}

#[test]
fn a_clear_takes_a_to_only_option_at_the_requests_revision() {
    let env = item_env(&["u1", "u2"]);
    let r = park(&env, "u1", Some(&facts(&three(), true)));
    let g = claim(&env);
    // A stale revision is refused.
    let why = rejected(
        env.engine
            .clear_unit_option("u1", "w3", r.revision + 1, g, &r.id),
    );
    assert!(why.contains("revision"), "{why}");
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    env.engine
        .clear_unit_option("u1", "w3", r.revision, g, &r.id)
        .unwrap();
    assert_eq!(
        answer_of(&request_of(&env, "u1")),
        json!({
            "verb": "clear",
            "to_stage": "verify",
            "option": { "id": "w3", "by": "workflow", "recommended": false },
        })
    );
    assert_eq!(unit_at(&env, "u1"), (Status::Running, 1));
    let line = env
        .ledger_text()
        .lines()
        .find(|l| l.contains("option w3"))
        .expect("a ledger line names the option")
        .to_string();
    assert!(
        line.ends_with(&format!(
            "answer {}@{} by claim {g}: option w3 by workflow: clear u1 --to verify",
            r.id, r.revision
        )),
        "{line}"
    );
}

#[test]
fn an_option_taken_with_the_wrong_verb_is_refused_and_names_the_right_one() {
    let env = item_env(&["u1"]);
    let r = park(&env, "u1", Some(&facts(&three(), true)));
    let g = claim(&env);
    let why = rejected(
        env.engine
            .answer_act(&r.id, r.revision, g, &AnswerAct::Option("w3")),
    );
    assert_eq!(
        why,
        format!(
            "option \"w3\" of request {} resumes the unit: take it with `clear`",
            r.id
        )
    );
    let why = rejected(
        env.engine
            .clear_unit_option("u1", "w1", r.revision, g, &r.id),
    );
    assert_eq!(
        why,
        format!(
            "option \"w1\" of request {} answers with a text: take it with `answer`",
            r.id
        )
    );
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
}

#[test]
fn an_option_whose_stage_is_outside_the_units_track_is_refused_at_the_act_and_changes_nothing() {
    let env = item_env(&["u1", "u2"]);
    let off_track = vec![
        declared("Off the track", Some("Go."), Some("nowhere"), false),
        declared("Off the track, no text", None, Some("nowhere"), false),
    ];
    let r = park(&env, "u1", Some(&facts(&off_track, true)));
    let g = claim(&env);
    let why = rejected(
        env.engine
            .answer_act(&r.id, r.revision, g, &AnswerAct::Option("w1")),
    );
    assert!(why.contains("nowhere"), "{why}");
    let why = rejected(
        env.engine
            .clear_unit_option("u1", "w2", r.revision, g, &r.id),
    );
    assert!(why.contains("nowhere"), "{why}");
    assert_eq!(request_of(&env, "u1").status, RequestStatus::Open);
    assert_eq!(unit_at(&env, "u1").0, Status::Parked);
    assert!(!env.ledger_text().contains("option w1"));
}
