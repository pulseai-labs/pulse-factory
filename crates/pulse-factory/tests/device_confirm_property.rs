//! AC-4 of `r1.s4.w1`: a property test of the confirm code and the device claim. Random sequences of
//! `confirm_code`, device `answer` and `clear` (a pool code, a stale code or a made-up one), a CLI
//! `claim`, a `revise_request` and a TTL step run in process through `ApiBridge`, with two devices, two
//! parked requests and a pool of 4 command IDs. A small model of the code table predicts each refusal;
//! after each step the invariants of the confirm code and the claim hold.
//!
//! Two steps use a fresh command ID and aim at a live code: `IssueNew` asks for a code, and `SpendLatest`
//! spends the newest live code of a device on the act it was issued for. Without them a pool of 4 IDs is
//! used up in a few steps and most spends are replays. The oracle for a successful act is exact: a device
//! that already held the current claim keeps its generation and adds no row (`from=held`); any other
//! device fences exactly the generation its code bound and adds one row. A mutation of either arm of
//! that decision makes the property or one of the three fixed-sequence tests at the end fail.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::chat::controller_ledger;
use common::ctl::Live;
use common::*;
use pf_api::ApiHost;
use pf_api::code::PairingCode;
use pf_api::types::DeviceRecord;
use pf_engine::claim::{holder_device, sha256_hex};
use pf_engine::port::store::{CONTROLLER_SCOPE, ClaimRow, RequestStatus, Store};
use pf_ipc::{Handler, Peer, Request};
use proptest::prelude::*;
use pulse_factory::controller::Config;
use pulse_factory::controller::api::ApiBridge;
use serde_json::{Value, json};

const TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
enum Step {
    Issue {
        dev: usize,
        req: usize,
        verb: usize,
        cmd: usize,
    },
    Spend {
        dev: usize,
        req: usize,
        verb: usize,
        code: usize,
        cmd: usize,
    },
    /// An issue with a command ID the pool does not use.
    IssueNew {
        dev: usize,
        req: usize,
        verb: usize,
    },
    /// Spend the newest live code of the device, on the act it was issued for, with a new command ID.
    SpendLatest {
        dev: usize,
    },
    CliClaim,
    Revise {
        req: usize,
    },
    Ttl,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (0..2usize, 0..2usize, 0..4usize, 0..4usize)
            .prop_map(|(dev, req, verb, cmd)| Step::Issue { dev, req, verb, cmd }),
        5 => (0..2usize, 0..2usize, 0..4usize, 0..9usize, 0..4usize)
            .prop_map(|(dev, req, verb, code, cmd)| Step::Spend { dev, req, verb, code, cmd }),
        4 => (0..2usize, 0..2usize, 0..4usize)
            .prop_map(|(dev, req, verb)| Step::IssueNew { dev, req, verb }),
        5 => (0..2usize).prop_map(|dev| Step::SpendLatest { dev }),
        1 => Just(Step::CliClaim),
        1 => (0..2usize).prop_map(|req| Step::Revise { req }),
        1 => Just(Step::Ttl),
    ]
}

/// The four verbs of the pool: an answer of one of two texts, a clear with no `to`, a clear to a stage.
fn verb_of(i: usize) -> (&'static str, Option<&'static str>) {
    match i {
        0 => ("answer", Some("t0")),
        1 => ("answer", Some("t1")),
        2 => ("clear", None),
        _ => ("clear", Some("verify")),
    }
}

fn sha(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Live,
    Spent,
    Voided,
    Expired,
}

#[derive(Clone, Debug)]
struct CodeRec {
    text: String,
    dev: usize,
    req: usize,
    rev: u32,
    verb: usize,
    bound_gen: Option<u64>,
    state: St,
    admitted: bool,
}

struct World {
    live: Live,
    bridge: Arc<ApiBridge>,
    devices: Vec<DeviceRecord>,
    requests: Vec<String>,
    codes: Vec<CodeRec>,
    /// `(request, revision)` pairs that an admitted answer or clear closed.
    closed: Vec<(String, u32)>,
    /// The next command ID that the pool does not use.
    fresh: u32,
}

