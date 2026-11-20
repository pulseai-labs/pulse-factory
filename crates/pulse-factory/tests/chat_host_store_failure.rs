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

/// A trigger that refuses the insert of an item of `kind`: a store write that fails.
fn fail_inserts(rig: &ChatRig, kind: &str) {
    let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER fault_insert BEFORE INSERT ON chat_items WHEN NEW.kind = '{kind}' \
         BEGIN SELECT RAISE(ABORT, 'injected store fault'); END;"
    ))
    .unwrap();
}

/// Take the fault away: the store write works again.
fn heal(rig: &ChatRig) {
    let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("DROP TRIGGER fault_insert;").unwrap();
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
    fail_inserts(&rig, "assistant_text");
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

#[test]
fn a_failed_closing_answer_on_an_exit_holds_the_exit_until_a_restart_closes_the_request() {
    let mut rig = ChatRig::new("permissions", None, &[], &[]);
    rig.json(&["session", "set", &rig.session.clone(), "--mode", "manual"]);
    rig.send("list the files");
    rig.wait_items("the approval request", |i| {
        i.iter().any(|x| x.kind == "approval_request")
    });
    let pid = rig.pid(0);

    // The child ends while the store refuses the `lost` answer that closes the request.
    fail_inserts(&rig, "approval_answer");
    let out = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    rig.wait_items("the notice of the lost write", |i| {
        i.iter()
            .any(|x| x.kind == "notice" && x.payload["code"] == "output_not_stored")
    });
    heal(&rig);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.count("approval_answer"), 0, "the request is still open");
    assert!(
        !rig.r.live_hosts().is_empty(),
        "the exit is not acknowledged, so the host waits"
    );

    // A restart reads the exit again. A re-read exit closes the request, and the host ends.
    rig.restart();
    rig.wait_items("the closing answer", |i| {
        i.iter().any(|x| x.kind == "approval_answer")
    });
    let answers: Vec<Item> = rig
        .items()
        .into_iter()
        .filter(|x| x.kind == "approval_answer")
        .collect();
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0].payload["outcome"], "lost");
    rig.r
        .wait_for("the host to end", Duration::from_secs(10), || {
            rig.r.live_hosts().is_empty()
        });
}

#[test]
fn an_exit_after_a_failed_item_write_stores_nothing_until_a_restart_reads_it_again() {
    let mut rig = ChatRig::new(PLAIN, None, &[("assistant", "g1")], &[]);
    rig.send("hello");
    rig.wait_progress(0, host_lines_before(PLAIN, "assistant"));
    std::thread::sleep(Duration::from_millis(300));
    let pid = rig.pid(0);

    // The write of the reply fails once, then the store works again.
    fail_inserts(&rig, "assistant_text");
    rig.release("g1");
    rig.wait_items("the notice of the lost write", |i| {
        i.iter()
            .any(|x| x.kind == "notice" && x.payload["code"] == "output_not_stored")
    });
    heal(&rig);
    rig.wait_progress(0, host_lines_through_result(PLAIN, 1));

    // The child ends. The exit follows the lost write, so it stores no `exited` notice, moves no
    // position and is not acknowledged: the host waits.
    let before = rig.row().host_cursor;
    let out = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    std::thread::sleep(Duration::from_millis(800));
    let exited = |rig: &ChatRig| {
        rig.items()
            .iter()
            .filter(|x| x.kind == "notice" && x.payload["code"] == "exited")
            .count()
    };
    assert_eq!(exited(&rig), 0, "the exit stores no notice");
    assert_eq!(rig.row().host_cursor, before, "the exit moves no position");
    assert!(
        !rig.r.live_hosts().is_empty(),
        "the exit is not acknowledged, so the host waits"
    );

    // A restart reads the output again from the stored position. Each item is stored once, the
    // exit is read again with them, and the host ends.
    rig.restart();
    rig.wait_items("the reply and the result", |i| {
        i.iter().any(|x| x.kind == "assistant_text") && i.iter().any(|x| x.kind == "turn_result")
    });
    rig.r
        .wait_for("the host to end", Duration::from_secs(10), || {
            rig.r.live_hosts().is_empty()
        });
    assert_eq!(
        without_notices(&rig),
        ["operator_message", "assistant_text", "turn_result"],
        "every item is stored once"
    );
    // The exit is at the position of the last stored line (the result), and it is a first exit:
    // the restart stores its notice once.
    assert_eq!(exited(&rig), 1, "the exit notice is stored once");
}
