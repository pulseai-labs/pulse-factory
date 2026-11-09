//! AC-8 of `r1.s3.w3`: the chat verbs as a property. Random sequences of start, send, answer,
//! interrupt, set, end and a sweep with a zero idle window, on a `FakeChat` with a cap of 2. After
//! each step the invariants of `run-state-admission` hold: a replay makes no port call, another
//! content under the same command ID changes nothing, the controller ledger gains one line per new
//! command, and the live processes, the totals and the pending set agree with the store.
//!
//! The sequences run over three projects, not two: with one live Orchestrator per project, two
//! projects can never fill a cap of 2, so the cap would hold by construction. Proptest keeps no
//! failure file (`failure_persistence: None`). No test starts `claude`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use common::chat::*;
use common::ctl::Live;
use pf_engine::port::chat::ChatPort;
use pf_engine::port::store::Store;
use pf_ipc::Handler;
use proptest::prelude::*;
use serde_json::{Value, json};

const CAP: usize = 2;
const PROJECTS: usize = 3;

#[derive(Clone, Copy, Debug)]
enum Text {
    Plain,
    Approval,
    Question,
}

#[derive(Clone, Debug)]
enum Op {
    Start {
        project: usize,
    },
    Send {
        sess: usize,
        text: Text,
    },
    Answer {
        sess: usize,
    },
    Interrupt {
        sess: usize,
    },
    Set {
        sess: usize,
        effort: usize,
    },
    End {
        sess: usize,
    },
    Sweep,
    /// Call an earlier command again, with the same ID and the same content.
    Repeat {
        back: usize,
    },
    /// Use an earlier command's ID for another content.
    Clash {
        back: usize,
    },
}

fn op() -> impl Strategy<Value = Op> {
    let sess = 0..4usize;
    prop_oneof![
        3 => (0..PROJECTS).prop_map(|project| Op::Start { project }),
        4 => (sess.clone(), prop_oneof![
                3 => Just(Text::Plain), 2 => Just(Text::Approval), 1 => Just(Text::Question)])
            .prop_map(|(sess, text)| Op::Send { sess, text }),
        3 => sess.clone().prop_map(|sess| Op::Answer { sess }),
        1 => sess.clone().prop_map(|sess| Op::Interrupt { sess }),
        2 => (sess.clone(), 0..2usize).prop_map(|(sess, effort)| Op::Set { sess, effort }),
        2 => sess.prop_map(|sess| Op::End { sess }),
        2 => Just(Op::Sweep),
        3 => (0..8usize).prop_map(|back| Op::Repeat { back }),
        2 => (0..8usize).prop_map(|back| Op::Clash { back }),
    ]
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Approval,
    Question,
}

struct Session {
    id: String,
    ended: bool,
    /// The pending request the model expects: its `seq` and kind.
    pending: Option<(u64, Kind)>,
}

/// What the model knows of the first use of a command ID.
struct Used {
    ok: bool,
}