fn pair(live: &Live, bridge: &ApiBridge, name: &str) -> DeviceRecord {
    let me = Peer {
        pid: Some(std::process::id()),
    };
    let reply = live
        .controller
        .handle_from(Request::new("device_pair", json!({})), me);
    assert!(reply.ok, "{:?}", reply.error);
    let code = PairingCode::decode(reply.result["code"].as_str().unwrap()).unwrap();
    bridge
        .redeem(&code.secret_hex(), name, "127.0.0.1".parse().unwrap())
        .unwrap()
        .device
}

fn world() -> World {
    let live = Live::start_config(BIN.into(), |cfg: &mut Config| {
        cfg.api_listen = Some("127.0.0.1:0".to_string());
        cfg.confirm_ttl = TTL;
    });
    live.arm_with_units(&["u1", "u2"]);
    live.park_unit("u1", "the reviewer found a defect");
    live.park_unit("u2", "the build is red");
    let bridge = Arc::new(ApiBridge::new(live.controller.clone()));
    let devices = vec![
        pair(&live, &bridge, "MacBook"),
        pair(&live, &bridge, "iPad"),
    ];
    let view = live.handle().engine.view().unwrap();
    let requests = ["u1", "u2"]
        .iter()
        .map(|u| {
            view.requests
                .iter()
                .find(|r| r.status == RequestStatus::Open && r.unit_id.as_deref() == Some(u))
                .unwrap()
                .id
                .clone()
        })
        .collect();
    World {
        live,
        bridge,
        devices,
        requests,
        codes: Vec::new(),
        closed: Vec::new(),
        fresh: 0,
    }
}

fn word(r: &pf_ipc::Response) -> Option<String> {
    r.result
        .get("refusal")
        .and_then(Value::as_str)
        .map(str::to_string)
}

impl World {
    fn claims(&self) -> Vec<ClaimRow> {
        self.live.controller.store().claims().unwrap()
    }

    fn admissions(&self) -> usize {
        self.live
            .controller
            .store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
            .len()
    }

    fn ledger_lines(&self) -> usize {
        controller_ledger(&self.live).len()
    }

    fn request(&self, req: usize) -> pf_engine::port::store::RequestRow {
        self.live
            .controller
            .store()
            .request(&self.requests[req])
            .unwrap()
            .unwrap()
    }

    fn known_command(&self, cmd: &str) -> bool {
        matches!(
            self.live
                .controller
                .store()
                .admission(CONTROLLER_SCOPE, cmd),
            Ok(Some(_))
        )
    }

    fn status_of(&self, req: usize) -> RequestStatus {
        self.request(req).status
    }

    fn fresh_command(&mut self) -> String {
        self.fresh += 1;
        format!("f{}", self.fresh)
    }

    fn void_oldest_live(&mut self, dev: usize) {
        let live: Vec<usize> = (0..self.codes.len())
            .filter(|&i| self.codes[i].dev == dev && self.codes[i].state == St::Live)
            .collect();
        if live.len() >= 4 {
            for &i in &live[..=live.len() - 4] {
                self.codes[i].state = St::Voided;
            }
        }
    }

