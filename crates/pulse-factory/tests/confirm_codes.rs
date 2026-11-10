//! AC-2 of `r1.s4.w1`: the confirm code and the claim of a paired device. A device asks for a code for
//! one request and one verb (issuing changes no claim), then spends it once on `answer`, `clear` or
//! `ack`: the spend moves the claim to the device and acts in one admission. The first device to act
//! wins. Every refusal is one word of a closed list and one refused admission line.
//!
//! The controller runs in the test process on a `FakeAdapter`, with the API not listening. Two devices
//! are paired in process (`device_pair` through `handle_from`, then `ApiBridge::redeem`), and the
//! device verbs are called through `ApiBridge::call`, as the router calls them.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::chat::{controller_ledger, ledger_kind};
use common::ctl::Live;
use common::*;
use pf_api::ApiHost;
use pf_api::code::PairingCode;
use pf_api::types::DeviceRecord;
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{AdmissionOutcome, CONTROLLER_SCOPE, ClaimRow, RequestStatus, Store};
use pf_ipc::{Handler, Peer, Request, Response};
use pulse_factory::controller::Config;
use pulse_factory::controller::api::ApiBridge;
use serde_json::{Value, json};

const PEER: Peer = Peer { pid: None };

fn me() -> Peer {
    Peer {
        pid: Some(std::process::id()),
    }
}

struct Rig {
    live: Live,
    bridge: Arc<ApiBridge>,
    d1: DeviceRecord,
    d2: DeviceRecord,
    /// The open request of `u1`, and of `u2`: `(id, revision)`.
    r1: (String, u32),
    r2: (String, u32),
    counter: std::cell::Cell<u32>,
}

fn pair(live: &Live, bridge: &ApiBridge, name: &str) -> DeviceRecord {
    let reply = live
        .controller
        .handle_from(Request::new("device_pair", json!({})), me());
    assert!(reply.ok, "{:?}", reply.error);
    let code = PairingCode::decode(reply.result["code"].as_str().unwrap()).unwrap();
    bridge
        .redeem(&code.secret_hex(), name, "127.0.0.1".parse().unwrap())
        .unwrap_or_else(|e| panic!("pairing {name}: {e:?}"))
        .device
}

fn open_request_of(live: &Live, unit: &str) -> (String, u32) {
    let view = live.handle().engine.view().unwrap();
    let r = view
        .requests
        .iter()
        .find(|r| r.status == RequestStatus::Open && r.unit_id.as_deref() == Some(unit))
        .unwrap_or_else(|| panic!("no open request for {unit}"));
    (r.id.clone(), r.revision)
}

fn rig() -> Rig {
    rig_with(|_| {})
}

fn rig_with(tweak: impl FnOnce(&mut Config) + Send + 'static) -> Rig {
    let live = Live::start_config(BIN.into(), move |cfg| {
        cfg.api_listen = Some("127.0.0.1:0".to_string());
        tweak(cfg);
    });
    rig_on(live)
}

fn rig_on(live: Live) -> Rig {
    live.arm_with_units(&["u1", "u2"]);
    live.park_unit("u1", "the reviewer found a defect");
    live.park_unit("u2", "the build is red");
    let bridge = Arc::new(ApiBridge::new(live.controller.clone()));
    let d1 = pair(&live, &bridge, "MacBook");
    let d2 = pair(&live, &bridge, "iPad");
    assert_eq!((d1.id.as_str(), d2.id.as_str()), ("D-1", "D-2"));
    let (r1, r2) = (open_request_of(&live, "u1"), open_request_of(&live, "u2"));
    Rig {
        live,
        bridge,
        d1,
        d2,
        r1,
        r2,
        counter: std::cell::Cell::new(0),
    }
}

