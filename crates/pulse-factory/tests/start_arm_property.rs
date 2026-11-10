//! AC-8 of `r1.s4.w2`: a property test of the start card's arm. Random sequences of `propose` (from
//! either of two chats), `confirm_code` for the arm subject (the right hash or a stale one), device
//! `arm` (a pool code, a stale code or a made-up one), an edit of the installed workflow, an edit of a
//! snapshot, the operator's CLI `arm` of the same spine inputs, `halt`, the end of a run and a TTL step
//! run in process through `ApiBridge`, with two devices, two sessions and a pool of 4 command IDs for
//! each verb. After each step the invariants of the spec hold: each proposal arms at most one run, each
//! run is one of the two kinds, each code admits at most one act, and no proposal is left `arming`.
//!
//! A fixed concurrent case follows the property: two proposals on the same spine inputs, each
//! confirmed by a device, are armed by two calls at once. Exactly one run exists, and the loser is
//! refused naming the winner's run, with no zombie `arming` proposal. The same holds with the second
//! call replaced by the operator's CLI `arm --yes`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::chat::controller_ledger;
use common::start::*;
use common::*;
use pf_api::ApiHost;
use pf_api::code::PairingCode;
use pf_api::types::DeviceRecord;
use pf_engine::port::store::{AdmissionOutcome, CONTROLLER_SCOPE, Store};
use pf_ipc::{Handler, Peer, Request};
use proptest::prelude::*;
use pulse_factory::controller::Config;
use pulse_factory::controller::api::ApiBridge;
use serde_json::{Value, json};

const TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
enum Step {
    /// `propose` from session `sess` with a command ID of the pool.
    Propose {
        sess: usize,
        cmd: usize,
    },
    /// `propose` with a command ID the pool does not use.
    ProposeNew {
        sess: usize,
    },
    /// `confirm_code` for a proposal, with the right or a stale hash and a command ID of the pool.
    Confirm {
        dev: usize,
        prop: usize,
        stale: bool,
        cmd: usize,
    },
    /// `confirm_code` with a command ID the pool does not use.
    ConfirmNew {
        dev: usize,
        prop: usize,
    },
    /// `arm` with a pool code (or a made-up one) and a command ID of the pool.
    Arm {
        dev: usize,
        prop: usize,
        code: usize,
        cmd: usize,
    },
    /// Spend the newest code of the device for the proposal it was issued for, with a new command ID.
    ArmLatest {
        dev: usize,
    },
    /// The installed workflow changes (a comment line).
    EditShare,
    /// A snapshot changes (a comment line).
    EditSnapshot {
        prop: usize,
    },
    /// The operator's CLI `arm --yes` of the same spine inputs.
    CliArm,
    Halt,
    /// Every active run ends.
    EndRuns,
    Tick,
    Ttl,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (0..2usize, 0..4usize).prop_map(|(sess, cmd)| Step::Propose { sess, cmd }),
        3 => (0..2usize).prop_map(|sess| Step::ProposeNew { sess }),
        3 => (0..2usize, 0..4usize, any::<bool>(), 0..4usize)
            .prop_map(|(dev, prop, stale, cmd)| Step::Confirm { dev, prop, stale, cmd }),
        4 => (0..2usize, 0..4usize).prop_map(|(dev, prop)| Step::ConfirmNew { dev, prop }),
        4 => (0..2usize, 0..4usize, 0..9usize, 0..4usize)
            .prop_map(|(dev, prop, code, cmd)| Step::Arm { dev, prop, code, cmd }),
        5 => (0..2usize).prop_map(|dev| Step::ArmLatest { dev }),
        1 => Just(Step::EditShare),
        1 => (0..4usize).prop_map(|prop| Step::EditSnapshot { prop }),
        1 => Just(Step::CliArm),
        1 => Just(Step::Halt),
        2 => Just(Step::EndRuns),
        1 => Just(Step::Tick),
        1 => Just(Step::Ttl),
    ]
}

#[derive(Clone, Debug)]
struct Prop {
    id: String,
    hash: String,
    session: String,
    project: String,
}

#[derive(Clone, Debug)]
struct CodeRec {
    text: String,
    dev: usize,
    prop: usize,
}

struct World {
    s: Start,
    bridge: Arc<ApiBridge>,
    devices: Vec<DeviceRecord>,
    shells: Vec<ChatShell>,
    proposals: Vec<Prop>,
    codes: Vec<CodeRec>,
    /// How many arms succeeded and were not replays, for each code text.
    spent: BTreeMap<String, u32>,
    fresh: u32,
    edits: u32,
}

