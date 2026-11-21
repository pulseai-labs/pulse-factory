//! AC-4 of `r2.s11.w1`: every park site names its cause, and the request it opens holds the
//! decision block (`docs/contracts/requests.md`, "The decision block"). The driver runs on the fake
//! adapter and a bare forge. The test plays the seat: it writes a stop file, a `park_on` file or
//! nothing, and ends the session. Nothing here reaches the network.

mod common;

use std::path::Path;

use common::rig::*;
use common::spine::*;
use common::*;
use pf_adapter_fake::Scenario;
use pf_engine::port::store::{RequestKind, RequestRow};
use pf_engine::status::Status;
use pf_forge_bare::PrChecks;
use serde_json::{Value, json};

const STOP_WITH_OPTIONS: &str = "The plan names two branches.\nOptions:\n- build on main\n- [recommended] build on the spine branch\n- ask the operator\n";

/// The stop file's `on_answer` hook of the shipped package, with the state in the run directory.
const STOP_HOOK: &str = r#"        stop_file:
          file: "{run.dir}/out/{unit.id}/stop"
          on_answer:
            - [sh, "{run.package}/scripts/rule.sh", "{run.dir}/out/{unit.id}/rulings.md", "{run.dir}/out/{unit.id}/stop", "{answer.file}", "{answer.request}", "{answer.generation}", implement]
"#;

/// Two `park_on` files: one with the hook that takes the answer's text, one with none.
const PARK_ON: &str = r#"        park_on:
          - file: "{run.dir}/out/{unit.id}/gaps.json"
            reason: "gaps surfaced for {unit.id}: answer them with `pulse-factory answer`"
            on_answer:
              - [sh, "{run.package}/scripts/clarify.sh", "{unit.vars.handoff_path}", "{request.body_file}", "{answer.file}", "{answer.request}", "{answer.generation}", "{param.spine_dir}"]
          - file: "{run.dir}/out/{unit.id}/correction.txt"
            reason: "the red gate needs a correction"
"#;

fn options(stop: &str, extra: &str) -> Options {
    Options {
        implement_stop: stop.to_string(),
        implement_extra: extra.to_string(),
        item_vars: "[worktree_path, branch, handoff_path]".to_string(),
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    }
}

fn rig(stop: &str, extra: &str) -> Rig {
    Rig::with(&options(stop, extra), |_| {})
}

/// Queue `u1`. A rig built with `options` reads `handoff_path` as a unit variable.
fn queue(r: &Rig) {
    r.queue_item_with("u1", 1, &[("handoff_path", "/nonexistent/handoff.md")]);
}

/// Queue `u1` on a rig of the default options.
fn queue_plain(r: &Rig) {
    r.queue_item("u1", 1);
}

fn body_of(request: &RequestRow) -> Value {
    serde_json::from_str(&request.body).unwrap()
}

/// The one open request of `u1`, and its decision block.
fn open_block(r: &Rig) -> (RequestRow, Value) {
    let request = r
        .view()
        .open_request_of("u1")
        .cloned()
        .unwrap_or_else(|| panic!("no open request: {}", r.ledger_text()));
    let block = body_of(&request)["decision"].clone();
    (request, block)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn option_ids(block: &Value) -> Vec<String> {
    block["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_stop_with_options_opens_a_stop_request_that_holds_the_seats_options_and_mark() {
    let r = rig("        stop_file: \"{run.dir}/out/{unit.id}/stop\"\n", "");
    queue(&r);
    r.tick();
    write(&r.out("u1", "stop"), STOP_WITH_OPTIONS);
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::Stop);
    assert_eq!(block["cause"], "stop_file");
    assert_eq!(option_ids(&block), ["s1", "s2", "s3"]);
    assert_eq!(
        block["options"][1]["effect"],
        json!({ "verb": "answer", "text": "build on the spine branch" })
    );
    assert_eq!(block["recommended"]["option"], "s2");
    assert_eq!(block["recommended"]["by"], "seat");
    assert_eq!(
        block["asked_by"],
        json!({ "kind": "seat", "role": "implementer", "unit": "u1" })
    );
    assert_eq!(block["refs"]["unit"], "u1");
    assert_eq!(block["refs"]["stage"], "implement");
    assert_eq!(block["attempt"], 1);
    // The stage declares no hook for its stop file: a typed answer reaches no seat.
    assert_eq!(block["takes_text"], false);
    // The seat's words stay in the stop text, not in the summary.
    let summary = block["summary"].as_str().unwrap();
    assert!(!summary.contains("spine branch"), "{summary}");
    assert!(
        body_of(&request)["stop_text"]
            .as_str()
            .unwrap()
            .contains("ask the operator")
    );
}

#[test]
fn a_stop_whose_stage_declares_a_hook_takes_text() {
    let r = rig(STOP_HOOK, "");
    queue(&r);
    r.tick();
    write(&r.out("u1", "stop"), "Which branch?\n");
    r.tick();
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::Stop);
    assert_eq!(block["cause"], "stop_file");
    assert_eq!(block["takes_text"], true);
    // No options block: the seat named none, and that is no error.
    assert!(block["options"].as_array().unwrap().is_empty(), "{block}");
    assert!(block.get("options_error").is_none(), "{block}");
    assert!(block.get("recommended").is_none());
}