fn sha(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

fn answer_subject(request: &(String, u32), text: &str) -> Value {
    json!({ "kind": "request", "request": request.0, "revision": request.1, "verb": "answer", "text_sha256": sha(text) })
}

fn clear_subject(request: &(String, u32), to: Option<&str>) -> Value {
    let mut v =
        json!({ "kind": "request", "request": request.0, "revision": request.1, "verb": "clear" });
    if let Some(to) = to {
        v["to"] = json!(to);
    }
    v
}

/// The word a refused answer carries.
fn refusal(r: &Response) -> Option<&str> {
    r.result.get("refusal").and_then(Value::as_str)
}

impl Rig {
    fn cmd(&self) -> String {
        self.counter.set(self.counter.get() + 1);
        format!("cmd-{}", self.counter.get())
    }

    fn call(&self, device: &DeviceRecord, verb: &str, args: Value) -> Response {
        self.bridge.call(device, verb, args)
    }

    /// Ask for a code. The answer must be ok.
    fn issue(&self, device: &DeviceRecord, subject: Value) -> Response {
        let r = self.call(
            device,
            "confirm_code",
            json!({ "subject": subject, "command_id": self.cmd() }),
        );
        assert!(r.ok, "confirm_code refused: {:?}", r.error);
        r
    }

    fn code_of(r: &Response) -> String {
        r.result["code"].as_str().unwrap().to_string()
    }

    fn answer_with(
        &self,
        device: &DeviceRecord,
        request: &(String, u32),
        text: &str,
        code: &str,
    ) -> Response {
        self.answer_cmd(device, request, text, code, &self.cmd())
    }

    fn answer_cmd(
        &self,
        device: &DeviceRecord,
        request: &(String, u32),
        text: &str,
        code: &str,
        cmd: &str,
    ) -> Response {
        self.call(
            device,
            "answer",
            json!({ "request": request.0, "rev": request.1, "text": text, "code": code, "command_id": cmd }),
        )
    }

    fn clear_with(
        &self,
        device: &DeviceRecord,
        request: &(String, u32),
        to: Option<&str>,
        code: &str,
    ) -> Response {
        self.call(
            device,
            "clear",
            json!({ "request": request.0, "rev": request.1, "to": to, "code": code, "command_id": self.cmd() }),
        )
    }

    fn claims(&self) -> Vec<ClaimRow> {
        self.live.controller.store().claims().unwrap()
    }

    fn current(&self) -> Option<ClaimRow> {
        self.claims().into_iter().find(|c| c.fenced_at.is_none())
    }

    fn cli_claim(&self, holder: &str) -> u64 {
        let out = self.live.cli(&["claim", "--holder", holder]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        self.current().unwrap().generation
    }

    fn lines(&self, kind: &str) -> Vec<String> {
        ledger_kind(&self.live, kind)
    }

    fn status_of(&self, request: &str) -> RequestStatus {
        self.live
            .controller
            .store()
            .request(request)
            .unwrap()
            .unwrap()
            .status
    }
}

/// Everything the controller wrote that a code, a token or an answer text must stay out of.
fn secrets_stay_out(rig: &Rig, secrets: &[&str], text: &[&str]) {
    let ledger = controller_ledger(&rig.live).join("\n");
    let admissions: String = rig
        .live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .iter()
        .map(|a| format!("{} {} {:?}", a.kind, a.content, a.reason))
        .collect::<Vec<_>>()
        .join("\n");
    let requests = rig.live.cli(&["requests", "--all", "--json"]);
    let show = rig.live.cli(&["show", &rig.r1.0, "--json"]);
    let run_ledger = rig.live.sb.read_ledger();
    for s in secrets {
        for (what, hay) in [
            ("controller.ledger", &ledger),
            ("the admissions", &admissions),
            ("the run ledger", &run_ledger),
            ("requests --json", &stdout(&requests)),
            ("show --json", &stdout(&show)),
        ] {
            assert!(!hay.contains(s), "{what} holds a secret: {s}");
        }
    }
    for t in text {
        assert!(
            !ledger.contains(t),
            "controller.ledger holds the answer text {t:?}"
        );
        assert!(
            !admissions.contains(t),
            "an admission holds the answer text {t:?}"
        );
    }
}

// ------------------------------------------------------------------ issue and spend

#[test]
fn issuing_moves_nothing_and_the_spend_moves_the_claim_and_acts() {
    let rig = rig();
    let g1 = rig.cli_claim("ann@host");
    assert_eq!(g1, 1);
    let token = std::fs::read_to_string(rig.live.sb.state.join("claim-token")).unwrap();
    let token = token.trim().to_string();
    let claims_before = rig.claims();

    // The issue.
    let issued = rig.issue(&rig.d1, answer_subject(&rig.r1, "go on"));
    let r = &issued.result;
    let code = Rig::code_of(&issued);
    assert!(code.starts_with("pfc1-"), "{code}");
    assert_eq!(code.len(), 5 + 32, "{code}");
    assert!(
        code[5..]
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        "{code}"
    );
    assert_eq!(r["replayed"], false);
    assert!(r["expires_at"].is_string(), "{r}");
    assert_eq!(r["generation"], 1);
    assert_eq!(r["device"], "D-1");
    assert_eq!(
        r["holder"]["holder"], "ann@host",
        "the dialog names the holder it takes from: {r}"
    );
    assert_eq!(r["subject"]["request"], rig.r1.0);
    assert_eq!(
        rig.claims(),
        claims_before,
        "issuing a code changes no claim"
    );
    let lines = rig.lines("confirm_code");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(&format!(
            "D-1 request={}@1 verb=answer generation=1",
            rig.r1.0
        )),
        "{}",
        lines[0]
    );

    // The spend moves the claim and answers under the new generation.
    let spent = rig.answer_cmd(&rig.d1, &rig.r1, "go on", &code, "spend-1");
    assert!(spent.ok, "{:?}", spent.error);
    assert_eq!(spent.result["generation"], 2);
    assert_eq!(spent.result["replayed"], false);
    assert_eq!(spent.result["verb"], "answer");
    let claims = rig.claims();
    assert_eq!(claims.len(), 2, "{claims:?}");
    assert!(
        claims[0].fenced_at.is_some(),
        "the CLI's claim is fenced: {claims:?}"
    );
    let now = rig.current().unwrap();
    assert_eq!(now.generation, 2);
    assert!(now.holder.starts_with("device D-1 ("), "{}", now.holder);
    assert_eq!(rig.status_of(&rig.r1.0), RequestStatus::Answered);
    assert_eq!(rig.live.unit_status("u1"), "running", "the unit resumed");
    let admitted = rig.lines("device_answer");
    assert_eq!(admitted.len(), 1, "{admitted:?}");
    assert!(
        admitted[0].contains(&format!("D-1 {}@1 generation=2 from=1 admitted", rig.r1.0)),
        "{}",
        admitted[0]
    );
    let run_ledger = rig.live.sb.read_ledger();
    assert!(
        run_ledger.contains(&format!("answer {}@1 by claim 2", rig.r1.0)),
        "{run_ledger}"
    );

    // Only now is the CLI's answer refused, and it names the device.
    let out = rig
        .live
        .cli(&["answer", &rig.r2.0, "--rev", "1", "--text", "x"]);
    assert_eq!(code_of(&out), 1);
    assert!(
        stderr(&out).contains("the claim moved to device D-1"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains(&token));
    assert_eq!(rig.status_of(&rig.r2.0), RequestStatus::Open);

    // The same command ID with the same content is a replay: nothing runs again.
    let again = rig.answer_cmd(&rig.d1, &rig.r1, "go on", &code, "spend-1");
    assert!(again.ok, "{:?}", again.error);
    assert_eq!(again.result["replayed"], true);
    assert_eq!(rig.lines("device_answer").len(), 1, "one admission");
    assert_eq!(
        rig.live
            .sb
            .read_ledger()
            .matches(&format!("answer {}@1 by claim 2", rig.r1.0))
            .count(),
        1,
        "the engine ran once"
    );
    assert_eq!(rig.claims().len(), 2);

    // A new command ID with the same code: the code is used.
    let reused = rig.answer_with(&rig.d1, &rig.r1, "go on", &code);
    assert!(!reused.ok);
    assert_eq!(refusal(&reused), Some("code_used"), "{reused:?}");
    assert!(!reused.error.as_deref().unwrap().contains(&code));
    let refused = rig.lines("device_answer refused");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("code_used"), "{}", refused[0]);

    // A second code from the device that holds the claim keeps its generation.
    let second = rig.issue(&rig.d1, answer_subject(&rig.r2, "fix it"));
    assert_eq!(second.result["generation"], 2);
    let code2 = Rig::code_of(&second);
    let spent = rig.answer_cmd(&rig.d1, &rig.r2, "fix it", &code2, "spend-2");
    assert!(spent.ok, "{:?}", spent.error);
    assert_eq!(spent.result["generation"], 2);
    assert_eq!(rig.claims().len(), 2, "no new claim row");
    assert!(
        rig.lines("device_answer")
            .iter()
            .any(|l| l.contains("generation=2 from=held admitted")),
        "{:?}",
        rig.lines("device_answer")
    );

    secrets_stay_out(&rig, &[&code, &code2, &token], &["go on", "fix it"]);
}

fn code_of(out: &std::process::Output) -> i32 {
    common::code(out)
}

#[test]
fn with_no_claim_a_code_binds_none_and_its_spend_makes_generation_one() {
    let rig = rig();
    assert!(rig.claims().is_empty());
    let issued = rig.issue(&rig.d1, answer_subject(&rig.r1, "go on"));
    assert!(issued.result["generation"].is_null(), "{}", issued.result);
    assert!(issued.result["holder"].is_null());
    assert!(rig.claims().is_empty(), "the issue made no claim");
    assert!(
        rig.lines("confirm_code")[0].contains("generation=none"),
        "{:?}",
        rig.lines("confirm_code")
    );
    let spent = rig.answer_with(&rig.d1, &rig.r1, "go on", &Rig::code_of(&issued));
    assert!(spent.ok, "{:?}", spent.error);
    assert_eq!(spent.result["generation"], 1);
    assert_eq!(rig.claims().len(), 1);
    assert!(rig.current().unwrap().holder.starts_with("device D-1 ("));
    assert!(
        rig.lines("device_answer")[0].contains("generation=1 from=none admitted"),
        "{:?}",
        rig.lines("device_answer")
    );
}

#[test]
fn the_first_spender_wins_and_an_abandoned_dialog_moves_nothing() {
    let rig = rig();
    let g = rig.cli_claim("ann@host");
    let c1 = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "one")));
    let c2 = Rig::code_of(&rig.issue(&rig.d2, answer_subject(&rig.r2, "two")));
    assert_eq!(rig.claims().len(), 1, "neither issue changed the claim");
    assert_eq!(rig.current().unwrap().generation, g);

    // D-2 spends first.
    let spent = rig.answer_with(&rig.d2, &rig.r2, "two", &c2);
    assert!(spent.ok, "{:?}", spent.error);
    assert_eq!(spent.result["generation"], g + 1);
    assert!(
        rig.lines("device_answer")[0].contains(&format!("generation={} from={g}", g + 1)),
        "{:?}",
        rig.lines("device_answer")
    );
    // D-1's code now meets a claim that moved to someone else.
    let late = rig.answer_with(&rig.d1, &rig.r1, "one", &c1);
    assert!(!late.ok);
    assert_eq!(refusal(&late), Some("claim_moved"), "{late:?}");
    assert!(
        late.error.as_deref().unwrap().contains("device D-2"),
        "{late:?}"
    );
    assert_eq!(
        rig.status_of(&rig.r1.0),
        RequestStatus::Open,
        "nothing was answered"
    );
    assert_eq!(rig.claims().len(), 2, "the refused spend moved nothing");
    assert_eq!(rig.current().unwrap().generation, g + 1);
    // The refused spend spent the code.
    let again = rig.answer_with(&rig.d1, &rig.r1, "one", &c1);
    assert_eq!(refusal(&again), Some("code_used"));
}

