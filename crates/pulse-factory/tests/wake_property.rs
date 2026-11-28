//! AC-6 of `r2.s3.w4` (gate `run-state-admission`, property test): for any order of request opens,
//! revisions and closes (below and above the line), operator messages, process releases, a dropped
//! count cache (as a restart drops it), grants and revokes, halts, settings changes of the two keys
//! and the cap, and ticks, over one project:
//!
//! - no wake without a live grant at its check, and none with `autopilot.wake` off or a run halted;
//! - no wake whose wake set is empty: each wake lists at least one request;
//! - with the cap `c` at 1 or more, at most `c` wakes between two operator messages;
//! - with `autopilot.wakes_per_hour` `r`, at most `r` wakes in any hour;
//! - each `wake:<S-n>:<seq>` and `woken:<R-n>` is one record and one ledger line;
//! - each wake's item exists before its send, and each send has an item and a record;
//! - no wake lists a request above the line, a closed one, or one that holds a `woken` record;
//! - a wake never moves the agent's `acked` position, and each request is marked woken at most once.
//!
//! Every turn is fed by the test: the wake's send and the operator's send are swallowed, and a `Turn`
//! step begins and ends the turn that a send would start. The controller runs in the test process on
//! a `FakeChat`. Nothing here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;
#[path = "common/wake_rig.rs"]
mod wake_rig;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use pf_engine::port::chat::{ChatEvent, TurnCause};
use pf_engine::port::store::{CONTROLLER_SCOPE, Cause, Store};
use proptest::prelude::*;
use serde_json::{Value, json};
use wake_rig::*;

const N: usize = 5;
const UNITS: [&str; N] = ["u1", "u2", "u3", "u4", "u5"];

#[derive(Clone, Copy, Debug)]
enum Op {
    /// Park a unit: below the line (a stall) or above it (a stop file).
    Park(usize, bool),
    Revise(usize),
    Close(usize),
    Tick,
    /// An operator message: its send is swallowed, and a `Turn` step ends the turn it would start.
    Say,
    /// Begin and end the turn that the last send would start.
    Turn,
    /// The controller forgets what it read of the store, as a restart does.
    DropCache,
    Grant,
    Revoke,
    Halt,
    WakeOn(bool),
    Cap(u8),
    PerHour(u8),
    /// The notice clock moves an hour on.
    Hour,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0usize..3, any::<bool>()).prop_map(|(i, b)| Op::Park(i, b)),
        1 => (0..N).prop_map(Op::Revise),
        1 => (0..N).prop_map(Op::Close),
        4 => Just(Op::Tick),
        2 => Just(Op::Say),
        3 => Just(Op::Turn),
        1 => Just(Op::DropCache),
        2 => Just(Op::Grant),
        1 => Just(Op::Revoke),
        1 => Just(Op::Halt),
        1 => any::<bool>().prop_map(Op::WakeOn),
        2 => (0u8..4).prop_map(Op::Cap),
        2 => (0u8..4).prop_map(Op::PerHour),
        1 => Just(Op::Hour),
    ]
}

/// What the test says about the world, from the steps it took.
struct Model {
    /// For each unit: `Some(below)` once parked.
    parked: [Option<bool>; N],
    closed: [bool; N],
    grant: bool,
    halted: bool,
    wake_on: bool,
    /// The built-ins: cap 3, 6 an hour.
    cap: u64,
    per_hour: u64,
    /// Wakes since the last operator message, and in the current hour (the clock's epoch).
    since_operator: u64,
    this_hour: u64,
    /// The turn that a swallowed send would start.
    pending: Option<TurnCause>,
    /// How many times the chat had been opened when `pending` was set.
    pending_opens: usize,
    seen_sends: usize,
}

impl Model {
    fn new() -> Model {
        Model {
            parked: [None; N],
            closed: [false; N],
            grant: false,
            halted: false,
            wake_on: true,
            cap: 3,
            per_hour: 6,
            since_operator: 0,
            this_hour: 0,
            pending: None,
            pending_opens: 0,
            seen_sends: 0,
        }
    }
}

/// A wake, as its command records it.
struct Wake {
    request: Vec<(String, u32)>,
}

/// The wakes (oldest first) and the `woken` marks that the store holds.
fn wakes_and_marks(w: &W) -> (Vec<(String, Wake)>, Vec<String>) {
    let mut wakes = Vec::new();
    let mut marks = Vec::new();
    for a in w
        .live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
    {
        match a.kind.as_str() {
            "wake" => {
                let v: Value = serde_json::from_str(&a.content).unwrap();
                let request = v["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        (
                            r["request"].as_str().unwrap().to_string(),
                            u32::try_from(r["revision"].as_u64().unwrap()).unwrap(),
                        )
                    })
                    .collect();
                wakes.push((a.command_id.clone(), Wake { request }));
            }
            "woken" => marks.push(a.command_id.clone()),
            _ => {}
        }
    }
    (wakes, marks)
}