fn pair(s: &Start, bridge: &ApiBridge, name: &str) -> DeviceRecord {
    let me = Peer {
        pid: Some(std::process::id()),
    };
    let reply = s
        .live
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
    let s = Start::with(StartOptions {
        sessions: 2,
        api: false,
        tweak: Box::new(|cfg: &mut Config| {
            cfg.api_listen = Some("127.0.0.1:0".to_string());
            cfg.confirm_ttl = TTL;
        }),
    });
    // The bound off: a sequence proposes more than 4 times in a minute.
    let out = s
        .live
        .cli(&["settings", "set", "chat.propose_per_min", "0"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let bridge = Arc::new(ApiBridge::new(s.live.controller.clone()));
    let devices = vec![pair(&s, &bridge, "MacBook"), pair(&s, &bridge, "iPad")];
    let shells = (0..2)
        .map(|n| s.chat_shell(n, true, Some(&s.sessions[n]), &[]))
        .collect();
    World {
        s,
        bridge,
        devices,
        shells,
        proposals: Vec::new(),
        codes: Vec::new(),
        spent: BTreeMap::new(),
        fresh: 0,
        edits: 0,
    }
}

fn word(r: &pf_ipc::Response) -> Option<String> {
    r.result
        .get("refusal")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The operator's `arm --yes` with the same inputs as a proposal, as owned values, so a thread can
/// run it.
struct CliArm {
    args: Vec<String>,
    state: std::path::PathBuf,
    share: std::path::PathBuf,
    path: String,
    home: std::path::PathBuf,
}

impl CliArm {
    fn of(s: &Start) -> CliArm {
        let mut args = s.propose_args("s1");
        args[0] = "arm".to_string();
        args.push("--yes".to_string());
        CliArm {
            args,
            state: s.live.sb.state.clone(),
            share: s.share.clone(),
            path: s.live.sb.path_env(),
            home: s.live.sb.root.path().to_path_buf(),
        }
    }

    fn run(&self) -> i32 {
        let out = Command::new(BIN)
            .args(&self.args)
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", &self.home)
            .env("PF_STATE_DIR", &self.state)
            .env("PF_SHARE_DIR", &self.share)
            .output()
            .unwrap();
        out.status.code().unwrap_or(-1)
    }
}

impl World {
    fn store(&self) -> &Arc<pf_store_sqlite::SqliteStore> {
        self.s.live.controller.store()
    }

    fn admissions(&self) -> Vec<pf_engine::port::store::AdmissionRow> {
        self.store().admissions(CONTROLLER_SCOPE).unwrap()
    }

    fn ledger(&self) -> Vec<String> {
        controller_ledger(&self.s.live)
    }

    fn known_command(&self, cmd: &str) -> bool {
        matches!(self.store().admission(CONTROLLER_SCOPE, cmd), Ok(Some(_)))
    }

    fn fresh_command(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}-new{}", self.fresh)
    }

    fn pick(&self, prop: usize) -> Option<Prop> {
        if self.proposals.is_empty() {
            None
        } else {
            Some(self.proposals[prop % self.proposals.len()].clone())
        }
    }

    // ------------------------------------------------------------------ the steps

    fn propose(&mut self, sess: usize, cmd: &str) {
        let known = self.known_command(cmd);
        let (adm0, lines0, runs0) = (self.admissions().len(), self.ledger().len(), self.s.runs());
        let r = self.s.propose_as(&self.shells[sess], "s1", cmd);
        let (adm, lines) = (self.admissions().len() - adm0, self.ledger().len() - lines0);
        assert_eq!(
            adm, lines,
            "one ledger line for each new admission: propose {cmd}"
        );
        assert!(adm <= 1, "propose {cmd}");
        assert_eq!(self.s.runs().len(), runs0.len(), "propose arms nothing");
        // A caller refusal comes before the store is asked about the command, so it is one refused
        // admission under an ID of its own: a halt or a session end stopped this shell's chat.
        let caller_refused = r.error.as_deref().is_some_and(|e| e.contains("the caller"));
        if known && !caller_refused {
            let all = self.admissions();
            assert_eq!(
                adm,
                0,
                "a replay or a conflict writes nothing: propose {cmd}, answer {r:?}, new rows {:?}",
                &all[adm0..]
            );
        }
        if r.ok && r.result["proposal"].is_string() {
            let id = r.result["proposal"].as_str().unwrap().to_string();
            if !self.proposals.iter().any(|p| p.id == id) {
                self.proposals.push(Prop {
                    id,
                    hash: r.result["plan_sha256"].as_str().unwrap().to_string(),
                    session: r.result["session"].as_str().unwrap().to_string(),
                    project: r.result["project_id"].as_str().unwrap().to_string(),
                });
            }
        }
    }

    fn confirm(&mut self, dev: usize, prop: usize, stale: bool, cmd: &str) {
        let Some(p) = self.pick(prop) else { return };
        let known = self.known_command(cmd);
        let claims0 = self.s.claims();
        let (adm0, lines0) = (self.admissions().len(), self.ledger().len());
        let hash = if stale {
            "0".repeat(64)
        } else {
            p.hash.clone()
        };
        let r = self.bridge.call(
            &self.devices[dev],
            "confirm_code",
            json!({
                "subject": {"kind": "arm", "proposal": p.id, "plan_sha256": hash},
                "command_id": cmd,
            }),
        );
        let (adm, lines) = (self.admissions().len() - adm0, self.ledger().len() - lines0);
        assert_eq!(
            adm, lines,
            "one ledger line for each new admission: confirm {cmd}"
        );
        assert!(adm <= 1);
        assert_eq!(self.s.claims(), claims0, "an issue moves no claim");
        if known {
            assert_eq!(adm, 0, "a replay or a conflict writes nothing");
            if r.ok {
                assert_eq!(r.result["replayed"], true, "{r:?}");
            }
            return;
        }
        let open = self
            .s
            .live
            .controller
            .proposal_state(&p.id)
            .is_some_and(|st| st == "open");
        if stale || !open {
            assert_eq!(word(&r).as_deref(), Some("subject_moved"), "{r:?}");
            assert!(r.result["code"].is_null(), "no code for a moved proposal");
        } else if r.ok && r.result["code"].is_string() {
            assert!(
                r.result["generation"].is_null(),
                "an arm code binds no claim"
            );
            self.codes.push(CodeRec {
                text: r.result["code"].as_str().unwrap().to_string(),
                dev,
                prop: self.proposals.iter().position(|x| x.id == p.id).unwrap(),
            });
        }
    }

    fn arm(&mut self, dev: usize, prop: usize, code: usize, cmd: &str) {
        let Some(p) = self.pick(prop) else { return };
        let known = self.known_command(cmd);
        let made_up = format!("pfc1-{}", "ab".repeat(16));
        let text = self.codes.get(code).map_or(made_up, |c| c.text.clone());
        self.spend(dev, &p, &text, cmd, known);
    }

    fn spend(&mut self, dev: usize, p: &Prop, text: &str, cmd: &str, known: bool) {
        let claims0 = self.s.claims();
        let runs0 = self.s.runs().len();
        let (adm0, lines0) = (self.admissions().len(), self.ledger().len());
        let r = self.bridge.call(
            &self.devices[dev],
            "arm",
            json!({"proposal": p.id, "plan_sha256": p.hash, "code": text, "command_id": cmd}),
        );
        let (adm, lines) = (self.admissions().len() - adm0, self.ledger().len() - lines0);
        assert_eq!(
            adm, lines,
            "one ledger line for each new admission: arm {cmd}"
        );
        assert_eq!(self.s.claims(), claims0, "an arm never changes the claims");
        if known {
            assert_eq!(adm, 0, "a replay or a conflict writes nothing");
            assert_eq!(self.s.runs().len(), runs0, "a replay arms nothing");
            return;
        }
        if r.ok {
            assert_eq!(r.result["replayed"], false, "{r:?}");
            let run = r.result["run_id"].as_str().unwrap().to_string();
            assert!(self.store().run(&run).unwrap().is_some(), "{r:?}");
            *self.spent.entry(text.to_string()).or_insert(0) += 1;
            assert_eq!(self.s.runs().len(), runs0 + 1);
        } else {
            assert_eq!(
                self.s.runs().len(),
                runs0,
                "a refused arm arms nothing: {r:?}"
            );
        }
    }

    fn step(&mut self, step: &Step) {
        match step {
            Step::Propose { sess, cmd } => self.propose(*sess, &format!("p{cmd}")),
            Step::ProposeNew { sess } => {
                let cmd = self.fresh_command("p");
                self.propose(*sess, &cmd)
            }
            Step::Confirm {
                dev,
                prop,
                stale,
                cmd,
            } => self.confirm(*dev, *prop, *stale, &format!("c{cmd}")),
            Step::ConfirmNew { dev, prop } => {
                let cmd = self.fresh_command("c");
                self.confirm(*dev, *prop, false, &cmd)
            }
            Step::Arm {
                dev,
                prop,
                code,
                cmd,
            } => self.arm(*dev, *prop, *code, &format!("a{cmd}")),
            Step::ArmLatest { dev } => {
                let latest = self.codes.iter().rev().find(|c| c.dev == *dev).cloned();
                if let Some(c) = latest {
                    let p = self.proposals[c.prop].clone();
                    let cmd = self.fresh_command("a");
                    self.spend(*dev, &p, &c.text, &cmd, false);
                }
            }
            Step::EditShare => {
                self.edits += 1;
                let file = self
                    .s
                    .share
                    .join("workflows")
                    .join(PACKAGE)
                    .join("workflow.yaml");
                let mut text = std::fs::read_to_string(&file).unwrap();
                text.push_str(&format!("\n# edit {}\n", self.edits));
                std::fs::write(&file, text).unwrap();
            }
            Step::EditSnapshot { prop } => {
                let Some(p) = self.pick(*prop) else { return };
                let file = self
                    .s
                    .live
                    .sb
                    .state
                    .join("proposals")
                    .join(&p.id)
                    .join("package/workflow.yaml");
                if let Ok(mut text) = std::fs::read_to_string(&file) {
                    self.edits += 1;
                    text.push_str(&format!("\n# snapshot edit {}\n", self.edits));
                    std::fs::write(&file, text).unwrap();
                }
            }
            Step::CliArm => {
                let before = self.s.runs().len();
                let rc = CliArm::of(&self.s).run();
                let after = self.s.runs().len();
                assert!(after <= before + 1);
                assert_eq!(
                    rc == 0,
                    after == before + 1,
                    "rc {rc}, runs {before} -> {after}"
                );
            }
            Step::Halt => {
                let out = self.s.live.cli(&["halt", "--reason", "model"]);
                // A run that an earlier step ended is refused by name; any other refusal is a bug.
                let ended = code(&out) == 1 && stderr(&out).contains("has ended");
                assert!(code(&out) == 0 || ended, "{}", stderr(&out));
            }
            Step::EndRuns => {
                for h in self.s.live.controller.handles() {
                    let active = self
                        .store()
                        .run(&h.info.run_id)
                        .unwrap()
                        .is_some_and(|r| r.state.as_str() == "active");
                    if active {
                        let _ = h.engine.end_run("the model ended it");
                        h.driver.sweep();
                    }
                }
            }
            Step::Tick => self.s.live.controller.tick_all(),
            Step::Ttl => self
                .s
                .live
                .controller
                .advance_confirm_clock(TTL + Duration::from_secs(1)),
        }
        self.invariants();
    }

    // ------------------------------------------------------------------ the invariants

    fn invariants(&self) {
        let ledger = self.ledger();
        let admissions = self.admissions();
        let runs = self.s.runs();

        // Each proposal arms at most one run: one admitted `device_arm` line, one `armed` result.
        let mut armed_by_proposal: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for n in 0..2 {
            for (_, body) in self.s.bodies(n, "start_result") {
                if body["result"] == "armed" {
                    // The `answers_seq` names the card; the card names the proposal.
                    let card = self
                        .s
                        .bodies(n, "start_card")
                        .into_iter()
                        .find(|(seq, _)| Some(*seq) == body["answers_seq"].as_u64())
                        .unwrap_or_else(|| panic!("an armed result answers no card: {body}"));
                    let run = body["run"].as_str().unwrap_or_default().to_string();
                    assert!(
                        self.store().run(&run).unwrap().is_some(),
                        "an armed result names a run that does not exist: {body}"
                    );
                    armed_by_proposal
                        .entry(card.1["proposal"].as_str().unwrap().to_string())
                        .or_default()
                        .push(run);
                }
            }
        }
        for (proposal, runs) in &armed_by_proposal {
            assert_eq!(runs.len(), 1, "{proposal} armed {runs:?}");
        }
        for p in &self.proposals {
            let admitted: Vec<&String> = ledger
                .iter()
                .filter(|l| {
                    l.contains("device_arm ")
                        && l.contains(&format!("proposal={} ", p.id))
                        && l.contains(" admitted ")
                })
                .collect();
            assert!(
                admitted.len() <= 1,
                "{} was admitted twice: {admitted:?}",
                p.id
            );
            let state = self
                .s
                .live
                .controller
                .proposal_state(&p.id)
                .unwrap_or_default();
            assert!(state != "arming", "{} is left arming", p.id);
            // Every admitted `device_arm` is followed by a run, or by the proposal's failed result.
            if admitted.len() == 1 {
                let run_line = ledger.iter().any(|l| {
                    l.contains("run_project ") && l.contains(&format!("proposal={} ", p.id))
                });
                let failed = state.starts_with("failed:");
                assert!(
                    run_line || failed,
                    "{} admitted, then neither run nor failure: {state}",
                    p.id
                );
            }
        }

        // Every run is one of two kinds.
        for run in &runs {
            let id = run["id"].as_str().unwrap();
            let project = run["project_id"].as_str();
            let linked: Vec<&String> = ledger
                .iter()
                .filter(|l| l.contains(&format!("run_project {id} ")))
                .collect();
            match project {
                Some(project) => {
                    assert_eq!(linked.len(), 1, "{id}: {linked:?}");
                    let proposal = self
                        .proposals
                        .iter()
                        .find(|p| linked[0].contains(&format!("proposal={} ", p.id)))
                        .unwrap_or_else(|| panic!("{id}: no proposal in {}", linked[0]));
                    assert_eq!(
                        proposal.project, project,
                        "the run's project is its session's"
                    );
                    assert!(
                        armed_by_proposal
                            .get(&proposal.id)
                            .is_some_and(|r| r.contains(&id.to_string())),
                        "{id}: no `armed` result names it"
                    );
                    assert!(
                        ledger.iter().any(|l| l.contains("device_arm ")
                            && l.contains(&format!("proposal={} ", proposal.id))
                            && l.contains(" admitted ")),
                        "{id}: no admitted device_arm line"
                    );
                }
                None => {
                    assert!(linked.is_empty(), "{id}: a CLI run with a link {linked:?}");
                    assert!(
                        !armed_by_proposal
                            .values()
                            .any(|r| r.contains(&id.to_string())),
                        "{id}: a CLI run with a start_result"
                    );
                }
            }
        }

        // At most one run that is not at an end state.
        let active = runs.iter().filter(|r| r["state"] == "active").count();
        assert!(active <= 1, "{active} active runs: {runs:?}");

        // Each code admits at most one act.
        for (text, n) in &self.spent {
            assert!(*n <= 1, "{text} was spent {n} times");
        }
        let mut codes_seen: BTreeSet<String> = BTreeSet::new();
        for a in admissions
            .iter()
            .filter(|a| a.kind == "device_arm" && a.outcome == AdmissionOutcome::Admitted)
        {
            let content: Value = serde_json::from_str(&a.content).unwrap();
            let sha = content["code_sha256"].as_str().unwrap().to_string();
            assert!(codes_seen.insert(sha), "a code admitted two acts");
        }

        // No duplicate launch: one admission for each unit, role and attempt of a run.
        for run in &runs {
            let mut seen: BTreeSet<(Option<String>, Option<String>, Option<u32>)> = BTreeSet::new();
            for l in self.s.launches(run["id"].as_str().unwrap()) {
                if l.outcome == AdmissionOutcome::Admitted {
                    assert!(
                        seen.insert((l.unit_id.clone(), l.role.clone(), l.attempt)),
                        "a duplicate launch: {l:?}"
                    );
                }
            }
        }

        // No claim ever: an arm takes none, and nothing in a sequence claims.
        assert!(self.s.claims().is_empty(), "{:?}", self.s.claims());

        // A session holds at most one open proposal.
        for session in &self.s.sessions {
            let open = self
                .proposals
                .iter()
                .filter(|p| &p.session == session)
                .filter(|p| {
                    self.s
                        .live
                        .controller
                        .proposal_state(&p.id)
                        .is_some_and(|st| st == "open")
                })
                .count();
            assert!(open <= 1, "{session} holds {open} open proposals");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn a_start_card_arms_at_most_one_run_and_each_run_is_one_of_two_kinds(
        steps in proptest::collection::vec(step(), 1..14)
    ) {
        let mut w = world();
        for s in &steps {
            w.step(s);
        }
    }
}

// ---------------------------------------------------------------------- the fixed cases

/// A proposal from each session, each confirmed by a device: `(proposal, hash, code)` for each.
fn two_confirmed(w: &mut World) -> Vec<(Prop, String)> {
    let mut out = Vec::new();
    for n in 0..2 {
        w.propose(n, &format!("fixed-p{n}"));
        let p = w.proposals[n].clone();
        let r = w.bridge.call(
            &w.devices[n],
            "confirm_code",
            json!({
                "subject": {"kind": "arm", "proposal": p.id, "plan_sha256": p.hash},
                "command_id": format!("fixed-c{n}"),
            }),
        );
        assert!(r.ok, "{r:?}");
        out.push((p, r.result["code"].as_str().unwrap().to_string()));
    }
    out
}

fn arm_args(p: &Prop, code: &str, cmd: &str) -> Value {
    json!({"proposal": p.id, "plan_sha256": p.hash, "code": code, "command_id": cmd})
}

fn assert_loser_is_open(w: &World, loser: &Prop, loser_cmd: &str, winner_run: &str) {
    let state = w.s.live.controller.proposal_state(&loser.id).unwrap();
    assert_eq!(state, "open", "no zombie `arming`");
    let n =
        w.s.sessions
            .iter()
            .position(|s| *s == loser.session)
            .unwrap();
    let results: Vec<_> =
        w.s.bodies(n, "start_result")
            .into_iter()
            .filter(|(_, b)| b["result"] == "armed")
            .collect();
    assert!(
        results.is_empty(),
        "the loser's card has no result: {results:?}"
    );
    let rows = w.admissions();
    let mine: Vec<_> = rows.iter().filter(|a| a.command_id == loser_cmd).collect();
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0].outcome, AdmissionOutcome::Refused, "{:?}", mine[0]);
    assert!(
        mine[0]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains(winner_run),
        "the refusal names the winner's run {winner_run}: {:?}",
        mine[0].reason
    );
    assert!(
        w.ledger().iter().all(|l| !(l.contains("device_arm")
            && l.contains(&format!("proposal={} ", loser.id))
            && l.contains(" admitted "))),
        "no admitted line for the loser"
    );
}

#[test]
fn two_arms_at_once_make_one_run_and_the_loser_stays_open() {
    let mut w = world();
    let both = two_confirmed(&mut w);
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<pf_ipc::Response> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|n| {
                let bridge = w.bridge.clone();
                let device = w.devices[n].clone();
                let args = arm_args(&both[n].0, &both[n].1, &format!("fixed-a{n}"));
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    bridge.call(&device, "arm", args)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winners: Vec<usize> = (0..2).filter(|n| results[*n].ok).collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    let (win, lose) = (winners[0], 1 - winners[0]);
    let runs = w.s.runs();
    assert_eq!(runs.len(), 1, "exactly one run: {runs:?}");
    let run = runs[0]["id"].as_str().unwrap();
    assert_eq!(results[win].result["run_id"], run);
    assert_loser_is_open(&w, &both[lose].0, &format!("fixed-a{lose}"), run);
    w.invariants();
}

#[test]
fn a_device_arm_and_the_operators_cli_arm_at_once_make_one_run() {
    let mut w = world();
    let both = two_confirmed(&mut w);
    let cli = CliArm::of(&w.s);
    let barrier = Arc::new(Barrier::new(2));
    let (device_reply, cli_rc) = std::thread::scope(|scope| {
        let bridge = w.bridge.clone();
        let device = w.devices[0].clone();
        let args = arm_args(&both[0].0, &both[0].1, "fixed-a0");
        let b1 = barrier.clone();
        let t1 = scope.spawn(move || {
            b1.wait();
            bridge.call(&device, "arm", args)
        });
        let b2 = barrier.clone();
        let cli = &cli;
        let t2 = scope.spawn(move || {
            b2.wait();
            cli.run()
        });
        (t1.join().unwrap(), t2.join().unwrap())
    });
    let runs = w.s.runs();
    assert_eq!(runs.len(), 1, "exactly one run: {runs:?}");
    let run = runs[0]["id"].as_str().unwrap().to_string();
    // One of the two won.
    assert!(
        device_reply.ok ^ (cli_rc == 0),
        "device ok {}, cli rc {cli_rc}",
        device_reply.ok
    );
    if device_reply.ok {
        assert_eq!(runs[0]["project_id"], both[0].0.project.as_str());
    } else {
        assert!(runs[0]["project_id"].is_null(), "a CLI run has no project");
        assert_loser_is_open(&w, &both[0].0, "fixed-a0", &run);
    }
    w.invariants();
}
