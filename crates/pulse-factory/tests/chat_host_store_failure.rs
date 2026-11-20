//! A chat whose item write fails does not move past the item (class C2 of PR #78, round 1). The
//! real `pulse-factory controller` binary runs `ClaudeChat` over `pf-claude-replay` in a detached
//! session host. A fault in the store refuses the write of the turn's reply, once. The controller
//! then stores no position, acknowledges nothing and applies no later delivery of that process (the
//! turn's result), and it says so in the log. After a restart the host sends the output again from
//! the last stored position, and every item is stored once.
//!
//! Build `pf-host` and `pf-claude-replay` first (`cargo build -p pf-host -p pf-adapter-claude
//! --bins`). Nothing starts a model, spends a token or reaches a network.

mod common;

use std::time::Duration;

use common::chat_restart::*;

const PLAIN: &str = "plain_turn";

/// A trigger that refuses the insert of an `assistant_text` item: a store write that fails.
fn fail_replies(rig: &ChatRig) {
    let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fault_reply BEFORE INSERT ON chat_items WHEN NEW.kind = 'assistant_text' \
         BEGIN SELECT RAISE(ABORT, 'injected store fault'); END;",
    )
    .unwrap();
}

/// Take the fault away: the store write works again.
fn heal(rig: &ChatRig) {
    let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("DROP TRIGGER fault_reply;").unwrap();
}

/// How many host lines a fixture holds through its `n`th `result` line.
fn host_lines_through_result(name: &str, n: usize) -> u64 {
    let mut lines = 0;
    let mut results = 0;
    for line in std::fs::read_to_string(fixture(name)).unwrap().lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["to"] != "host" {
            continue;
        }
        lines += 1;
        if v["msg"]["type"] == "result" {
            results += 1;
            if results == n {
                return lines;
            }
        }
    }
    panic!("{name} has fewer than {n} results");
}

fn without_notices(rig: &ChatRig) -> Vec<String> {
    rig.kinds().into_iter().filter(|k| k != "notice").collect()
}

#[test]
fn a_failed_item_write_stops_the_position_and_the_acknowledgement_until_a_restart_reads_it_again() {
    let mut rig = ChatRig::new(PLAIN, None, &[("assistant", "g1")], &[]);
    rig.send("hello");
    rig.wait_progress(0, host_lines_before(PLAIN, "assistant"));
    // The deltas are written and read; the replay holds before the reply.
    std::thread::sleep(Duration::from_millis(300));
    let before = rig.row().host_cursor;
    assert_eq!(rig.count("assistant_text"), 0);
    let pid = rig.pid(0);

    // The write of the reply fails once. The controller says so in the log.
    fail_replies(&rig);
    rig.release("g1");
    rig.wait_items("the notice of the lost write", |i| {
        i.iter()
            .any(|x| x.kind == "notice" && x.payload["code"] == "output_not_stored")
    });
    heal(&rig);

    // The turn's result is the next delivery. The store works again, yet the controller does not
    // move past the reply that is missing: nothing is stored and no position moves.
    rig.wait_progress(0, host_lines_through_result(PLAIN, 1));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.count("assistant_text"), 0, "the reply is not stored");
    assert_eq!(rig.count("turn_result"), 0, "the result is held behind it");
    assert_eq!(
        rig.row().host_cursor,
        before,
        "no position moved: the store holds the position before the lost item"
    );
    assert_eq!(rig.row().cost_micros, 0, "no cost counted");

    // A restart reads the output again from the position the store holds: each item once.
    rig.restart();
    rig.wait_items("the reply and the result", |i| {
        i.iter().any(|x| x.kind == "assistant_text") && i.iter().any(|x| x.kind == "turn_result")
    });
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        without_notices(&rig),
        ["operator_message", "assistant_text", "turn_result"],
        "every item is stored once"
    );
    assert_eq!(
        rig.row().cost_micros as u64,
        result_micros(PLAIN)[0],
        "the turn's cost counts once"
    );
    assert!(alive(pid), "the chat's process is the same and runs");
    assert_eq!(rig.launches(), 1, "no second launch");
}
