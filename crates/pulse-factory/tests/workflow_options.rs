//! AC-4 of `r2.s11.w4`: a workflow's own `park_on` file names the options of its park. The driver
//! reads the entry that opened the request and adds the options to the park's facts, so the
//! request's decision block lists them (`by: "workflow"`) and round 2's acts take them. The real
//! package's `rule.sh` is the hook, as `park_note.rs` uses it. The driver runs on the fake adapter and
//! a bare forge; the tests play the seat. Nothing here reaches the network or spends.

#[path = "common/needs_you_rig.rs"]
mod api_rig;
mod common;

use std::sync::Arc;

use common::chat::{controller_ledger, ledger_kind};
use common::rig::Rig;
use common::*;
use pf_api::ApiHost;
use pf_api::code::PairingCode;
use pf_api::types::DeviceRecord;
use pf_engine::claim::{AnswerAct, sha256_hex};
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus, Store};
use pf_engine::status::Status;
use pf_ipc::{Handler, Peer, Request, Response};
use pulse_factory::controller::api::ApiBridge;
use serde_json::{Value, json};

/// The `park_on` entries of the stage `implement`: a red gate with the hook of the shipped package and
/// two options, a hold with two stage-only options and no hook, a plain file with neither, and a
/// gaps file.
const PARK_ON: &str = r#"        park_on:
          - file: "{run.dir}/out/{unit.id}/correction.txt"
            reason: "the red gate needs a correction"
            on_answer:
              - [sh, "{run.package}/scripts/rule.sh", "{run.dir}/out/{unit.id}/rulings.md", "{run.dir}/out/{unit.id}/correction.txt", "{answer.file}", "{answer.request}", "{answer.generation}", implement, "{answer.source}"]
            options:
              - label: "The correction is made: go on to verify"
                answer: "The correction is made. Go on to verify."
                to: verify
                recommended: true
              - label: "A flaky failure: run the stage again"
                answer: "Treat the failure as flaky. Run the stage again."
                to: implement
          - file: "{run.dir}/out/{unit.id}/hold.txt"
            reason: "a hold"
            options:
              - label: "Go on to verify"
                to: verify
              - label: "Run the stage again"
                to: implement
          - file: "{run.dir}/out/{unit.id}/plain.txt"
            reason: "a plain park"
          - file: "{run.dir}/out/{unit.id}/gaps.json"
            reason: "gaps"
"#;

const STOP: &str =
    "Which branch?\nOptions:\n- build on main\n- [recommended] build on the spine branch\n";

fn options() -> Options {
    Options {
        implement_extra: PARK_ON.to_string(),
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    }
}

fn rig() -> Rig {
    Rig::with(&options(), |_| {})
}

fn claim(r: &Rig) -> u64 {
    r.engine
        .claim("ann@host", &sha256_hex(&[7u8; 32]))
        .unwrap()
        .generation
}

fn open_request(r: &Rig, unit: &str) -> RequestRow {
    r.view()
        .open_request_of(unit)
        .cloned()
        .unwrap_or_else(|| panic!("no open request for {unit}: {}", r.ledger_text()))
}

fn block_of(request: &RequestRow) -> Value {
    let body: Value = serde_json::from_str(&request.body).unwrap();
    body["decision"].clone()
}