#[test]
fn a_stop_with_a_broken_options_block_still_stands_and_says_why() {
    let r = rig("        stop_file: \"{run.dir}/out/{unit.id}/stop\"\n", "");
    queue(&r);
    r.tick();
    write(&r.out("u1", "stop"), "Stuck.\nOptions:\n- only one\n");
    r.tick();
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::Stop);
    assert_eq!(block["cause"], "stop_file");
    assert!(block["options"].as_array().unwrap().is_empty());
    assert!(
        block["options_error"].as_str().unwrap().contains("2 to 4"),
        "{block}"
    );
    assert_eq!(
        body_of(&request)["stop_text"],
        "Stuck.\nOptions:\n- only one\n"
    );
}

#[test]
fn a_park_on_file_opens_a_request_with_its_cause_and_takes_text_when_its_entry_has_a_hook() {
    let r = rig(
        "        stop_file: \"{run.dir}/out/{unit.id}/stop\"\n",
        PARK_ON,
    );
    queue(&r);
    r.tick();
    write(
        &r.out("u1", "gaps.json"),
        &json!({ "gaps": [{ "section": "s", "question": "q", "severity": "blocking" }] })
            .to_string(),
    );
    r.tick();
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::Gaps);
    assert_eq!(block["cause"], "park_file");
    assert_eq!(block["takes_text"], true);
    // The typed answer is the act: no option.
    assert!(block["options"].as_array().unwrap().is_empty(), "{block}");
    assert_eq!(block["asked_by"]["kind"], "seat");
}

#[test]
fn a_park_on_file_with_no_hook_offers_one_resume_and_does_not_take_text() {
    let r = rig(
        "        stop_file: \"{run.dir}/out/{unit.id}/stop\"\n",
        PARK_ON,
    );
    queue(&r);
    r.tick();
    write(&r.out("u1", "correction.txt"), "fix the red gate\n");
    r.tick();
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::RedGate);
    assert_eq!(block["cause"], "park_file");
    assert_eq!(block["takes_text"], false);
    assert_eq!(option_ids(&block), ["resume"]);
    assert!(
        block["options"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("gone"),
        "{block}"
    );
    assert!(block.get("recommended").is_none());
}

#[test]
fn a_turn_that_ends_with_no_done_signal_opens_a_request_that_names_its_seat() {
    let r = Rig::new();
    // The seat ends its turn and wrote no done file.
    r.script("u1", vec![pf_adapter_fake::Step::TurnEnd]);
    queue_plain(&r);
    r.tick();
    r.fake
        .finish(&r.session_ref("u1").expect("an open session"));
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::Park);
    assert_eq!(request.reason, "seat ended with no done signal");
    assert_eq!(block["cause"], "no_done_signal");
    assert_eq!(block["asked_by"], json!({ "kind": "controller" }));
    assert_eq!(block["about"]["unit"], "u1");
    assert_eq!(block["about"]["role"], "implementer");
    assert_eq!(block["takes_text"], false);
    assert_eq!(option_ids(&block), ["resume"]);
    assert!(block.get("recommended").is_none());
}

#[test]
fn a_refused_launch_and_a_failed_launch_name_their_causes_and_recommend_nothing() {
    let r = Rig::new();
    r.fake
        .script("u1", Scenario::RefusedLaunch("no seat free".to_string()));
    queue_plain(&r);
    r.tick();
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::Park);
    assert_eq!(block["cause"], "launch_refused");
    assert!(block.get("recommended").is_none());
    assert_eq!(option_ids(&block), ["resume"]);

    let r = Rig::new();
    r.fake.script(
        "u1",
        Scenario::NoFirstTurn {
            screen: "the host is gone".to_string(),
        },
    );
    queue_plain(&r);
    r.tick();
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    let (_, block) = open_block(&r);
    assert_eq!(block["cause"], "launch_failed");
    // The agent port's launch error carries no mark that it is transient.
    assert!(block.get("recommended").is_none(), "{block}");
}

