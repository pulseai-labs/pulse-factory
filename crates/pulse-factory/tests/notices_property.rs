//! AC-6 of `r2.s11.w3`: for any sequence of parks, revisions and closes, and any number of ticks
//! and restarts between them, each request event that a tick saw has exactly one notice, no closed
//! request holds a waiting delivery, and no target's position falls. A restart is the loss of the
//! controller's memory of the notices (`Controller::forget_notices`): the store keeps every notice,
//! and the next tick reads them again. The controller runs in the test process on the fake adapter.

mod common;
#[path = "common/notice_rig.rs"]
mod rig;

use std::collections::{BTreeMap, BTreeSet};

use common::chat::*;
use pf_engine::port::store::{Cause, DeliveryState, Store};
use proptest::prelude::*;
use rig::*;
use serde_json::json;

const UNITS: [&str; 3] = ["u1", "u2", "u3"];

#[derive(Clone, Copy, Debug)]
enum Op {
    Park(usize),
    Revise(usize),
    Close(usize),
    Tick,
    Restart,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..3).prop_map(Op::Park),
        (0usize..3).prop_map(Op::Revise),
        (0usize..3).prop_map(Op::Close),
        Just(Op::Tick),
        Just(Op::Tick),
        Just(Op::Restart),
    ]
}

/// What the model says about one request: what a tick saw of it.
#[derive(Default)]
struct Model {
    opened: bool,
    closed: bool,
    top: u32,
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 12,
        max_shrink_iters: 40,
        ..ProptestConfig::default()
    })]

    #[test]
    fn each_request_event_a_tick_saw_has_exactly_one_notice(ops in prop::collection::vec(op(), 1..16)) {
        let live = start(Rec::new(strict_fake()));
        let project = add_project(&live, &repo(&live, "alpha"));
        live.arm_with_units(&UNITS);
        let run = live.handle().info.run_id.clone();
        link(&live, &run, &project);
        let engine = live.handle().engine.clone();

        // The expected set of notices, built from what each tick saw.
        let mut expected: BTreeSet<(String, u32, String)> = BTreeSet::new();
        let mut model: BTreeMap<String, Model> = BTreeMap::new();
        let mut acked: BTreeMap<String, u64> = BTreeMap::new();
        let mut parked = [false; 3];
        let mut note = 0;

        let observe = |expected: &mut BTreeSet<(String, u32, String)>, model: &mut BTreeMap<String, Model>| {
            let view = engine.view().unwrap();
            for r in &view.requests {
                let m = model.entry(r.id.clone()).or_default();
                if r.status == pf_engine::port::store::RequestStatus::Open {
                    if !m.opened {
                        m.opened = true;
                        m.top = r.revision;
                        expected.insert((r.id.clone(), r.revision, "opened".to_string()));
                    } else if r.revision > m.top {
                        m.top = r.revision;
                        expected.insert((r.id.clone(), r.revision, "revised".to_string()));
                    }
                } else if m.opened && !m.closed {
                    m.closed = true;
                    expected.insert((r.id.clone(), r.revision, "closed".to_string()));
                }
            }
        };

        for op in &ops {
            match *op {
                Op::Park(i) => {
                    if !parked[i] && engine.park(UNITS[i], Cause::Stalled, "stalled for the test").is_ok() {
                        parked[i] = true;
                    }
                }
                Op::Revise(i) => {
                    if let Some(r) = engine.view().unwrap().open_request_of(UNITS[i]) {
                        note += 1;
                        let (id, rev) = (r.id.clone(), r.revision);
                        let _ = engine.revise_request(&id, rev, &json!({ "note": note }));
                    }
                }
                Op::Close(i) => {
                    if let Some(r) = engine.view().unwrap().open_request_of(UNITS[i]) {
                        let id = r.id.clone();
                        let _ = engine.withdraw_request(&id, "closed by the test");
                    }
                }
                Op::Tick => {
                    live.controller.tick_all();
                    observe(&mut expected, &mut model);
                }
                Op::Restart => live.controller.forget_notices(),
            }
            // No target's position falls, after any step.
            for t in live.controller.store().notice_targets().unwrap() {
                let before = acked.insert(t.target.clone(), t.acked).unwrap_or(0);
                prop_assert!(t.acked >= before, "{} fell from {before} to {}", t.target, t.acked);
            }
        }
        // A last tick, so what the sequence left is derived.
        live.controller.tick_all();
        observe(&mut expected, &mut model);

        let stored: BTreeSet<(String, u32, String)> = notices(&live)
            .into_iter()
            .map(|n| (n.request_id, n.revision, n.event))
            .collect();
        prop_assert_eq!(notices(&live).len(), stored.len(), "a request event holds two notices");
        prop_assert_eq!(&stored, &expected);

        // No closed request holds a waiting delivery.
        let closed: BTreeSet<String> = notices(&live)
            .into_iter()
            .filter(|n| n.event == "closed")
            .map(|n| n.request_id)
            .collect();
        for n in notices(&live).into_iter().filter(|n| closed.contains(&n.request_id)) {
            for d in live.controller.store().notice_deliveries(n.position).unwrap() {
                prop_assert!(
                    d.state == DeliveryState::Superseded,
                    "notice {} of closed request {} waits on {}", n.position, n.request_id, d.channel
                );
            }
        }
    }
}
