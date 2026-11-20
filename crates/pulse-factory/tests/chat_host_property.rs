//! AC-11 of `r2.s2.w5` (the `run-state-admission` property): for any recorded fixture of the
//! single-turn set and any set of controller kill points, with or without a gap, each output item
//! is committed once, the session's cost equals the fixture's, the stored position `committed`
//! never moves back, and at most one host of the session is live at any time.
//!
//! The spec asks for an in-memory test host. This test uses the real detached host instead, with
//! the real controller binary and `SIGKILL` (the `ChatRig` of `chat_host_restart.rs`): the
//! controller always makes its own `HostService`, so an in-memory host would reach neither the
//! registry rows tier 1 reads nor the pump that commits and acknowledges, and a copy of that pump
//! in the test would prove nothing about the real one. A kill point is a `--gate` of the replay
//! (before the first `command_lifecycle`, `assistant` or `result` line), and a gap is the recording
//! of `big_output` with its tool output written three times and a 64 KiB host buffer.
//! Proptest keeps no failure file and shrinks a little: a case starts real processes.
//!
//! Build `pf-host` and `pf-claude-replay` first (`cargo build -p pf-host -p pf-adapter-claude
//! --bins`). Nothing starts a model, spends a token or reaches a network.

mod common;

use std::time::Duration;

use common::chat_restart::*;
use proptest::prelude::*;

/// The gates a kill can hold at, in the order the lines come.
const POINTS: [&str; 3] = ["command_lifecycle", "assistant", "result"];

/// The recorded single-turn set: each plays one operator message to one `result`.
const FIXTURES: [&str; 2] = ["plain_turn", "init"];

#[derive(Clone, Debug)]
enum Case {
    /// Kill at the chosen points, and once more after the turn is committed when `at_end`.
    Kills {
        fixture: usize,
        points: [bool; 3],
        at_end: bool,
    },
    /// The host drops output while the controller is down.
    Gap,
}

fn case() -> impl Strategy<Value = Case> {
    prop_oneof![
        4 => (0..FIXTURES.len(), any::<[bool; 3]>(), any::<bool>())
            .prop_map(|(fixture, points, at_end)| Case::Kills { fixture, points, at_end }),
        1 => Just(Case::Gap),
    ]
}

/// What holds at every check of one case.
struct Watch {
    committed: u64,
}

impl Watch {
    /// `committed` never moves back, and at most one host of the session is live.
    fn check(&mut self, rig: &ChatRig) {
        let live = rig
            .host_rows()
            .into_iter()
            .filter(|r| r.1.is_none())
            .count();
        assert!(live <= 1, "more than one live host: {:?}", rig.host_rows());
        if let Some(cursor) = rig.row().host_cursor {
            let committed = cursor["committed"].as_u64().unwrap_or(0);
            assert!(
                committed >= self.committed,
                "committed moved back from {} to {committed}: {cursor}",
                self.committed
            );
            self.committed = committed;
        }
    }
}

/// The turn is stored once, its cost counts once, and each output item is there once.
fn settled(rig: &ChatRig, fixture: &str, watch: &mut Watch) {
    rig.wait_items("the turn's result", |i| {
        i.iter().any(|x| x.kind == "turn_result")
    });
    // A second write of an item would show after a moment.
    std::thread::sleep(Duration::from_millis(400));
    watch.check(rig);
    let kinds = rig.kinds();
    assert_eq!(
        kinds.iter().filter(|k| *k == "turn_result").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "operator_message").count(),
        1,
        "{kinds:?}"
    );
    let seqs: Vec<i64> = rig.items().iter().map(|i| i.seq).collect();
    let mut unique = seqs.clone();
    unique.dedup();
    assert_eq!(seqs, unique, "a seq is repeated");
    let calls: Vec<String> = rig
        .items()
        .into_iter()
        .filter(|i| i.kind == "tool_call")
        .map(|i| {
            i.payload["call_id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let mut once = calls.clone();
    once.sort();
    once.dedup();
    assert_eq!(
        calls.len(),
        once.len(),
        "a tool call is stored twice: {calls:?}"
    );
    let want = result_micros(fixture)[0];
    assert_eq!(rig.row().cost_micros as u64, want, "the cost counts once");
    assert_eq!(rig.chat_lines("turn").len(), 1, "one `turn` line");
    assert_eq!(rig.launches(), 1, "the chat never launched twice");
    assert!(rig.row().ended_at.is_none(), "the chat is still live");
}

fn run(case: &Case) {
    match case {
        Case::Kills {
            fixture,
            points,
            at_end,
        } => {
            let fixture = FIXTURES[*fixture];
            let gates: Vec<(&str, String)> = POINTS
                .iter()
                .zip(points)
                .enumerate()
                .filter(|(_, (_, on))| **on)
                .map(|(i, (ty, _))| (*ty, format!("g{i}")))
                .collect();
            let borrowed: Vec<(&str, &str)> = gates.iter().map(|(t, n)| (*t, n.as_str())).collect();
            let mut rig = ChatRig::new(fixture, None, &borrowed, &[]);
            let mut watch = Watch { committed: 0 };
            rig.send("hello");
            for (ty, name) in &gates {
                rig.wait_progress(0, host_lines_before(fixture, ty));
                if *ty == "result" {
                    rig.wait_items("the reply item", |i| {
                        i.iter().any(|x| x.kind == "assistant_text")
                    });
                }
                std::thread::sleep(Duration::from_millis(200));
                watch.check(&rig);
                rig.restart();
                watch.check(&rig);
                rig.release(name);
            }
            settled(&rig, fixture, &mut watch);
            if *at_end {
                rig.restart();
                settled(&rig, fixture, &mut watch);
            }
        }
        Case::Gap => {
            let tmp = tempfile::tempdir().unwrap();
            let big = tripled_big_output(tmp.path());
            let fixture = big.as_str();
            let mut rig = ChatRig::with(
                fixture,
                None,
                &[("user", "g1")],
                &[],
                &[("session.replay_bytes", "65536")],
            );
            let mut watch = Watch { committed: 0 };
            rig.send("make some output");
            rig.wait_progress(0, host_lines_before(fixture, "user"));
            rig.wait_items("the tool call's start", |i| !i.is_empty());
            watch.check(&rig);
            rig.r.kill9();
            rig.release("g1");
            rig.wait_progress(0, host_lines_before(fixture, "result") + 1);
            rig.r.start_controller();
            settled(&rig, fixture, &mut watch);
            assert_eq!(rig.notices("output_lost").len(), 1, "{:?}", rig.kinds());
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 8,
        max_shrink_iters: 4,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn items_and_cost_are_committed_once_across_any_kills(c in case()) {
        run(&c);
    }
}