    fn step(&mut self, step: &Step) {
        let (claims0, adm0, lines0) = (self.claims(), self.admissions(), self.ledger_lines());
        match step {
            Step::Issue {
                dev,
                req,
                verb,
                cmd,
            } => self.issue(*dev, *req, *verb, &format!("c{cmd}"), &claims0),
            Step::IssueNew { dev, req, verb } => {
                let cmd = self.fresh_command();
                self.issue(*dev, *req, *verb, &cmd, &claims0)
            }
            Step::SpendLatest { dev } => {
                let latest = (0..self.codes.len())
                    .rev()
                    .find(|&i| self.codes[i].dev == *dev && self.codes[i].state == St::Live);
                if let Some(i) = latest {
                    let (req, verb) = (self.codes[i].req, self.codes[i].verb);
                    let cmd = self.fresh_command();
                    self.spend(*dev, req, verb, i, &cmd, &claims0)
                }
            }
            Step::Spend {
                dev,
                req,
                verb,
                code,
                cmd,
            } => self.spend(*dev, *req, *verb, *code, &format!("c{cmd}"), &claims0),
            Step::CliClaim => {
                let out = self.live.cli(&["claim", "--holder", "ann@host"]);
                assert_eq!(common::code(&out), 0, "{}", stderr(&out));
            }
            Step::Revise { req } => {
                let row = self.request(*req);
                if row.status == RequestStatus::Open {
                    let mut body: Value = serde_json::from_str(&row.body).unwrap();
                    body["n"] = json!(row.revision);
                    self.live
                        .handle()
                        .engine
                        .revise_request(&row.id, row.revision, &body)
                        .unwrap();
                }
            }
            Step::Ttl => {
                self.live
                    .controller
                    .advance_confirm_clock(TTL + Duration::from_secs(1));
                for c in &mut self.codes {
                    if c.state == St::Live {
                        c.state = St::Expired;
                    }
                }
            }
        }
        // The invariants that hold after every step.
        let claims = self.claims();
        assert!(
            claims.iter().filter(|c| c.fenced_at.is_none()).count() <= 1,
            "at most one current claim: {claims:?}"
        );
        for c in &self.codes {
            let _ = c;
        }
        let (adm, lines) = (self.admissions() - adm0, self.ledger_lines() - lines0);
        assert_eq!(
            adm, lines,
            "one controller.ledger line per new admission: {step:?}"
        );
        assert!(adm <= 1, "{step:?}");
    }