#[test]
fn a_cli_claim_between_the_issue_and_the_spend_makes_the_spend_claim_moved() {
    let rig = rig();
    rig.cli_claim("ann@host");
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    let g = rig.cli_claim("bob@host");
    let spent = rig.answer_with(&rig.d1, &rig.r1, "go on", &c);
    assert!(!spent.ok);
    assert_eq!(refusal(&spent), Some("claim_moved"), "{spent:?}");
    assert!(
        spent.error.as_deref().unwrap().contains("bob@host"),
        "{spent:?}"
    );
    assert_eq!(
        rig.current().unwrap().generation,
        g,
        "the claim stays with the CLI"
    );
    assert_eq!(rig.current().unwrap().holder, "bob@host");
    assert_eq!(rig.status_of(&rig.r1.0), RequestStatus::Open);
}

#[test]
fn a_code_that_is_never_spent_expires_and_leaves_the_claim_as_it_was() {
    let rig = rig_with(|cfg| cfg.confirm_ttl = Duration::from_secs(1));
    rig.cli_claim("ann@host");
    let claims = rig.claims();
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    std::thread::sleep(Duration::from_millis(1_300));
    assert_eq!(rig.claims(), claims, "an abandoned dialog moved nothing");
    let spent = rig.answer_with(&rig.d1, &rig.r1, "go on", &c);
    assert!(!spent.ok);
    assert_eq!(refusal(&spent), Some("code_expired"), "{spent:?}");
    assert_eq!(rig.claims(), claims);
    assert_eq!(rig.status_of(&rig.r1.0), RequestStatus::Open);
}