fn ids(block: &Value) -> Vec<String> {
    block["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect()
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Launch `unit`, write `file` as its seat, and tick: the unit parks on it. Returns its request.
fn park_on(r: &Rig, unit: &str, file: &str, text: &str) -> RequestRow {
    if !r.has_unit(unit) {
        r.queue_item(unit, 1);
        r.tick();
    }
    std::fs::create_dir_all(r.out(unit, "")).unwrap();
    std::fs::write(r.out(unit, file), text).unwrap();
    r.tick();
    assert_eq!(r.unit(unit).0, Status::Parked, "{}", r.ledger_text());
    open_request(r, unit)
}

fn stage_nodes(r: &Rig, unit: &str, stage: &str) -> usize {
    r.stage_node(unit, stage).len()
}

// ------------------------------------------------------------------ the block

#[test]
fn a_correction_park_opens_a_red_gate_request_whose_block_lists_the_entrys_options() {
    let r = rig();
    let req = park_on(&r, "u1", "correction.txt", "gate red: AC-2 exits 1\n");
    assert_eq!(req.kind, RequestKind::RedGate);
    let block = block_of(&req);
    assert_eq!(block["cause"], "park_file");
    assert_eq!(ids(&block), ["w1", "w2"]);
    assert_eq!(
        block["options"][0],
        json!({
            "id": "w1",
            "label": "The correction is made: go on to verify",
            "detail": "Answer with the workflow's text, then resume the unit at stage verify.",
            "effect": { "verb": "answer", "text": "The correction is made. Go on to verify.", "to": "verify" },
            "by": "workflow",
        })
    );
    assert_eq!(block["options"][1]["by"], "workflow");
    assert_eq!(block["options"][1]["effect"]["to"], "implement");
    assert_eq!(
        block["recommended"],
        json!({ "option": "w1", "by": "workflow", "why": "The workflow marked this option." })
    );
    assert_eq!(
        block["asked_by"],
        json!({ "kind": "seat", "role": "implementer", "unit": "u1" })
    );
    assert_eq!(block["takes_text"], true);
    // The facts ride in the request's body, as the park command carried them.
    let body: Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(body["park_options"].as_array().unwrap().len(), 2);
}

#[test]
fn answer_with_w1_runs_the_entrys_hook_and_the_unit_runs_the_options_stage() {
    let r = rig();
    let req = park_on(&r, "u1", "correction.txt", "gate red: AC-2 exits 1\n");
    let generation = claim(&r);
    let answered = r
        .driver
        .answer_act(&req.id, 1, generation, &AnswerAct::Option("w1"))
        .unwrap();
    assert!(
        answered.hook_ran && answered.hook_failed.is_none(),
        "{answered:?}"
    );
    assert_eq!(r.unit("u1").0, Status::Running, "{}", r.ledger_text());

    // The heading says that the workflow wrote the option; the packet is quoted, and the entry holds
    // the option's text.
    let rulings = read(&r.out("u1", "rulings.md"));
    let heading = rulings
        .lines()
        .find(|l| l.starts_with("## "))
        .expect("a heading");
    assert!(
        heading.ends_with(" — stage implement — claim 1 — option w1, written by the workflow"),
        "{heading}"
    );
    assert!(rulings.contains("> gate red: AC-2 exits 1"), "{rulings}");
    assert_eq!(
        rulings.lines().rfind(|l| !l.is_empty()).unwrap(),
        "The correction is made. Go on to verify."
    );
    // The packet is retired, so the unit does not park on it again.
    assert!(!r.out("u1", "correction.txt").exists());
    assert!(
        r.out("u1", &format!("correction.txt.answered-{}", req.id))
            .exists()
    );

    // The next stage node is the option's stage, and not the parked one.
    r.tick();
    r.tick();
    assert_eq!(stage_nodes(&r, "u1", "implement"), 1, "{}", r.ledger_text());
    assert_eq!(stage_nodes(&r, "u1", "verify"), 1, "{}", r.ledger_text());
    assert!(r.view().open_request_of("u1").is_none());

    // The run ledger names the option, who wrote it, the mark and the stage.
    let lines = r.ledger_lines(&format!("answer {}@1", req.id));
    assert_eq!(lines.len(), 1, "{}", r.ledger_text());
    assert!(
        lines[0].contains(": option w1 by workflow, recommended: ")
            && lines[0].ends_with(" --to verify"),
        "{}",
        lines[0]
    );
    let stored: Value = serde_json::from_str(
        r.view()
            .requests
            .iter()
            .find(|q| q.id == req.id)
            .unwrap()
            .answer
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        stored["option"],
        json!({ "id": "w1", "by": "workflow", "recommended": true })
    );
    assert_eq!(stored["to_stage"], "verify");
}

#[test]
fn a_typed_answer_on_a_second_red_gate_request_still_works() {
    let r = rig();
    let first = park_on(&r, "u1", "correction.txt", "gate red\n");
    let generation = claim(&r);
    r.driver
        .answer_act(&first.id, 1, generation, &AnswerAct::Option("w2"))
        .unwrap();
    r.tick();
    // The second red gate, on the same unit: the typed answer is the act, as before.
    let second = park_on(&r, "u1", "correction.txt", "gate red again\n");
    assert_ne!(second.id, first.id);
    assert_eq!(ids(&block_of(&second)), ["w1", "w2"]);
    let answered = r
        .driver
        .answer_request(&second.id, 1, generation, "fixed it by hand")
        .unwrap();
    assert!(
        answered.hook_ran && answered.hook_failed.is_none(),
        "{answered:?}"
    );
    let rulings = read(&r.out("u1", "rulings.md"));
    let last_heading = rulings.lines().rfind(|l| l.starts_with("## ")).unwrap();
    assert!(!last_heading.contains("option"), "{last_heading}");
    assert!(rulings.contains("fixed it by hand"), "{rulings}");
}

#[test]
fn a_clear_takes_a_to_only_option_and_the_unit_runs_that_stage() {
    let r = rig();
    let req = park_on(&r, "u1", "hold.txt", "held\n");
    assert_eq!(req.kind, RequestKind::Park);
    let block = block_of(&req);
    assert_eq!(ids(&block), ["w1", "w2"]);
    assert_eq!(
        block["options"][0]["effect"],
        json!({ "verb": "resume", "to": "verify" })
    );
    assert!(block.get("recommended").is_none(), "{block}");
    assert_eq!(block["takes_text"], false);
    // The entry has no hook and the stage has no fallback: no text reaches a seat.
    let generation = claim(&r);
    let err = r
        .driver
        .answer_request(&req.id, 1, generation, "go on")
        .unwrap_err();
    assert!(err.contains("takes no text"), "{err}");

    // A stale revision is refused; the current one takes the option.
    let err = r
        .driver
        .clear_option("u1", "w1", 2, generation, &req.id)
        .unwrap_err();
    assert!(err.contains("revision"), "{err}");
    r.driver
        .clear_option("u1", "w1", 1, generation, &req.id)
        .unwrap();
    std::fs::remove_file(r.out("u1", "hold.txt")).unwrap();
    r.tick();
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Running, "{}", r.ledger_text());
    assert_eq!(stage_nodes(&r, "u1", "verify"), 1, "{}", r.ledger_text());
    let lines = r.ledger_lines(&format!("answer {}@1", req.id));
    assert!(
        lines[0].contains(": option w1 by workflow: clear u1 --to verify"),
        "{}",
        lines[0]
    );
}

#[test]
fn an_entry_with_no_hook_no_fallback_and_no_options_gives_one_resume_and_takes_no_text() {
    let r = rig();
    let req = park_on(&r, "u1", "plain.txt", "plain\n");
    let block = block_of(&req);
    assert_eq!(ids(&block), ["resume"]);
    assert_eq!(block["options"][0]["by"], "controller");
    assert_eq!(
        block["options"][0]["detail"],
        "Resume the unit at the stage it parked in. The park file must be gone first, or the unit parks again."
    );
    assert_eq!(block["takes_text"], false);
    let generation = claim(&r);
    let err = r
        .driver
        .answer_request(&req.id, 1, generation, "go on")
        .unwrap_err();
    assert!(err.contains("takes no text"), "{err}");
    assert_eq!(r.unit("u1").0, Status::Parked);
}

#[test]
fn a_gaps_park_gives_a_block_with_no_workflow_option() {
    let r = rig();
    let req = park_on(&r, "u1", "gaps.json", "{}");
    assert_eq!(req.kind, RequestKind::Gaps);
    let block = block_of(&req);
    assert_eq!(block["cause"], "park_file");
    assert!(ids(&block).iter().all(|id| !id.starts_with('w')), "{block}");
    let body: Value = serde_json::from_str(&req.body).unwrap();
    assert!(body.get("park_options").is_none(), "{body}");
}

// ------------------------------------------------------------------ a device, and show

fn me() -> Peer {
    Peer {
        pid: Some(std::process::id()),
    }
}

struct World {
    api: api_rig::Api,
    bridge: Arc<ApiBridge>,
    d1: DeviceRecord,
    counter: std::cell::Cell<u32>,
}

fn world() -> World {
    let api = api_rig::api_with(options());
    let live = &api.s.live;
    let reply = live
        .controller
        .handle_from(Request::new("device_pair", json!({})), me());
    assert!(reply.ok, "{:?}", reply.error);
    let code = PairingCode::decode(reply.result["code"].as_str().unwrap()).unwrap();
    let bridge = Arc::new(ApiBridge::new(live.controller.clone()));
    let d1 = bridge
        .redeem(&code.secret_hex(), "MacBook", "127.0.0.1".parse().unwrap())
        .unwrap_or_else(|e| panic!("pairing: {e:?}"))
        .device;
    World {
        api,
        bridge,
        d1,
        counter: std::cell::Cell::new(0),
    }
}

impl World {
    fn cmd(&self) -> String {
        self.counter.set(self.counter.get() + 1);
        format!("cmd-{}", self.counter.get())
    }

    fn call(&self, verb: &str, args: Value) -> Response {
        self.bridge.call(&self.d1, verb, args)
    }

    fn code_for(&self, subject: Value) -> String {
        let r = self.call(
            "confirm_code",
            json!({ "subject": subject, "command_id": self.cmd() }),
        );
        assert!(r.ok, "confirm_code refused: {:?}", r.error);
        r.result["code"].as_str().unwrap().to_string()
    }

    /// The seat of `unit` writes its correction packet: the unit parks on it. Returns the request.
    fn red_gate(&self, unit: &str, number: u64) -> String {
        let s = &self.api.s;
        s.item(unit, number as u32, number);
        s.tick();
        std::fs::create_dir_all(s.out(unit, "")).unwrap();
        std::fs::write(s.out(unit, "correction.txt"), "gate red: AC-2 exits 1\n").unwrap();
        s.tick();
        let open: Vec<_> = s
            .requests(RequestKind::RedGate)
            .into_iter()
            .filter(|r| r.status == RequestStatus::Open && r.unit_id.as_deref() == Some(unit))
            .collect();
        assert_eq!(open.len(), 1, "{open:?}");
        open[0].id.clone()
    }

    fn status_of(&self, request: &str) -> RequestStatus {
        self.api
            .s
            .live
            .controller
            .store()
            .request(request)
            .unwrap()
            .unwrap()
            .status
    }
}

fn subject(request: &str, verb: &str, option: &str) -> Value {
    json!({ "kind": "request", "request": request, "revision": 1, "verb": verb, "option": option })
}

#[test]
fn a_device_takes_a_code_for_w1_and_spends_it_and_a_code_for_w1_spent_with_w2_is_refused() {
    let w = world();
    let id = w.red_gate("u1", 1);

    // A code for `w1` spent with `w2` is a refused subject, and moves no claim.
    let code = w.code_for(subject(&id, "answer", "w1"));
    let wrong = w.call(
        "answer",
        json!({ "request": id, "rev": 1, "text": "", "option": "w2", "code": code, "command_id": w.cmd() }),
    );
    assert!(!wrong.ok, "{wrong:?}");
    assert_eq!(wrong.result["refusal"], "code_subject", "{wrong:?}");
    assert_eq!(w.status_of(&id), RequestStatus::Open);
    assert!(
        w.api.s.live.controller.store().claims().unwrap().is_empty(),
        "a refused spend moves no claim"
    );

    // The right spend takes the option; the empty text means that the option is the act.
    let code = w.code_for(subject(&id, "answer", "w1"));
    let spent = w.call(
        "answer",
        json!({ "request": id, "rev": 1, "text": "", "option": "w1", "code": code, "command_id": w.cmd() }),
    );
    assert!(spent.ok, "{:?}", spent.error);
    assert_eq!(spent.result["option"], "w1", "{}", spent.result);
    assert_eq!(w.status_of(&id), RequestStatus::Answered);
    let lines = ledger_kind(&w.api.s.live, "device_answer");
    let admitted: Vec<_> = lines.iter().filter(|l| !l.contains("refused")).collect();
    assert_eq!(admitted.len(), 1, "{lines:?}");
    assert!(
        admitted[0].contains("D-1") && admitted[0].contains(" option=w1"),
        "{}",
        admitted[0]
    );
    // The ledgers hold no code and no text of the option.
    let all = format!(
        "{}\n{}",
        controller_ledger(&w.api.s.live).join("\n"),
        w.api.api_ledger().join("\n")
    );
    for secret in [code.as_str(), "The correction is made. Go on to verify."] {
        assert!(!all.contains(secret), "a ledger holds {secret:?}");
    }
    // The run ledger names the option and who wrote it.
    assert!(
        w.api
            .s
            .ledger()
            .contains(": option w1 by workflow, recommended: "),
        "{}",
        w.api.s.ledger()
    );
}

#[test]
fn show_names_who_wrote_each_option_a_workflow_a_seat_and_the_controller() {
    let w = world();
    let red = w.red_gate("u1", 1);
    let live = &w.api.s.live;
    let shown = |id: &str| {
        let out = live.cli(&["show", id]);
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out)
    };

    // A workflow's option: who wrote it, the text, the stage and the fixed reason.
    let text = shown(&red);
    assert!(
        text.contains(
            "option w1: The correction is made: go on to verify (by workflow; answers with the text \"The correction is made. Go on to verify.\", then resumes the unit at stage verify; recommended by workflow: The workflow marked this option.)"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "option w2: A flaky failure: run the stage again (by workflow; answers with the text \"Treat the failure as flaky. Run the stage again.\", then resumes the unit at stage implement)"
        ),
        "{text}"
    );
    assert!(text.contains("takes text: yes"), "{text}");

    // A seat's option.
    let s = &w.api.s;
    s.item("u2", 2, 2);
    s.tick();
    std::fs::create_dir_all(s.out("u2", "")).unwrap();
    std::fs::write(s.out("u2", "stop"), STOP).unwrap();
    s.live.ports.fake.finish(&s.session_of("u2"));
    s.tick();
    let stop = s
        .requests(RequestKind::Stop)
        .into_iter()
        .find(|r| r.status == RequestStatus::Open)
        .expect("a stop request")
        .id;
    let text = shown(&stop);
    assert!(
        text.contains(
            "option s1: build on main (by seat; answers with the text \"build on main\")"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "option s2: build on the spine branch (by seat; answers with the text \"build on the spine branch\"; recommended by seat: The seat marked this option.)"
        ),
        "{text}"
    );

    // The controller's `resume`.
    s.item("u3", 3, 3);
    s.tick();
    std::fs::create_dir_all(s.out("u3", "")).unwrap();
    std::fs::write(s.out("u3", "plain.txt"), "plain\n").unwrap();
    s.tick();
    let plain = s
        .requests(RequestKind::Park)
        .into_iter()
        .find(|r| r.status == RequestStatus::Open && r.unit_id.as_deref() == Some("u3"))
        .expect("a park request")
        .id;
    let text = shown(&plain);
    assert!(
        text.contains("option resume: Resume (by controller; resumes the unit)"),
        "{text}"
    );
}