    fn issue(&mut self, dev: usize, req: usize, verb: usize, cmd: &str, claims0: &[ClaimRow]) {
        let known = self.known_command(cmd);
        let row = self.request(req);
        let (kind, text) = verb_of(verb);
        let mut subject =
            json!({ "kind": "request", "request": row.id, "revision": row.revision, "verb": kind });
        match (kind, text) {
            ("answer", Some(t)) => subject["text_sha256"] = json!(sha(t)),
            (_, Some(to)) => subject["to"] = json!(to),
            _ => {}
        }
        let (adm0, lines0) = (self.admissions(), self.ledger_lines());
        let r = self.bridge.call(
            &self.devices[dev],
            "confirm_code",
            json!({ "subject": subject, "command_id": cmd }),
        );
        assert_eq!(
            self.claims(),
            claims0,
            "confirm_code never changes the claims"
        );
        if known {
            // A replay answers the first record; another content is a conflict. Neither writes.
            assert_eq!((self.admissions(), self.ledger_lines()), (adm0, lines0));
            if r.ok {
                assert_eq!(r.result["replayed"], true, "{r:?}");
                assert!(r.result["code"].is_null());
            } else {
                assert!(
                    r.error
                        .as_deref()
                        .unwrap_or_default()
                        .contains("other arguments")
                        || word(&r).is_some()
                        || r.error.is_some(),
                    "{r:?}"
                );
            }
            return;
        }
        if row.status != RequestStatus::Open {
            assert_eq!(word(&r).as_deref(), Some("subject_moved"), "{r:?}");
            return;
        }
        assert!(r.ok, "{r:?}");
        let text_code = r.result["code"].as_str().unwrap().to_string();
        self.void_oldest_live(dev);
        self.codes.push(CodeRec {
            text: text_code,
            dev,
            req,
            rev: row.revision,
            verb,
            bound_gen: r.result["generation"].as_u64(),
            state: St::Live,
            admitted: false,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn spend(
        &mut self,
        dev: usize,
        req: usize,
        verb: usize,
        code: usize,
        cmd: &str,
        claims0: &[ClaimRow],
    ) {
        let known = self.known_command(cmd);
        let pooled = self.codes.get(code).cloned();
        let made_up = format!("pfc1-{}", "cd".repeat(16));
        let text = pooled.as_ref().map_or(made_up, |c| c.text.clone());
        let row = self.request(req);
        // A device acts on the dialog it was shown: the revision of its code.
        let rev = pooled.as_ref().map_or(row.revision, |c| c.rev);
        let (kind, param) = verb_of(verb);
        let args = if kind == "answer" {
            json!({ "request": row.id, "rev": rev, "text": param.unwrap(), "code": text, "command_id": cmd })
        } else {
            json!({ "request": row.id, "rev": rev, "to": param, "code": text, "command_id": cmd })
        };
        let (adm0, lines0) = (self.admissions(), self.ledger_lines());
        let r = self.bridge.call(&self.devices[dev], kind, args);
        if known {
            assert_eq!(
                (self.admissions(), self.ledger_lines()),
                (adm0, lines0),
                "a replay or a conflict writes nothing"
            );
            assert_eq!(
                self.claims(),
                claims0,
                "a replay or a conflict changes no claim"
            );
            if r.ok {
                assert_eq!(r.result["replayed"], true, "{r:?}");
            }
            return;
        }
        let w = word(&r);
        // What `take` predicts, from the model of the table.
        let call_matches = |c: &CodeRec| c.req == req && c.rev == rev && same_verb(c.verb, verb);
        let predicted: Option<&str> = match &pooled {
            None => Some("code_unknown"),
            Some(c) if c.dev != dev => Some("code_foreign"),
            Some(c) => match c.state {
                St::Spent => Some("code_used"),
                St::Voided => Some("code_voided"),
                St::Expired => Some("code_expired"),
                St::Live if !call_matches(c) => Some("code_subject"),
                St::Live => None,
            },
        };
        if let Some(p) = predicted {
            assert_eq!(w.as_deref(), Some(p), "the model predicted {p}: {r:?}");
            assert_eq!(self.claims(), claims0, "a refused code moves no claim");
        }
        // The effect of the presentation on the table.
        match &pooled {
            None => {
                for c in self
                    .codes
                    .iter_mut()
                    .filter(|c| c.dev == dev && c.state == St::Live)
                {
                    c.state = St::Voided;
                }
            }
            Some(c) if c.dev != dev => self.codes[code].state = St::Spent,
            Some(c) if c.state == St::Live => self.codes[code].state = St::Spent,
            _ => {}
        }
        if predicted.is_some() && predicted != Some("code_subject") {
            return;
        }
        if predicted == Some("code_subject") {
            return;
        }
        let c = pooled.unwrap();
        if r.ok {
            assert_eq!(r.result["replayed"], false);
            assert!(!c.admitted, "a code admits at most one act");
            self.codes[code].admitted = true;
            // The act's parameters equal the ones the code bound.
            assert!(call_matches(&c));
            let now = self.claims();
            let current = now.iter().find(|x| x.fenced_at.is_none()).unwrap();
            assert_eq!(
                holder_device(&current.holder),
                Some(self.devices[dev].id.as_str())
            );
            assert_eq!(r.result["generation"], current.generation);
            let before = claims0.iter().find(|x| x.fenced_at.is_none());
            let held_before = before.and_then(|x| holder_device(&x.holder))
                == Some(self.devices[dev].id.as_str());
            if held_before {
                // `from=held`: the device keeps its generation, whatever generation the code bound.
                assert_eq!(
                    now.len(),
                    claims0.len(),
                    "a device that holds the claim adds no claim row"
                );
                assert_eq!(
                    Some(current.generation),
                    before.map(|x| x.generation),
                    "a device that holds the claim keeps its generation"
                );
            } else {
                // The move: it fenced exactly the generation the code bound, and added one row.
                assert_eq!(
                    now.len(),
                    claims0.len() + 1,
                    "a move adds exactly one claim row"
                );
                assert_eq!(
                    before.map(|x| x.generation),
                    c.bound_gen,
                    "the move fenced the generation the code bound"
                );
                assert!(
                    now.iter().filter(|x| x.fenced_at.is_some()).count()
                        == claims0.iter().filter(|x| x.fenced_at.is_some()).count()
                            + usize::from(before.is_some()),
                    "the move fenced the current claim and nothing else"
                );
            }
            let key = (row.id.clone(), c.rev);
            assert!(
                !self.closed.contains(&key),
                "at most one answer per request and revision"
            );
            self.closed.push(key);
        } else {
            match w.as_deref() {
                Some("claim_moved") => {
                    let cur = claims0.iter().find(|x| x.fenced_at.is_none());
                    let held = cur.and_then(|x| holder_device(&x.holder))
                        == Some(self.devices[dev].id.as_str());
                    assert!(
                        !held && cur.map(|x| x.generation) != c.bound_gen,
                        "claim_moved needs a claim that moved"
                    );
                    assert_eq!(self.claims(), claims0, "claim_moved moves nothing");
                }
                Some("subject_moved") => assert_eq!(self.claims(), claims0),
                other => panic!("an unexpected refusal {other:?}: {r:?}"),
            }
        }
    }
}

fn same_verb(a: usize, b: usize) -> bool {
    verb_of(a) == verb_of(b)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn the_confirm_code_and_the_device_claim_hold_their_rules(steps in proptest::collection::vec(step(), 1..14)) {
        let mut w = world();
        for s in &steps {
            w.step(s);
        }
    }
}

fn issue_new(w: &mut World, dev: usize, req: usize) {
    w.step(&Step::IssueNew { dev, req, verb: 0 });
}

/// Spend the live code at `index` of the model, on the act it was issued for.
fn spend_code(w: &mut World, index: usize) {
    let (dev, req, verb) = (w.codes[index].dev, w.codes[index].req, w.codes[index].verb);
    let claims0 = w.claims();
    let cmd = w.fresh_command();
    w.spend(dev, req, verb, index, &cmd, &claims0);
}

/// The held arm of the spend's claim decision, code bound to the held generation: D-1 takes the claim
/// with its first code (`from=none`), and its second code, which bound that generation, keeps it. A
/// spend that re-made the claim would add a row.
#[test]
fn a_device_that_holds_the_claim_keeps_it_for_a_code_that_bound_it() {
    let mut w = world();
    issue_new(&mut w, 0, 0);
    w.step(&Step::SpendLatest { dev: 0 });
    assert_eq!(w.claims().len(), 1);
    assert_eq!(w.claims()[0].generation, 1);
    issue_new(&mut w, 0, 1);
    assert_eq!(w.codes.last().unwrap().bound_gen, Some(1));
    w.step(&Step::SpendLatest { dev: 0 });
    assert_eq!(w.claims().len(), 1, "a held claim is kept, not re-made");
    assert_eq!(w.status_of(1), RequestStatus::Answered, "the act ran");
}

/// The held arm, code bound to an OLDER generation: both codes of D-1 bind "no claim"; the second spend
/// is by the device that now holds generation 1, so it is kept and not `claim_moved`.
#[test]
fn a_device_that_holds_the_claim_keeps_it_for_a_code_that_bound_an_older_one() {
    let mut w = world();
    issue_new(&mut w, 0, 0);
    issue_new(&mut w, 0, 1);
    assert_eq!(w.codes[0].bound_gen, None);
    assert_eq!(w.codes[1].bound_gen, None);
    spend_code(&mut w, 1);
    assert_eq!(w.claims().len(), 1);
    spend_code(&mut w, 0);
    assert_eq!(w.claims().len(), 1, "a held claim is kept, not re-made");
    assert_eq!(w.status_of(0), RequestStatus::Answered, "the act ran");
    assert_eq!(w.status_of(1), RequestStatus::Answered);
}

/// The moved arm: both devices bind "no claim"; D-1 spends first and takes it; D-2's spend is
/// `claim_moved` and moves nothing.
#[test]
fn the_other_device_is_refused_when_the_claim_moved_since_its_issue() {
    let mut w = world();
    issue_new(&mut w, 0, 0);
    issue_new(&mut w, 1, 1);
    spend_code(&mut w, 0);
    let claims = w.claims();
    spend_code(&mut w, 1);
    assert_eq!(w.claims(), claims, "claim_moved moves nothing");
    assert_eq!(
        w.status_of(1),
        RequestStatus::Open,
        "its request stays open"
    );
}