#[test]
fn another_devices_code_is_foreign_and_spent() {
    let rig = rig();
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    let foreign = rig.answer_with(&rig.d2, &rig.r1, "go on", &c);
    assert_eq!(refusal(&foreign), Some("code_foreign"), "{foreign:?}");
    let owner = rig.answer_with(&rig.d1, &rig.r1, "go on", &c);
    assert_eq!(refusal(&owner), Some("code_used"), "{owner:?}");
    assert_eq!(rig.status_of(&rig.r1.0), RequestStatus::Open);
    assert!(rig.claims().is_empty());
}

#[test]
fn a_code_binds_its_request_its_verb_its_revision_and_its_parameters() {
    let rig = rig();
    // Another verb: an answer code used on a clear.
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r2, "go on")));
    let wrong = rig.clear_with(&rig.d1, &rig.r2, None, &c);
    assert_eq!(refusal(&wrong), Some("code_subject"), "{wrong:?}");
    // Another request.
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    let wrong = rig.answer_with(&rig.d1, &rig.r2, "go on", &c);
    assert_eq!(refusal(&wrong), Some("code_subject"), "{wrong:?}");
    // Other answer text.
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    let wrong = rig.answer_with(&rig.d1, &rig.r1, "go on, merge too", &c);
    assert_eq!(refusal(&wrong), Some("code_subject"), "{wrong:?}");
    // A clear code issued with no `to`, spent with one; and the other way round.
    let c = Rig::code_of(&rig.issue(&rig.d1, clear_subject(&rig.r1, None)));
    let wrong = rig.clear_with(&rig.d1, &rig.r1, Some("verify"), &c);
    assert_eq!(refusal(&wrong), Some("code_subject"), "{wrong:?}");
    let c = Rig::code_of(&rig.issue(&rig.d1, clear_subject(&rig.r1, Some("verify"))));
    let wrong = rig.clear_with(&rig.d1, &rig.r1, None, &c);
    assert_eq!(refusal(&wrong), Some("code_subject"), "{wrong:?}");
    assert_eq!(rig.status_of(&rig.r1.0), RequestStatus::Open);
    assert_eq!(rig.status_of(&rig.r2.0), RequestStatus::Open);
    assert!(rig.claims().is_empty(), "a refused spend moves no claim");

    // A request revised after the issue.
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r2, "go on")));
    let row = rig
        .live
        .controller
        .store()
        .request(&rig.r2.0)
        .unwrap()
        .unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!("the facts changed");
    rig.live
        .handle()
        .engine
        .revise_request(&rig.r2.0, 1, &body)
        .unwrap();
    let moved = rig.answer_with(&rig.d1, &rig.r2, "go on", &c);
    assert_eq!(refusal(&moved), Some("subject_moved"), "{moved:?}");
    assert!(
        moved.error.as_deref().unwrap().contains('2'),
        "names the revision: {moved:?}"
    );
    assert!(rig.claims().is_empty());
    // And an issue for the old revision is refused the same way.
    let stale = rig.call(
        &rig.d1,
        "confirm_code",
        json!({ "subject": answer_subject(&rig.r2, "go on"), "command_id": rig.cmd() }),
    );
    assert!(!stale.ok);
    assert_eq!(refusal(&stale), Some("subject_moved"), "{stale:?}");
}