struct World {
    live: Live,
    rec: std::sync::Arc<Rec>,
    projects: Vec<String>,
    sessions: Vec<Session>,
    used: BTreeMap<String, Used>,
    /// The commands run so far, oldest first: the verb, the ID and the arguments.
    history: Vec<(&'static str, String, Value)>,
    next_id: usize,
}

/// The port's own counters: opens, stops, texts sent, live option changes.
fn counters(w: &World) -> (usize, usize, usize, usize) {
    let sent: usize = w
        .rec
        .chats
        .lock()
        .unwrap()
        .iter()
        .map(|c| w.rec.fake.sent(c).len())
        .sum();
    (
        w.rec.open_count(),
        w.rec.stop_count(),
        sent,
        w.rec.sets.lock().unwrap().len(),
    )
}

fn call(w: &World, verb: &str, args: Value) -> (bool, Value, String) {
    let r = w.live.controller.handle(pf_ipc::Request::new(verb, args));
    (r.ok, r.result, r.error.unwrap_or_default())
}

fn item_count(w: &World, session: &str, kind: &str) -> usize {
    items(&w.live, session)
        .iter()
        .filter(|i| i.kind == kind)
        .count()
}

fn wait(what: &str, ok: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fresh(w: &mut World) -> String {
    w.next_id += 1;
    format!("n{}", w.next_id)
}

fn pick(w: &World, sess: usize) -> Option<usize> {
    if w.sessions.is_empty() {
        None
    } else {
        Some(sess % w.sessions.len())
    }
}

/// Run one step. A step the model cannot run (a send during a pending card) is skipped.
fn step(w: &mut World, op: &Op) {
    let ledger_before = controller_ledger(&w.live).len();
    // The verb, its arguments, and what the model does when the first call is admitted.
    let (verb, cmd, args) = match op {
        Op::Sweep => {
            w.live.controller.sweep_chats();
            return;
        }
        Op::Repeat { back } => {
            if w.history.is_empty() {
                return;
            }
            let (verb, cmd, args) = w.history[back % w.history.len()].clone();
            let first = w.used.get(&cmd).expect("a used command");
            let first_ok = first.ok;
            let before = counters(w);
            let (ok, result, why) = call(w, verb, args);
            assert_eq!(ok, first_ok, "replay of {verb} {cmd}: {why}");
            if ok {
                assert_eq!(result["replayed"], true, "replay of {verb} {cmd}");
            }
            assert_eq!(counters(w), before, "a replay of {cmd} made a port call");
            assert_eq!(controller_ledger(&w.live).len(), ledger_before, "{cmd}");
            return;
        }
        Op::Clash { back } => {
            if w.history.is_empty() || w.sessions.is_empty() {
                return;
            }
            let (_, cmd, _) = w.history[back % w.history.len()].clone();
            let before = counters(w);
            let n_items: usize = w.sessions.iter().map(|s| items(&w.live, &s.id).len()).sum();
            let text = format!("clash {}", w.next_id);
            w.next_id += 1;
            let s = w.sessions[0].id.clone();
            let (ok, _, why) = call(
                w,
                "session_send",
                json!({"session_id": s, "text": text, "command_id": cmd}),
            );
            assert!(!ok, "a send under the used ID {cmd} was admitted");
            assert!(why.contains(&cmd), "the conflict names the ID: {why}");
            assert_eq!(counters(w), before, "a conflict made a port call");
            assert_eq!(controller_ledger(&w.live).len(), ledger_before);
            let after: usize = w.sessions.iter().map(|s| items(&w.live, &s.id).len()).sum();
            assert_eq!(after, n_items, "a conflict changed the log");
            return;
        }
        Op::Start { project } => {
            let id = fresh(w);
            let p = w.projects[*project].clone();
            (
                "session_start",
                id.clone(),
                json!({"project_id": p, "command_id": id}),
            )
        }
        Op::Send { sess, text } => {
            let Some(i) = pick(w, *sess) else { return };
            if w.sessions[i].pending.is_some() {
                return;
            }
            let id = fresh(w);
            let body = match text {
                Text::Plain => format!("hello {id}"),
                Text::Approval => "fake:approval".to_string(),
                Text::Question => "fake:question".to_string(),
            };
            let s = w.sessions[i].id.clone();
            (
                "session_send",
                id.clone(),
                json!({"session_id": s, "text": body, "command_id": id}),
            )
        }
        Op::Answer { sess } => {
            let Some(i) = pick(w, *sess) else { return };
            let s = w.sessions[i].id.clone();
            let id = fresh(w);
            let (seq, answer) = match w.sessions[i].pending {
                Some((seq, Kind::Approval)) => (seq, json!({"type": "allow"})),
                Some((seq, Kind::Question)) => (
                    seq,
                    json!({"type": "answers", "value": {"Which one?": "B"}}),
                ),
                // Nothing is pending: the answer names a request that is not there.
                None => (9999, json!({"type": "allow"})),
            };
            (
                "session_answer",
                id.clone(),
                json!({"session_id": s, "seq": seq, "answer": answer, "command_id": id}),
            )
        }
        Op::Interrupt { sess } => {
            let Some(i) = pick(w, *sess) else { return };
            if w.sessions[i].pending.is_some() {
                return;
            }
            let s = w.sessions[i].id.clone();
            let id = fresh(w);
            (
                "session_interrupt",
                id.clone(),
                json!({"session_id": s, "command_id": id}),
            )
        }
        Op::Set { sess, effort } => {
            let Some(i) = pick(w, *sess) else { return };
            let s = w.sessions[i].id.clone();
            let id = fresh(w);
            let e = ["low", "medium"][*effort];
            (
                "session_set",
                id.clone(),
                json!({"session_id": s, "effort": e, "command_id": id}),
            )
        }
        Op::End { sess } => {
            let Some(i) = pick(w, *sess) else { return };
            let s = w.sessions[i].id.clone();
            let id = fresh(w);
            (
                "session_end",
                id.clone(),
                json!({"session_id": s, "command_id": id}),
            )
        }
    };

    let before = counters(w);
    let turns_before = |w: &World, s: &str| item_count(w, s, "turn_result");
    assert!(!w.used.contains_key(&cmd), "a fresh ID {cmd} was used");
    w.history.push((verb, cmd.clone(), args.clone()));
    {
        {
            let session_idx = args["session_id"]
                .as_str()
                .and_then(|s| w.sessions.iter().position(|x| x.id == s));
            let turns = session_idx.map(|i| turns_before(w, &w.sessions[i].id));
            let (ok, result, _why) = call(w, verb, args.clone());
            w.used.insert(cmd.clone(), Used { ok });
            assert_eq!(
                controller_ledger(&w.live).len(),
                ledger_before + 1,
                "one ledger line for the new command {verb} {cmd} (ok={ok})"
            );
            if !ok {
                let after = counters(w);
                assert_eq!((after.2, after.3), (before.2, before.3), "a refusal sent");
            } else {
                match op {
                    Op::Start { .. } => {
                        let id = result["session"]["id"].as_str().unwrap().to_string();
                        w.sessions.push(Session {
                            id,
                            ended: false,
                            pending: None,
                        });
                    }
                    Op::Send { text, .. } => {
                        let i = session_idx.unwrap();
                        let s = w.sessions[i].id.clone();
                        match text {
                            Text::Plain => {
                                let n = turns.unwrap() + 1;
                                wait("the turn result of a send", || {
                                    item_count(w, &s, "turn_result") >= n
                                });
                            }
                            Text::Approval | Text::Question => {
                                let kind = if matches!(text, Text::Approval) {
                                    Kind::Approval
                                } else {
                                    Kind::Question
                                };
                                wait("the pending request", || {
                                    !w.live
                                        .controller
                                        .store()
                                        .pending_requests(&s)
                                        .unwrap()
                                        .is_empty()
                                });
                                let seq =
                                    w.live.controller.store().pending_requests(&s).unwrap()[0].seq;
                                w.sessions[i].pending = Some((seq, kind));
                            }
                        }
                    }
                    Op::Answer { .. } => {
                        let i = session_idx.unwrap();
                        let s = w.sessions[i].id.clone();
                        let n = turns.unwrap() + 1;
                        wait("the turn result of an answer", || {
                            item_count(w, &s, "turn_result") >= n
                        });
                        w.sessions[i].pending = None;
                    }
                    Op::End { .. } => {
                        let i = session_idx.unwrap();
                        w.sessions[i].ended = true;
                        w.sessions[i].pending = None;
                    }
                    Op::Interrupt { .. }
                    | Op::Set { .. }
                    | Op::Sweep
                    | Op::Repeat { .. }
                    | Op::Clash { .. } => {}
                }
            }
        }
    }
}

/// What must hold after every step.
fn check(w: &World) {
    let live = w.live.controller.live_chat_sessions();
    let distinct: BTreeSet<&String> = live.iter().collect();
    assert_eq!(
        distinct.len(),
        live.len(),
        "two processes for one session: {live:?}"
    );
    assert!(
        live.len() <= CAP,
        "more than {CAP} live processes: {live:?}"
    );
    for s in &live {
        let m = w
            .sessions
            .iter()
            .find(|x| &x.id == s)
            .expect("a known session");
        assert!(!m.ended, "{s} is ended and has a process");
    }
    // No process is alive that the controller does not hold.
    let alive = w
        .rec
        .chats
        .lock()
        .unwrap()
        .iter()
        .filter(|c| w.rec.fake.is_alive(c).unwrap())
        .count();
    assert_eq!(
        alive,
        live.len(),
        "live processes and the controller's table differ"
    );

    let store = w.live.controller.store();
    let mut operator_messages = 0;
    for m in &w.sessions {
        let row = store.chat_session(&m.id).unwrap().unwrap();
        let all = items(&w.live, &m.id);
        operator_messages += all.iter().filter(|i| i.kind == "operator_message").count();
        let mut sum = [0u64; 5];
        for item in all.iter().filter(|i| i.kind == "turn_result") {
            let v: Value = serde_json::from_str(&item.payload).unwrap();
            for (n, k) in [
                "input_tokens",
                "output_tokens",
                "cache_read_tokens",
                "cache_write_tokens",
                "cost_micros",
            ]
            .iter()
            .enumerate()
            {
                sum[n] += v["usage"][k].as_u64().unwrap();
            }
        }
        assert_eq!(
            [
                row.input_tokens,
                row.output_tokens,
                row.cache_read_tokens,
                row.cache_write_tokens,
                row.cost_micros
            ],
            sum,
            "the totals of {} are the sum of its turns",
            m.id
        );
        // The pending set equals the model's.
        let pending = store.pending_requests(&m.id).unwrap();
        assert_eq!(
            pending.iter().map(|p| p.seq).collect::<Vec<_>>(),
            m.pending.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
            "the pending requests of {}",
            m.id
        );
        assert_eq!(row.ended_at.is_some(), m.ended, "{}", m.id);
    }
    // Every operator message has one admitted `chat_send`.
    let admitted = ledger_kind(&w.live, "chat_send")
        .iter()
        .filter(|l| l.contains(" admitted "))
        .count();
    assert_eq!(
        operator_messages, admitted,
        "operator messages and admitted sends"
    );
    // At most one live Orchestrator per project.
    for (n, project) in w.projects.iter().enumerate() {
        let live_here = store
            .chat_sessions(project)
            .unwrap()
            .iter()
            .filter(|s| s.ended_at.is_none())
            .count();
        assert!(live_here <= 1, "project {n} has {live_here} live sessions");
    }
}

fn world() -> World {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), |cfg| {
        cfg.chat_max_live = CAP;
        cfg.chat_idle = Duration::ZERO;
    });
    let projects = (0..PROJECTS)
        .map(|n| add_project(&live, &repo(&live, &format!("p{n}"))))
        .collect();
    World {
        live,
        rec,
        projects,
        sessions: Vec::new(),
        used: BTreeMap::new(),
        history: Vec::new(),
        next_id: 0,
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        max_shrink_iters: 12,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn the_chat_commands_admit_once_and_keep_their_invariants(
        ops in prop::collection::vec(op(), 3..14)
    ) {
        let mut w = world();
        for op in &ops {
            step(&mut w, op);
            check(&w);
        }
    }
}
