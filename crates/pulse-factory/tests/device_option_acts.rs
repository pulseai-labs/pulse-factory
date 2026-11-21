//! AC-7 of `r2.s11.w2`: a paired device presses an option under a confirm code that binds the
//! option's ID. The controller runs in the test process on the spine-close rig (a `FakeAdapter` and
//! a bare forge), with the API not used: the device verbs are called through `ApiBridge::call`, as
//! the router calls them. The text of an answer, a code and a token stay out of both ledgers.

mod common;
#[path = "common/needs_you_rig.rs"]
mod rig;

use std::sync::Arc;

use common::chat::{controller_ledger, ledger_kind};
use common::*;
use pf_api::ApiHost;
use pf_api::code::PairingCode;
use pf_api::types::DeviceRecord;
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{CONTROLLER_SCOPE, RequestStatus, Store};
use pf_ipc::{Handler, Peer, Request, Response};
use pulse_factory::controller::api::ApiBridge;
use serde_json::{Value, json};

const STOP: &str =
    "Which branch?\nOptions:\n- build on main\n- [recommended] build on the spine branch";

fn me() -> Peer {
    Peer {
        pid: Some(std::process::id()),
    }
}

/// A package whose stage `implement` takes a text answer on any park and delivers it to no script.
fn noted() -> Options {
    Options {
        implement_extra: "        on_answer:\n          - [\"true\"]\n".to_string(),
        ..Options::default()
    }
}

struct World {
    api: rig::Api,
    bridge: Arc<ApiBridge>,
    d1: DeviceRecord,
    counter: std::cell::Cell<u32>,
}