#[test]
fn the_parameters_of_a_code_are_checked_at_issue_and_the_text_is_never_kept() {
    let rig = rig();
    let bad = [
        // An answer with no hash, and one that is not 64 lower-case hex digits.
        json!({ "kind": "request", "request": rig.r1.0, "revision": 1, "verb": "answer" }),
        json!({ "kind": "request", "request": rig.r1.0, "revision": 1, "verb": "answer", "text_sha256": "abc" }),
        json!({ "kind": "request", "request": rig.r1.0, "revision": 1, "verb": "answer", "text_sha256": sha("x").to_uppercase() }),
        // A clear to a stage the track does not have.
        clear_subject(&rig.r1, Some("nowhere")),
        // An unknown request, and a verb the request does not take.
        json!({ "kind": "request", "request": "R-99", "revision": 1, "verb": "ack" }),
        json!({ "kind": "request", "request": rig.r1.0, "revision": 1, "verb": "ack" }),
    ];
    for subject in bad {
        let r = rig.call(
            &rig.d1,
            "confirm_code",
            json!({ "subject": subject, "command_id": rig.cmd() }),
        );
        assert!(!r.ok, "{subject}");
        assert_eq!(refusal(&r), None, "a plain refusal names no word: {r:?}");
    }
    // A good clear code, with a stage of the track.
    let ok = rig.issue(&rig.d1, clear_subject(&rig.r1, Some("verify")));
    let c = Rig::code_of(&ok);
    let cleared = rig.clear_with(&rig.d1, &rig.r1, Some("verify"), &c);
    assert!(cleared.ok, "{:?}", cleared.error);
    assert_eq!(cleared.result["to_stage"], "verify");
    assert_eq!(rig.status_of(&rig.r1.0), RequestStatus::Answered);
    // The hash is in the admission; the text never is.
    let c2 = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r2, "secret ruling text")));
    let spent = rig.answer_with(&rig.d1, &rig.r2, "secret ruling text", &c2);
    assert!(spent.ok, "{:?}", spent.error);
    let admissions = rig
        .live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap();
    let device_answer = admissions
        .iter()
        .find(|a| a.kind == "device_answer")
        .unwrap();
    assert!(
        device_answer.content.contains(&sha("secret ruling text"))
            || device_answer.content.contains("code_sha256"),
        "{}",
        device_answer.content
    );
    secrets_stay_out(&rig, &[&c, &c2], &["secret ruling text"]);
}