/// The request of each unit, by the unit's position.
fn request_ids(w: &W) -> [Option<String>; N] {
    let view = w.live.handle().engine.view().unwrap();
    let mut out: [Option<String>; N] = Default::default();
    for r in &view.requests {
        if let Some(i) = UNITS.iter().position(|u| Some(*u) == r.unit_id.as_deref()) {
            out[i] = Some(r.id.clone());
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 24,
        max_shrink_iters: 30,
        ..ProptestConfig::default()
    })]

    #[test]
    fn a_wake_keeps_to_its_conditions_its_bounds_and_its_records(
        // The starting bounds: the cap (0 is off) and the hour, and whether the grant is given.
        start in (0u8..4, 1u8..4, any::<bool>()),
        ops in prop::collection::vec(op(), 1..30)
    ) {
        // A one-millisecond idle time: a tick releases an idle process, so releases happen at ticks.
        let w = world_with(&UNITS, Start::Running, |cfg| cfg.chat_idle = Duration::from_millis(1));
        w.port.swallow_wakes();
        w.port.swallow_operator(true);
        let engine = w.live.handle().engine.clone();
        let mut m = Model::new();
        let mut note = 0u32;
        let mut acked = w.acked().unwrap_or(0);
        let (mut wakes_before, mut marks_before) = wakes_and_marks(&w);
        w.set("chat.self_turn_cap", &start.0.to_string());
        w.set("autopilot.wakes_per_hour", &start.1.to_string());
        m.cap = u64::from(start.0);
        m.per_hour = u64::from(start.1);
        if start.2 {
            w.grant();
            m.grant = true;
        }

        for op in &ops {
            let mut ends_operator_turn = false;
            match *op {
                Op::Park(i, below) => {
                    if m.parked[i].is_none() {
                        let cause = if below { Cause::Stalled } else { Cause::StopFile };
                        if engine.park(UNITS[i], cause, "parked for the test").is_ok() {
                            m.parked[i] = Some(below);
                        }
                    }
                }
                Op::Revise(i) => {
                    if let Some(r) = engine.view().unwrap().open_request_of(UNITS[i]) {
                        note += 1;
                        let (id, rev) = (r.id.clone(), r.revision);
                        let mut body: Value = serde_json::from_str(&r.body).unwrap_or(json!({}));
                        body["note"] = json!(note);
                        let _ = engine.revise_request(&id, rev, &body);
                    }
                }
                Op::Close(i) => {
                    if let Some(r) = engine.view().unwrap().open_request_of(UNITS[i]) {
                        let id = r.id.clone();
                        if engine.withdraw_request(&id, "closed by the test").is_ok() {
                            m.closed[i] = true;
                        }
                    }
                }
                Op::Tick => w.live.controller.tick_all(),
                Op::Say => {
                    if !m.halted && m.pending.is_none() && !w.live.controller.live_chat_sessions().is_empty() {
                        let out = w.live.cli(&["session", "send", &w.session, "hello"]);
                        if common::code(&out) == 0 {
                            m.pending = Some(TurnCause::Operator);
                            m.pending_opens = w.open_count();
                            // An operator message resets the count.
                            m.since_operator = 0;
                        }
                    }
                }
                Op::Turn => {
                    if w.live.controller.live_chat_sessions().is_empty() {
                        m.pending = None;
                    }
                    if let Some(cause) = m.pending.take() {
                        let begun = w
                            .live
                            .controller
                            .apply_chat_event(&w.session, ChatEvent::TurnStarted { cause });
                        if begun.is_ok() {
                            ends_operator_turn = cause == TurnCause::Operator;
                            w.end(cause);
                        }
                    }
                }
                Op::DropCache => w.live.controller.forget_notices(),
                Op::Grant => {
                    if !m.halted {
                        let _ = w.live.cli(&["grant", "clear", "--project", &w.project]);
                        m.grant = true;
                    }
                }
                Op::Revoke => {
                    if !m.halted {
                        let _ = w.live.cli(&["grant", "revoke", "--project", &w.project]);
                        m.grant = false;
                    }
                }
                Op::Halt => {
                    if !m.halted {
                        let out = w.live.cli(&["halt", "--reason", "the property test"]);
                        if common::code(&out) == 0 {
                            m.halted = true;
                            m.pending = None;
                        }
                    }
                }
                Op::WakeOn(on) => {
                    if !m.halted {
                        w.set("autopilot.wake", if on { "[\"below\"]" } else { "[\"off\"]" });
                        m.wake_on = on;
                    }
                }
                Op::Cap(n) => {
                    if !m.halted {
                        w.set("chat.self_turn_cap", &n.to_string());
                        m.cap = u64::from(n);
                    }
                }
                Op::PerHour(n) => {
                    if !m.halted {
                        w.set("autopilot.wakes_per_hour", &n.to_string());
                        m.per_hour = u64::from(n);
                    }
                }
                Op::Hour => {
                    w.live.controller.advance_notice_clock(Duration::from_secs(3601));
                    m.this_hour = 0;
                }
            }

            // A swallowed wake leaves a turn that only a `Turn` step ends.
            let sends = w.port.wake_sends().len();
            if sends > m.seen_sends {
                m.seen_sends = sends;
                // A wake goes into no pending turn: unless the process was released since, and
                // opened again by the wake, which lost the swallowed send.
                prop_assert!(
                    m.pending.is_none() || w.open_count() > m.pending_opens,
                    "a wake went into a pending turn"
                );
                m.pending = Some(TurnCause::Notice);
                m.pending_opens = w.open_count();
            }

            // Each wake this step made.
            let (wakes, marks) = wakes_and_marks(&w);
            let ids = request_ids(&w);
            for (id, wake) in wakes.iter().skip(wakes_before.len()) {
                prop_assert!(m.grant && m.wake_on && !m.halted, "{id}: a wake with no live grant, or off, or halted");
                prop_assert!(!wake.request.is_empty(), "{id}: a wake with an empty wake set");
                let mut seen = BTreeSet::new();
                for (r, _) in &wake.request {
                    prop_assert!(seen.insert(r.clone()), "{id}: {r} twice in one wake");
                    let unit = ids.iter().position(|x| x.as_deref() == Some(r.as_str()));
                    prop_assert!(unit.is_some(), "{id}: {r} is no request of the run");
                    let u = unit.unwrap();
                    prop_assert_eq!(m.parked[u], Some(true), "{}: {} is not below the line", id, r);
                    prop_assert!(!m.closed[u], "{id}: {r} is closed");
                    prop_assert!(
                        !marks_before.contains(&format!("woken:{r}")),
                        "{id}: {r} already held a woken record"
                    );
                }
                m.since_operator += 1;
                m.this_hour += 1;
                if m.cap >= 1 {
                    prop_assert!(
                        m.since_operator <= m.cap,
                        "{id}: {} wakes since the operator's message, cap {}", m.since_operator, m.cap
                    );
                }
                prop_assert!(m.per_hour >= 1, "{id}: a wake with {} wakes an hour", m.per_hour);
                prop_assert!(
                    m.this_hour <= m.per_hour,
                    "{id}: {} wakes this hour, bound {}", m.this_hour, m.per_hour
                );
            }
            // A wake never moves the agent's position: only an operator turn's result does.
            let now = w.acked().unwrap_or(0);
            if !ends_operator_turn {
                prop_assert_eq!(now, acked, "a step that ended no operator turn moved the position");
            }
            prop_assert!(now >= acked, "the position fell");
            acked = now;
            wakes_before = wakes;
            marks_before = marks;
        }

        // The records, at the end: one command, one ledger line and one item for each wake, the item
        // before the send, and each request marked once.
        let (wakes, marks) = wakes_and_marks(&w);
        let unique: BTreeSet<&String> = wakes.iter().map(|(id, _)| id).collect();
        prop_assert_eq!(unique.len(), wakes.len(), "a wake command twice");
        let marked: BTreeSet<&String> = marks.iter().collect();
        prop_assert_eq!(marked.len(), marks.len(), "a request marked twice");
        prop_assert_eq!(w.ledger("wake").len(), wakes.len());
        prop_assert_eq!(w.ledger("woken").len(), marks.len());
        prop_assert_eq!(w.port.wake_sends().len(), wakes.len());
        let items_by_seq: BTreeMap<u64, String> = w.wake_items().into_iter().collect();
        prop_assert_eq!(items_by_seq.len(), wakes.len());
        for (id, _) in &wakes {
            let seq: u64 = id.rsplit(':').next().unwrap().parse().unwrap();
            prop_assert!(items_by_seq.contains_key(&seq), "{id}: no item {seq}");
        }
        // Each send is the text of one item, in the same order.
        let texts: Vec<String> = items_by_seq.values().cloned().collect();
        prop_assert_eq!(w.port.wake_sends(), texts);
        // Each mark names a request of the run.
        let known: BTreeSet<String> = w
            .live
            .handle()
            .engine
            .view()
            .unwrap()
            .requests
            .iter()
            .map(|r| r.id.clone())
            .collect();
        for mark in &marks {
            let request = mark.trim_start_matches("woken:");
            prop_assert!(known.contains(request), "{mark} names no request");
        }
    }
}