fn world(o: Options) -> World {
    let api = rig::api_with(o);
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

    /// Ask for a code for `subject`. Returns the whole reply.
    fn issue(&self, subject: Value) -> Response {
        self.call(
            "confirm_code",
            json!({ "subject": subject, "command_id": self.cmd() }),
        )
    }

    fn code_for(&self, subject: Value) -> String {
        let r = self.issue(subject);
        assert!(r.ok, "confirm_code refused: {:?}", r.error);
        r.result["code"].as_str().unwrap().to_string()
    }

    /// The stop request of `u1`, with the seat's two options.
    fn stop(&self) -> String {
        self.api.open_stop(STOP)
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

fn subject(request: &str, rev: u32, verb: &str, option: Option<&str>) -> Value {
    let mut v = json!({ "kind": "request", "request": request, "revision": rev, "verb": verb });
    if let Some(option) = option {
        v["option"] = json!(option);
    }
    v
}

fn refusal(r: &Response) -> Option<&str> {
    r.result.get("refusal").and_then(Value::as_str)
}

#[test]
fn a_device_takes_a_code_for_an_option_and_spends_it_and_the_claim_moves() {
    let w = world(noted());
    let id = w.stop();
    let claims_before = w.api.s.live.controller.store().claims().unwrap();

    // The issue binds the option and shows its label; it moves no claim.
    let issued = w.issue(subject(&id, 1, "answer", Some("s2")));
    assert!(issued.ok, "{:?}", issued.error);
    assert_eq!(
        issued.result["subject"]["option"], "s2",
        "{}",
        issued.result
    );
    assert_eq!(
        issued.result["subject"]["option_label"],
        "build on the spine branch"
    );
    assert_eq!(
        w.api.s.live.controller.store().claims().unwrap(),
        claims_before
    );
    let code = issued.result["code"].as_str().unwrap().to_string();

    // The spend sends an empty text: the option is the act.
    let spent = w.call(
        "answer",
        json!({ "request": id, "rev": 1, "text": "", "option": "s2", "code": code, "command_id": "spend-1" }),
    );
    assert!(spent.ok, "{:?}", spent.error);
    assert_eq!(spent.result["option"], "s2", "{}", spent.result);
    assert_eq!(spent.result["generation"], 1);
    assert_eq!(w.status_of(&id), RequestStatus::Answered);
    let claims = w.api.s.live.controller.store().claims().unwrap();
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert!(
        claims[0].holder.starts_with("device D-1 ("),
        "{}",
        claims[0].holder
    );
    // The stored answer names the option, who wrote it and the recommendation.
    let shown: Value = serde_json::from_str(
        &w.api
            .s
            .live
            .controller
            .store()
            .request(&id)
            .unwrap()
            .unwrap()
            .answer
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        shown["option"],
        json!({ "id": "s2", "by": "seat", "recommended": true })
    );
    assert_eq!(shown["text"], "build on the spine branch");

    // The ledger line names the device and the option, and holds no code and no text.
    let lines = ledger_kind(&w.api.s.live, "device_answer");
    let admitted: Vec<_> = lines.iter().filter(|l| !l.contains("refused")).collect();
    assert_eq!(admitted.len(), 1, "{lines:?}");
    assert!(
        admitted[0].contains("D-1") && admitted[0].contains(" option=s2"),
        "{}",
        admitted[0]
    );
    let issue_lines = ledger_kind(&w.api.s.live, "confirm_code");
    assert!(
        issue_lines.iter().any(|l| l.contains(" option=s2")),
        "{issue_lines:?}"
    );
    let all = format!(
        "{}\n{}",
        controller_ledger(&w.api.s.live).join("\n"),
        w.api.api_ledger().join("\n")
    );
    for secret in [code.as_str(), "build on the spine branch"] {
        assert!(!all.contains(secret), "a ledger holds {secret:?}");
    }
}

#[test]
fn a_code_for_one_option_spent_with_another_a_text_or_none_is_code_subject() {
    let w = world(noted());
    let id = w.stop();
    let cases: [(&str, Value); 4] = [
        (
            "another option",
            json!({ "request": id, "rev": 1, "text": "", "option": "s1" }),
        ),
        (
            "a text beside the option",
            json!({ "request": id, "rev": 1, "text": "build on the spine branch", "option": "s2" }),
        ),
        (
            "a stage beside the option",
            json!({ "request": id, "rev": 1, "text": "", "option": "s2", "to": "verify" }),
        ),
        (
            "no option",
            json!({ "request": id, "rev": 1, "text": "build on the spine branch" }),
        ),
    ];
    for (what, mut args) in cases {
        let code = w.code_for(subject(&id, 1, "answer", Some("s2")));
        args["code"] = json!(code);
        args["command_id"] = json!(w.cmd());
        let r = w.call("answer", args);
        assert!(!r.ok, "{what}: {r:?}");
        assert_eq!(refusal(&r), Some("code_subject"), "{what}: {r:?}");
        assert!(!r.error.as_deref().unwrap().contains(&code), "{r:?}");
    }
    assert_eq!(w.status_of(&id), RequestStatus::Open);
    assert!(
        w.api.s.live.controller.store().claims().unwrap().is_empty(),
        "a refused spend moves no claim"
    );
}

#[test]
fn a_code_for_an_unknown_option_or_for_an_option_of_another_verb_is_refused_at_issue() {
    let w = world(noted());
    let id = w.stop();
    // An unknown option, an option the verb does not fit (`s1` answers: it is no `clear`), a bad
    // ID, and an option beside a text hash.
    let mut bad_hash = subject(&id, 1, "answer", Some("s1"));
    bad_hash["text_sha256"] = json!(sha256_hex(b"x"));
    for (what, subject) in [
        ("unknown", subject(&id, 1, "answer", Some("zz"))),
        ("wrong verb", subject(&id, 1, "clear", Some("s1"))),
        ("wrong verb, ack", subject(&id, 1, "ack", Some("s1"))),
        ("bad ID", subject(&id, 1, "answer", Some("S 1"))),
        ("an option and a hash", bad_hash),
    ] {
        let r = w.issue(subject);
        assert!(!r.ok, "{what}: {r:?}");
        assert!(
            r.error
                .as_deref()
                .unwrap()
                .starts_with("confirm_code refused"),
            "{what}: {r:?}"
        );
    }
    let wrong_verb = w.issue(subject(&id, 1, "clear", Some("s1")));
    assert!(
        wrong_verb.error.as_deref().unwrap().contains("answer"),
        "the refusal names the right verb: {wrong_verb:?}"
    );
    // No code was made: no admission of an issue, and the request is as it was.
    assert!(
        ledger_kind(&w.api.s.live, "confirm_code").is_empty(),
        "{:?}",
        controller_ledger(&w.api.s.live)
    );
    let issued: Vec<_> = w
        .api
        .s
        .live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "confirm_code")
        .collect();
    assert!(issued.is_empty(), "{issued:?}");
    assert_eq!(w.status_of(&id), RequestStatus::Open);
}

#[test]
fn a_revision_between_the_issue_and_the_spend_is_subject_moved() {
    let w = world(noted());
    let id = w.stop();
    let code = w.code_for(subject(&id, 1, "answer", Some("s1")));
    let store = w.api.s.live.controller.store();
    let row = store.request(&id).unwrap().unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!("the facts changed");
    w.api
        .s
        .live
        .handle()
        .engine
        .revise_request(&id, 1, &body)
        .unwrap();
    // The card was rendered at revision 1: the press names it, and the request is at 2 now.
    let moved = w.call(
        "answer",
        json!({ "request": id, "rev": 1, "text": "", "option": "s1", "code": code, "command_id": w.cmd() }),
    );
    assert!(!moved.ok, "{moved:?}");
    assert_eq!(refusal(&moved), Some("subject_moved"), "{moved:?}");
    assert_eq!(w.status_of(&id), RequestStatus::Open);
    assert!(store.claims().unwrap().is_empty());
}

#[test]
fn an_approve_option_merges_under_a_fresh_code_as_the_ack_does() {
    let w = world(Options::default());
    let (id, head) = w.api.to_merge_ack();
    let s = &w.api.s;
    let main_before = s.head("main");

    // `approve` is the `ack` verb: an `answer` code for it is refused at issue.
    let wrong = w.issue(subject(&id, 1, "answer", Some("approve")));
    assert!(!wrong.ok, "{wrong:?}");
    assert!(wrong.error.as_deref().unwrap().contains("ack"), "{wrong:?}");

    let issued = w.issue(subject(&id, 1, "ack", Some("approve")));
    assert!(issued.ok, "{:?}", issued.error);
    assert_eq!(issued.result["subject"]["option"], "approve");
    assert_eq!(issued.result["subject"]["head_sha"], head.as_str());
    let code = issued.result["code"].as_str().unwrap().to_string();
    assert_eq!(s.head("main"), main_before, "the issue merged nothing");

    let acked = w.call(
        "ack",
        json!({ "request": id, "rev": 1, "option": "approve", "code": code, "command_id": "ack-1" }),
    );
    assert!(acked.ok, "{:?}", acked.error);
    assert_eq!(acked.result["merged"], true, "{}", acked.result);
    assert_eq!(acked.result["option"], "approve");
    assert_eq!(acked.result["head_sha"], head.as_str());
    assert_ne!(s.head("main"), main_before);
    let lines = ledger_kind(&s.live, "device_ack");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("D-1") && l.contains(" option=approve")),
        "{lines:?}"
    );
    // A replay of the same spend is the first record, and merges nothing again.
    let main = s.head("main");
    let again = w.call(
        "ack",
        json!({ "request": id, "rev": 1, "option": "approve", "code": code, "command_id": "ack-1" }),
    );
    assert!(again.ok, "{:?}", again.error);
    assert_eq!(s.head("main"), main);
}

#[test]
fn a_code_for_a_text_on_a_request_that_takes_none_is_refused_at_issue() {
    // The default package has no hook for the stop: a typed answer reaches no seat.
    let w = world(Options::default());
    let id = w.stop();
    let typed = w.issue(json!({
        "kind": "request", "request": id, "revision": 1, "verb": "answer",
        "text_sha256": sha256_hex(b"go on"),
    }));
    assert!(!typed.ok, "{typed:?}");
    let why = typed.error.as_deref().unwrap();
    assert!(why.contains("takes no text"), "{why}");
    assert!(why.contains("pulse-factory clear"), "{why}");
    // An option whose effect is a text is that text: it is refused the same way.
    let option = w.issue(subject(&id, 1, "answer", Some("s1")));
    assert!(!option.ok, "{option:?}");
    assert!(
        option.error.as_deref().unwrap().contains("takes no text"),
        "{option:?}"
    );
    // No code was spent on it, and nothing was admitted.
    assert!(ledger_kind(&w.api.s.live, "confirm_code").is_empty());
    assert_eq!(w.status_of(&id), RequestStatus::Open);
}