// ------------------------------------------------------------------ guessing, tombstones, revoke

#[test]
fn a_made_up_code_is_unknown_and_voids_the_devices_other_live_codes() {
    let rig = rig();
    let live_code = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    let made_up = format!("pfc1-{}", "ab".repeat(16));
    let unknown = rig.answer_with(&rig.d1, &rig.r1, "go on", &made_up);
    assert_eq!(refusal(&unknown), Some("code_unknown"), "{unknown:?}");
    assert!(!unknown.error.as_deref().unwrap().contains(&made_up));
    let after = rig.answer_with(&rig.d1, &rig.r1, "go on", &live_code);
    assert_eq!(refusal(&after), Some("code_voided"), "{after:?}");
    // Another device's codes are not touched by D-1's guess.
    let other = Rig::code_of(&rig.issue(&rig.d2, answer_subject(&rig.r2, "two")));
    let _ = rig.answer_with(&rig.d1, &rig.r1, "go on", &made_up);
    let spent = rig.answer_with(&rig.d2, &rig.r2, "two", &other);
    assert!(spent.ok, "{:?}", spent.error);
}

#[test]
fn a_fifth_issue_voids_the_oldest_and_a_voided_or_expired_code_voids_nothing_else() {
    let rig = rig();
    let codes: Vec<String> = (0..5)
        .map(|i| Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, &format!("t{i}")))))
        .collect();
    // The oldest was voided by the cap.
    let voided = rig.answer_with(&rig.d1, &rig.r1, "t0", &codes[0]);
    assert_eq!(refusal(&voided), Some("code_voided"), "{voided:?}");
    // The other live codes still work after it.
    let spent = rig.answer_with(&rig.d1, &rig.r1, "t4", &codes[4]);
    assert!(spent.ok, "{:?}", spent.error);
    // With a short tombstone window, the voided code is dropped and reads as unknown.
    let short = rig_with(|cfg| {
        cfg.confirm_ttl = Duration::from_secs(1);
        cfg.confirm_tombstone = Duration::from_secs(1);
    });
    let old: Vec<String> = (0..5)
        .map(|i| Rig::code_of(&short.issue(&short.d1, answer_subject(&short.r1, &format!("t{i}")))))
        .collect();
    std::thread::sleep(Duration::from_millis(2_400));
    let gone = short.answer_with(&short.d1, &short.r1, "t0", &old[0]);
    assert_eq!(refusal(&gone), Some("code_unknown"), "{gone:?}");
}

#[test]
fn an_expired_code_is_expired_and_voids_no_other_live_code() {
    let rig = rig_with(|cfg| cfg.confirm_ttl = Duration::from_secs(1));
    let old = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "old")));
    std::thread::sleep(Duration::from_millis(1_300));
    let fresh = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "fresh")));
    let expired = rig.answer_with(&rig.d1, &rig.r1, "old", &old);
    assert_eq!(refusal(&expired), Some("code_expired"), "{expired:?}");
    let spent = rig.answer_with(&rig.d1, &rig.r1, "fresh", &fresh);
    assert!(spent.ok, "{:?}", spent.error);
}

#[test]
fn a_revoke_voids_the_devices_codes_and_a_new_device_cannot_use_them() {
    let rig = rig();
    let held = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    let out = rig.live.cli(&["device", "revoke", "D-1"]);
    assert_eq!(common::code(&out), 0, "{}", stderr(&out));
    let after = rig.answer_with(&rig.d1, &rig.r1, "go on", &held);
    assert!(!after.ok);
    assert_eq!(refusal(&after), Some("device_revoked"), "{after:?}");
    assert_eq!(
        rig.status_of(&rig.r1.0),
        RequestStatus::Open,
        "nothing acted"
    );
    assert!(rig.claims().is_empty());
    let issue = rig.call(
        &rig.d1,
        "confirm_code",
        json!({ "subject": answer_subject(&rig.r1, "x"), "command_id": rig.cmd() }),
    );
    assert_eq!(refusal(&issue), Some("device_revoked"), "{issue:?}");
    // The same name pairs again as a new device: the old code is another device's.
    let d3 = pair(&rig.live, &rig.bridge, "MacBook");
    assert_eq!(d3.id, "D-3");
    let foreign = rig.answer_with(&d3, &rig.r1, "go on", &held);
    assert_eq!(refusal(&foreign), Some("code_foreign"), "{foreign:?}");
}