#[test]
fn red_checks_at_the_merge_open_a_merge_failed_request_that_recommends_a_resume() {
    let r = Rig::new();
    r.item("u1", 1, 5, PrChecks::Red);
    r.reach_close("u1");
    r.close("u1", 5);
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::MergeFailed);
    assert_eq!(block["cause"], "merge_checks_red");
    assert_eq!(block["recommended"]["option"], "resume");
    assert_eq!(block["recommended"]["by"], "controller");
    assert_eq!(block["attempt"], 0);
    assert_eq!(block["refs"]["stage"], "merge");
    assert!(block["refs"]["pr"].as_str().unwrap().ends_with("#5"));
    assert_eq!(block["takes_text"], false);
}

#[test]
fn an_outcome_cap_opens_a_park_request_whose_options_name_the_capped_stage() {
    let o = Options {
        review_loop: true,
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    r.item("u1", 1, 1, PrChecks::Green);
    r.reach_close("u1");
    r.script("u1", vec![pf_adapter_fake::Step::TurnEnd]);
    r.close("u1", 1);
    let old = git(&r.bare, &["rev-parse", "refs/heads/spine"]);
    // Three passes that quote an old head: each sends the unit to `review` again, up to its cap.
    for _ in 0..3 {
        let text = format!("verdict: pass\nhead: {old}\n\n- a finding\n");
        std::fs::write(r.out("u1", "review.md"), text).unwrap();
        r.fake.finish(&r.session_ref("u1").expect("an open review"));
        r.tick();
    }
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    let (request, block) = open_block(&r);
    assert_eq!(request.kind, RequestKind::Park);
    assert_eq!(request.reason, "review: review cap 2 reached");
    assert_eq!(block["cause"], "outcome_cap");
    assert_eq!(option_ids(&block), ["again", "go_on"]);
    assert_eq!(
        block["options"][0]["effect"],
        json!({ "verb": "resume", "to": "review" })
    );
    assert!(block.get("recommended").is_none());
    let body = body_of(&request);
    assert_eq!(body["cap_to"], "review");
    assert_eq!(body["cap_max"], 2);
    assert!(block["summary"].as_str().unwrap().contains('2'));
}

#[test]
fn a_merge_that_waits_on_the_ack_opens_a_request_with_one_approve_option() {
    let s = Spine::start();
    let fake = &s.live.ports.fake;
    fake.script(
        "lane",
        Scenario::Script(vec![cost(1_000), pf_adapter_fake::Step::TurnEnd]),
    );
    fake.script(
        "lane.2",
        Scenario::Script(vec![cost(2_000), pf_adapter_fake::Step::TurnEnd]),
    );
    fake.script(
        "spine",
        Scenario::Script(vec![cost(7_000), pf_adapter_fake::Step::TurnEnd]),
    );
    s.tick();
    s.item("u1", 1, 1);
    s.tick();
    s.finish_item("u1", 1);
    s.pass("lane", &s.out("lane", "lane.done"));
    s.tick();
    s.touch(&s.run_dir().join("rounds.done"));
    s.tick();
    assert!(s.has_unit("spine"));
    s.tick();
    s.forge
        .write_pr(9, SPINE_BRANCH, "main", PrChecks::Green)
        .unwrap();
    let head = s.head(SPINE_BRANCH);
    std::fs::create_dir_all(s.out("spine", "")).unwrap();
    std::fs::write(
        s.out("spine", "pr.url"),
        format!("file://{}#9\n", s.forge.repo()),
    )
    .unwrap();
    s.live.ports.fake.finish(&s.session_of("spine"));
    s.tick();

    let open = s.requests(RequestKind::MergeAck);
    assert_eq!(open.len(), 1, "{open:?}");
    let block = body_of(&open[0])["decision"].clone();
    assert_eq!(block["cause"], "merge_waits_ack");
    assert_eq!(option_ids(&block), ["approve"]);
    assert_eq!(block["options"][0]["effect"], json!({ "verb": "ack" }));
    // The ack is the operator's own act: never a recommendation.
    assert!(block.get("recommended").is_none(), "{block}");
    assert!(block["summary"].as_str().unwrap().contains(&head[..12]));
    assert_eq!(block["asked_by"], json!({ "kind": "controller" }));
    assert_eq!(block["takes_text"], false);
}