#[test]
fn an_act_past_take_when_a_revoke_commits_completes_and_the_next_call_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    // The hook waits for the file `$1`, so the answer is held inside the engine call.
    std::fs::write(
        scripts.join("hold.sh"),
        "#!/bin/sh\ntouch \"$1.started\"\ni=0\nwhile [ ! -e \"$1\" ] && [ $i -lt 400 ]; do sleep 0.05; i=$((i+1)); done\n",
    )
    .unwrap();
    let release = dir.path().join("release");
    let o = Options {
        implement_stop: format!(
            "        stop_file:\n          file: \"{{run.dir}}/out/{{unit.id}}/stop\"\n          on_answer:\n            - [sh, \"{{run.package}}/scripts/hold.sh\", \"{}\"]\n",
            release.display()
        ),
        scripts_from: Some(scripts),
        ..Options::default()
    };
    let live = Live::start_package(BIN.into(), &o, |cfg| {
        cfg.api_listen = Some("127.0.0.1:0".to_string());
    });
    live.arm_with_units(&["u1"]);
    let stop = live.run_dir().join("out/u1/stop");
    std::fs::create_dir_all(stop.parent().unwrap()).unwrap();
    std::fs::write(&stop, "which branch?").unwrap();
    live.controller.tick_all();
    let request = open_request_of(&live, "u1");
    let bridge = Arc::new(ApiBridge::new(live.controller.clone()));
    let d1 = pair(&live, &bridge, "MacBook");
    let issued = bridge.call(
        &d1,
        "confirm_code",
        json!({ "subject": answer_subject(&request, "main"), "command_id": "c-issue" }),
    );
    assert!(issued.ok, "{:?}", issued.error);
    let code = issued.result["code"].as_str().unwrap().to_string();
    let (b, d, r) = (bridge.clone(), d1.clone(), request.clone());
    let worker = std::thread::spawn(move || {
        b.call(
            &d,
            "answer",
            json!({ "request": r.0, "rev": r.1, "text": "main", "code": code, "command_id": "c-answer" }),
        )
    });
    // The hook has started: the act is past `take`.
    let started = PathBuf::from(format!("{}.started", release.display()));
    let end = std::time::Instant::now() + Duration::from_secs(20);
    while !started.exists() {
        assert!(std::time::Instant::now() < end, "the hook never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = live.cli(&["device", "revoke", "D-1"]);
    assert_eq!(common::code(&out), 0, "{}", stderr(&out));
    std::fs::write(&release, "").unwrap();
    let done = worker.join().unwrap();
    assert!(
        done.ok,
        "an act already past take completes: {:?}",
        done.error
    );
    assert_eq!(
        live.controller
            .store()
            .request(&request.0)
            .unwrap()
            .unwrap()
            .status,
        RequestStatus::Answered
    );
    let next = bridge.call(
        &d1,
        "confirm_code",
        json!({ "subject": answer_subject(&request, "x"), "command_id": "c-next" }),
    );
    assert_eq!(refusal(&next), Some("device_revoked"), "{next:?}");
}

// ------------------------------------------------------------------ the socket and the rest

#[test]
fn the_socket_refuses_a_code_and_a_cli_label_cannot_pose_as_a_device() {
    let rig = rig();
    let issue = rig.live.controller.handle_from(
        Request::new(
            "confirm_code",
            json!({ "subject": answer_subject(&rig.r1, "x"), "command_id": "s1" }),
        ),
        me(),
    );
    assert!(!issue.ok);
    assert!(
        issue
            .error
            .as_deref()
            .unwrap()
            .contains("pulse-factory claim"),
        "{issue:?}"
    );
    for peer in [me(), PEER] {
        let answer = rig.live.controller.handle_from(
            Request::new(
                "answer",
                json!({ "request": rig.r1.0, "rev": 1, "text": "x", "code": format!("pfc1-{}", "ab".repeat(16)) }),
            ),
            peer,
        );
        assert!(!answer.ok);
        assert!(
            answer
                .error
                .as_deref()
                .unwrap()
                .contains("pulse-factory claim"),
            "{answer:?}"
        );
    }
    assert!(rig.claims().is_empty());
    let out = rig.live.cli(&["claim", "--holder", "device D-1 (MacBook)"]);
    assert_eq!(common::code(&out), 1);
    assert!(
        stderr(&out).contains("kept for paired devices"),
        "{}",
        stderr(&out)
    );
    assert!(rig.claims().is_empty());
}

#[test]
fn every_refusal_is_one_refused_admission_and_holds_no_code() {
    let rig = rig();
    let c = Rig::code_of(&rig.issue(&rig.d1, answer_subject(&rig.r1, "go on")));
    let foreign = rig.answer_with(&rig.d2, &rig.r1, "go on", &c);
    let wrong = rig.answer_with(&rig.d1, &rig.r1, "go on", &c);
    for r in [&foreign, &wrong] {
        assert!(!r.ok);
        assert!(r.result["refusal"].is_string());
        assert!(!r.error.as_deref().unwrap().contains(&c), "{r:?}");
    }
    let refused: Vec<_> = rig
        .live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "device_answer" && a.outcome == AdmissionOutcome::Refused)
        .collect();
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert_eq!(rig.lines("device_answer refused").len(), 2);
}

#[test]
fn with_no_random_bytes_a_code_is_refused_and_the_claim_does_not_move() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("random");
    std::os::unix::fs::symlink("/dev/urandom", &link).unwrap();
    let rig = {
        let link = link.clone();
        rig_with(move |cfg| cfg.random_device = link)
    };
    rig.cli_claim("ann@host");
    let claims = rig.claims();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(dir.path().join("missing"), &link).unwrap();
    let r = rig.call(
        &rig.d1,
        "confirm_code",
        json!({ "subject": answer_subject(&rig.r1, "go on"), "command_id": rig.cmd() }),
    );
    assert!(!r.ok, "{r:?}");
    assert_eq!(rig.claims(), claims);
    assert!(
        rig.lines("confirm_code").is_empty(),
        "no admission was made"
    );
}

/// A replay answers the FIRST record (spec, Approach 3): the generation the code bound, the expiry the
/// first answer carried, the holder it named and the subject as it was shown, even when the claim and the
/// request have moved since. Only `replayed` and `code` differ.
#[test]
fn a_replay_of_an_issue_answers_the_first_record_whatever_moved_since() {
    let rig = rig();
    let held = rig.cli_claim("ann@host");
    let args = json!({ "subject": answer_subject(&rig.r2, "go on"), "command_id": "issue-first" });
    let first = rig.call(&rig.d1, "confirm_code", args.clone());
    assert!(first.ok, "{:?}", first.error);
    assert_eq!(first.result["generation"], held, "{}", first.result);
    assert!(first.result["expires_at"].is_string(), "{}", first.result);
    // The request is revised and the claim moves to someone else after the issue.
    let row = rig
        .live
        .controller
        .store()
        .request(&rig.r2.0)
        .unwrap()
        .unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!("the facts changed");
    rig.live
        .handle()
        .engine
        .revise_request(&rig.r2.0, 1, &body)
        .unwrap();
    let later = rig.cli_claim("bob@host");
    assert!(later > held);
    let replay = rig.call(&rig.d1, "confirm_code", args);
    assert!(replay.ok, "{:?}", replay.error);
    assert_eq!(replay.result["replayed"], true);
    assert!(replay.result["code"].is_null(), "{}", replay.result);
    let strip = |v: &Value| {
        let mut v = v.clone();
        v["replayed"] = Value::Null;
        v["code"] = Value::Null;
        v
    };
    assert_eq!(
        strip(&replay.result),
        strip(&first.result),
        "a replay is the first answer, less the code"
    );
    assert_eq!(rig.lines("confirm_code").len(), 1);
}

#[test]
fn a_replay_of_an_issue_answers_without_a_code_and_the_first_code_still_works() {
    let rig = rig();
    let args = json!({ "subject": answer_subject(&rig.r1, "go on"), "command_id": "issue-1" });
    let first = rig.call(&rig.d1, "confirm_code", args.clone());
    assert!(first.ok, "{:?}", first.error);
    let code = Rig::code_of(&first);
    let replay = rig.call(&rig.d1, "confirm_code", args);
    assert!(replay.ok, "{:?}", replay.error);
    assert_eq!(replay.result["replayed"], true);
    assert!(replay.result["code"].is_null(), "{}", replay.result);
    assert_eq!(rig.lines("confirm_code").len(), 1);
    let spent = rig.answer_with(&rig.d1, &rig.r1, "go on", &code);
    assert!(spent.ok, "{:?}", spent.error);
    // The same command ID with other content is a conflict, and changes nothing.
    let other = rig.call(
        &rig.d1,
        "confirm_code",
        json!({ "subject": answer_subject(&rig.r2, "other"), "command_id": "issue-1" }),
    );
    assert!(!other.ok);
    assert!(
        other.error.as_deref().unwrap().contains("other arguments"),
        "{other:?}"
    );
    assert_eq!(rig.lines("confirm_code").len(), 1);
}
