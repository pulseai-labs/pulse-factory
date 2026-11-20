//! AC-7 of `r2.s2.w5`: a chat survives a controller `SIGKILL`. The real `pulse-factory controller`
//! binary runs `ClaudeChat` over `pf-claude-replay` in a detached session host. The controller is
//! killed at each of five points and started again: the chat's process is the same, nothing
//! launches a second time, every item is stored once, the turn's cost is counted once, a pending
//! approval stays pending, and each rebind writes its audit lines. Then the cases tier 1 can end
//! in: a host that is gone, a host that does not answer, a host that names another record, a live
//! `set`, and a reader that reconnects.
//!
//! Build `pf-host` and `pf-claude-replay` first (`cargo build -p pf-host -p pf-adapter-claude
//! --bins`). Nothing starts a model, spends a token or reaches a network.

mod common;

use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::chat_restart::*;
use pf_host::launcher::Launcher;
use serde_json::Value;

const PLAIN: &str = "plain_turn";

/// The units' `host_attach … attached` lines in `controller.ledger`.
fn attaches(rig: &ChatRig, unit: &str) -> Vec<String> {
    rig.r.ledger_lines(&format!("host_attach {unit} attached"))
}

fn tier1(rig: &ChatRig, unit: &str, outcome: &str) -> Vec<String> {
    rig.r.ledger_lines(&format!("host_tier1 {unit} {outcome}"))
}

/// Start the rig, send one message, run `hold` until the replay is at the point to kill, kill the
/// controller, start it again and check the chat survived. Returns the rig, the child's PID and the
/// host's unit.
fn killed_at(
    fixture_name: &str,
    gates: &[(&str, &str)],
    manual: bool,
    hold: impl Fn(&ChatRig),
) -> (ChatRig, u32, String) {
    let mut rig = ChatRig::new(fixture_name, None, gates, &[]);
    if manual {
        rig.json(&["session", "set", &rig.session.clone(), "--mode", "manual"]);
    }
    rig.send("hello");
    hold(&rig);
    let pid = rig.pid(0);
    let unit = rig.unit();
    assert!(alive(pid), "the chat's process runs before the kill");
    rig.restart();
    survived(&rig, pid, &unit, 1);
    (rig, pid, unit)
}

/// The chat survived `rebinds` restarts: one process, one host, one launch, the session live.
fn survived(rig: &ChatRig, pid: u32, unit: &str, rebinds: usize) {
    assert!(alive(pid), "the chat's process is the same and runs");
    assert_eq!(rig.launches(), 1, "no second launch");
    let row = rig.row();
    assert!(row.ended_at.is_none(), "the session is still live");
    let hosts = rig.host_rows();
    assert_eq!(hosts.len(), 1, "one host row: {hosts:?}");
    assert!(hosts[0].1.is_none(), "the host row is live");
    // One launch attach, and one attach and one `survived` line for each rebind.
    assert_eq!(
        attaches(rig, unit).len(),
        1 + rebinds,
        "{}",
        rig.r.controller_ledger()
    );
    assert_eq!(tier1(rig, unit, "survived").len(), rebinds);
    assert!(
        rig.r
            .controller_ledger()
            .lines()
            .all(|l| !l.contains(" refused ")),
        "{}",
        rig.r.controller_ledger()
    );
    let rebound = rig.chat_lines("rebind");
    assert_eq!(rebound.len(), rebinds, "{rebound:?}");
    assert!(
        rebound.iter().all(|l| l.contains("outcome=survived")),
        "{rebound:?}"
    );
    let listed = rig.json(&["session", "list"]);
    let mine = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == rig.session.as_str())
        .unwrap_or_else(|| panic!("{listed}"));
    assert!(
        mine["process"].is_object(),
        "the session has a live process: {mine}"
    );
}

/// What a whole plain turn stores: once.
fn plain_turn_done_once(rig: &ChatRig) {
    rig.wait_items("the turn's result", |i| {
        i.iter().filter(|x| x.kind == "turn_result").count() >= 1
    });
    // Wait a moment: a double write would show.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        rig.kinds(),
        ["operator_message", "assistant_text", "turn_result"],
        "every item is stored once"
    );
    let want = result_micros(PLAIN)[0];
    assert_eq!(
        rig.row().cost_micros as u64,
        want,
        "the turn's cost counts once"
    );
    let result = rig
        .items()
        .into_iter()
        .find(|i| i.kind == "turn_result")
        .unwrap();
    assert_eq!(result.payload["usage"]["cost_micros"].as_u64(), Some(want));
    let reply = rig
        .items()
        .into_iter()
        .find(|i| i.kind == "assistant_text")
        .unwrap();
    assert_eq!(
        reply.payload["text"].as_str(),
        Some(first_assistant_text(PLAIN).as_str())
    );
    let turns = rig.chat_lines("turn");
    assert_eq!(turns.len(), 1, "one `turn` line: {turns:?}");
    assert!(
        turns[0].contains(&format!("cost_micros={want}")),
        "{}",
        turns[0]
    );
}

#[test]
fn a_kill_before_the_turn_starts_loses_nothing() {
    let (rig, _, _) = killed_at(PLAIN, &[("command_lifecycle", "g1")], false, |rig| {
        rig.wait_items("the message", |i| {
            i.iter().any(|x| x.kind == "operator_message")
        });
        std::thread::sleep(Duration::from_millis(300));
    });
    rig.release("g1");
    plain_turn_done_once(&rig);
}

#[test]
fn a_kill_after_the_deltas_loses_nothing() {
    let (rig, _, _) = killed_at(PLAIN, &[("assistant", "g1")], false, |rig| {
        rig.wait_progress(0, host_lines_before(PLAIN, "assistant"));
        // The deltas are written; the controller may or may not have read them.
        std::thread::sleep(Duration::from_millis(300));
    });
    assert_eq!(
        rig.count("assistant_text"),
        0,
        "the reply is not stored yet"
    );
    rig.release("g1");
    plain_turn_done_once(&rig);
}

#[test]
fn a_kill_after_the_reply_item_loses_nothing_and_stores_it_once() {
    let (rig, _, _) = killed_at(PLAIN, &[("result", "g1")], false, |rig| {
        rig.wait_items("the reply item", |i| {
            i.iter().any(|x| x.kind == "assistant_text")
        });
    });
    assert_eq!(rig.count("assistant_text"), 1, "the reply is stored once");
    assert_eq!(rig.count("turn_result"), 0);
    rig.release("g1");
    plain_turn_done_once(&rig);
}

#[test]
fn a_pending_approval_stays_pending_and_is_answered_after_the_restart() {
    let (rig, _, _) = killed_at("permissions", &[], true, |rig| {
        rig.wait_items("the approval request", |i| {
            i.iter().any(|x| x.kind == "approval_request")
        });
    });
    // Still pending: no answer item, and the session waits for the operator.
    assert_eq!(rig.count("approval_request"), 1, "stored once");
    assert_eq!(rig.count("approval_answer"), 0);
    assert_eq!(rig.row().state, "needs_you");
    let request = rig
        .items()
        .into_iter()
        .find(|i| i.kind == "approval_request")
        .unwrap();
    rig.json(&[
        "session",
        "answer",
        &rig.session,
        &request.seq.to_string(),
        "--deny",
        "no",
    ]);
    rig.wait_items("the turn's result", |i| {
        i.iter().any(|x| x.kind == "turn_result")
    });
    let kinds = rig.kinds();
    let at = |k: &str| {
        kinds
            .iter()
            .position(|x| x == k)
            .unwrap_or_else(|| panic!("{kinds:?}"))
    };
    assert!(
        at("approval_request") < at("approval_answer")
            && at("approval_answer") < at("assistant_text")
            && at("assistant_text") < at("turn_result"),
        "{kinds:?}"
    );
    assert_eq!(kinds.iter().filter(|k| *k == "approval_request").count(), 1);
    assert_eq!(kinds.iter().filter(|k| *k == "turn_result").count(), 1);
    let answer = rig
        .items()
        .into_iter()
        .find(|i| i.kind == "approval_answer")
        .unwrap();
    assert_eq!(answer.payload["outcome"], "answered");
    assert_eq!(
        rig.row().cost_micros as u64,
        result_micros("permissions")[0]
    );
    assert_eq!(rig.launches(), 1, "the turn went on in the same process");
}

#[test]
fn a_kill_after_the_turn_result_is_committed_counts_the_turn_once() {
    let (rig, _, _) = killed_at(PLAIN, &[], false, |rig| {
        rig.wait_items("the turn's result", |i| {
            i.iter().any(|x| x.kind == "turn_result")
        });
    });
    // The acknowledgement may or may not have gone before the kill: either way the host sends the
    // turn again, and the controller reads it as a re-read.
    plain_turn_done_once(&rig);
    // The chat is usable: a second message runs a second turn in the same process.
    rig.send("again");
    rig.wait_items("the second turn", |i| {
        i.iter().filter(|x| x.kind == "turn_result").count() >= 2
    });
    assert_eq!(rig.launches(), 1);
}

#[test]
fn two_kills_in_a_row_rebind_twice() {
    let (mut rig, pid, unit) = killed_at(PLAIN, &[("result", "g1")], false, |rig| {
        rig.wait_items("the reply item", |i| {
            i.iter().any(|x| x.kind == "assistant_text")
        });
    });
    rig.restart();
    survived(&rig, pid, &unit, 2);
    rig.release("g1");
    plain_turn_done_once(&rig);
}

#[test]
fn a_reader_that_reconnects_after_a_seq_gets_each_later_item_once() {
    let (rig, _, _) = killed_at(PLAIN, &[("result", "g1")], false, |rig| {
        rig.wait_items("the reply item", |i| {
            i.iter().any(|x| x.kind == "assistant_text")
        });
    });
    let before = rig.items();
    let last = before.last().unwrap().seq;
    rig.release("g1");
    plain_turn_done_once(&rig);
    let v = rig.json(&[
        "session",
        "items",
        &rig.session,
        "--after",
        &last.to_string(),
    ]);
    let later: Vec<(u64, String)> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["seq"].as_u64().unwrap(),
                i["body"]["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        later,
        [(last as u64 + 1, "turn_result".to_string())],
        "{later:?}"
    );
}

#[test]
fn a_set_made_while_the_host_lives_writes_revision_two_and_a_restart_still_finds_it_survived() {
    // The `permissions` recording answers a `set_permission_mode` request, so the live change
    // reaches the replay and leaves the process alone.
    let mut rig = ChatRig::new("permissions", None, &[], &[]);
    let unit = rig.unit();
    let pid = rig.pid(0);
    assert_eq!(rig.records().len(), 1, "launch: revision 1");
    rig.json(&[
        "session",
        "set",
        &rig.session.clone(),
        "--mode",
        "manual",
        "--command-id",
        "set-1",
    ]);
    let records = rig.records();
    assert_eq!(records.len(), 2, "the set wrote the next revision");
    assert_eq!(
        (records[1].0, records[1].1),
        (1, 2),
        "incarnation 1, revision 2"
    );
    assert_eq!(records[1].2["settings"]["permission_mode"], "manual");
    assert_eq!(
        records[0].2["settings"]["permission_mode"],
        "bypassPermissions"
    );
    assert_eq!(
        records[1].2["argv"], records[0].2["argv"],
        "`argv` stays as the host ran it"
    );
    let lines = rig.r.ledger_lines("host_record");
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("revision=2 written"))
            .count(),
        1,
        "{lines:?}"
    );
    // A replay of the same command writes no second revision.
    rig.json(&[
        "session",
        "set",
        &rig.session.clone(),
        "--mode",
        "manual",
        "--command-id",
        "set-1",
    ]);
    assert_eq!(rig.records().len(), 2, "a replay writes no revision");
    rig.restart();
    survived(&rig, pid, &unit, 1);
    assert!(
        rig.r
            .ledger_lines(&format!("host_tier1 {unit} lost"))
            .is_empty()
    );
}

// ------------------------------------------------------------------ what else tier 1 ends in

#[test]
fn a_host_stopped_while_the_controller_was_down_is_lost_and_closes_its_requests() {
    let mut rig = ChatRig::new("permissions", None, &[], &[]);
    rig.json(&["session", "set", &rig.session.clone(), "--mode", "manual"]);
    rig.send("list the files");
    rig.wait_items("the approval request", |i| {
        i.iter().any(|x| x.kind == "approval_request")
    });
    let unit = rig.unit();
    rig.r.kill9();
    rig.r.detached().stop(&unit).unwrap();
    rig.r.start_controller();

    let row = rig.row();
    assert_eq!(row.state, "idle");
    assert!(row.ended_at.is_none(), "the session stays live");
    let answer = rig
        .items()
        .into_iter()
        .find(|i| i.kind == "approval_answer")
        .expect("closed");
    assert_eq!(answer.payload["outcome"], "lost");
    assert!(
        answer.payload["reason"]
            .as_str()
            .unwrap()
            .starts_with("the chat process was lost: "),
        "{}",
        answer.payload
    );
    assert_eq!(rig.notices("host_lost").len(), 1);
    let lines = rig.chat_lines("rebind");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("outcome=lost"), "{}", lines[0]);
    assert_eq!(tier1(&rig, &unit, "lost").len(), 1);
    assert!(rig.host_rows()[0].1.is_some(), "the host row ended");
    assert_eq!(rig.launches(), 1, "nothing started");
}

/// Make the host of `unit` unreachable: kill it without a trace, then put in its place a socket that
/// accepts and never speaks, and a pid file that names a live process.
fn make_unreachable(rig: &ChatRig, unit: &str) -> Child {
    rig.r.sigkill_host(unit);
    let server = rig.r.server();
    let (session, inc) = pf_host::names::parse_unit(&server, unit).unwrap();
    let dir = rig.r.hosts_dir(&server).join(format!("{session}-{inc}"));
    let holder = Command::new("sleep")
        .arg("600")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let start = pf_host::peer::start_time(holder.id()).expect("the holder runs");
    std::fs::write(dir.join("host.pid"), format!("{} {start}\n", holder.id())).unwrap();
    let _ = std::fs::remove_file(dir.join("host.sock"));
    let listener = UnixListener::bind(dir.join("host.sock")).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in listener.incoming().flatten() {
            held.push(conn);
        }
    });
    holder
}

#[test]
fn a_host_that_never_sends_a_valid_hello_is_unreachable_and_the_next_send_stops_it_then_resumes() {
    let mut rig = ChatRig::new(PLAIN, Some("resume"), &[], &[]);
    let out = rig
        .r
        .cli(&["settings", "set", "session.attach_wait_ms", "100"]);
    assert!(out.status.success());
    rig.send("hello");
    rig.wait_items("the turn", |i| i.iter().any(|x| x.kind == "turn_result"));
    let unit = rig.unit();
    rig.r.kill9();
    let mut holder = make_unreachable(&rig, &unit);
    rig.r.start_controller();

    let row = rig.row();
    assert_eq!(row.state, "idle");
    assert!(row.ended_at.is_none());
    assert_eq!(rig.notices("host_unreachable").len(), 1);
    let lines = rig.chat_lines("rebind");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("outcome=unreachable"), "{}", lines[0]);
    assert_eq!(tier1(&rig, &unit, "unreachable").len(), 1);
    // Nothing started: the one row, still live, and no other host.
    assert_eq!(rig.host_rows().len(), 1, "{:?}", rig.host_rows());
    assert!(rig.host_rows()[0].1.is_none());
    assert_eq!(rig.launches(), 1);

    // The next send stops the older incarnation first, then resumes the chat in the next one.
    rig.write_transcript(&rig.row().runtime_ref.expect("the runtime ref"));
    rig.send("again");
    rig.wait_items("the resumed turn", |i| {
        i.iter().filter(|x| x.kind == "turn_result").count() >= 2
    });
    assert_eq!(rig.launches(), 2);
    assert!(
        rig.argv(1).iter().any(|a| a == "--resume"),
        "{:?}",
        rig.argv(1)
    );
    let rows = rig.host_rows();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].1.is_some(), "the older incarnation ended");
    assert!(rows[1].1.is_none(), "the newer one runs");
    let _ = holder.kill();
    let _ = holder.wait();
}

#[test]
fn a_host_whose_hello_names_another_record_is_refused_with_one_line() {
    let mut rig = ChatRig::new(PLAIN, None, &[], &[]);
    let out = rig
        .r
        .cli(&["settings", "set", "session.attach_wait_ms", "100"]);
    assert!(out.status.success());
    let unit = rig.unit();
    rig.r.kill9();
    // Tamper with the registry while the controller is down: the row names another record hash.
    {
        let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
        db.execute_batch("DROP TRIGGER hosts_end_once").unwrap();
        db.execute(
            "UPDATE hosts SET launched_record_sha256 = ?1 WHERE unit = ?2",
            [&"0".repeat(64), &unit],
        )
        .unwrap();
    }
    rig.r.start_controller();
    let refused: Vec<String> = rig
        .r
        .ledger_lines("host_attach refused")
        .into_iter()
        .filter(|l| l.contains(&unit))
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("record_sha256"), "{}", refused[0]);
    let row = rig.row();
    assert_eq!(row.state, "idle");
    assert_eq!(rig.notices("host_unreachable").len(), 1);
    assert!(tier1(&rig, &unit, "survived").is_empty());
}

#[test]
fn the_decoded_fixture_cost_is_what_the_tests_compare_with() {
    // A guard for the helpers: the first `result` of the plain fixture is the turn's cost.
    let micros = result_micros(PLAIN);
    assert_eq!(micros[0], 3465, "{micros:?}");
    let v: Value = serde_json::json!(micros);
    assert!(v.as_array().unwrap().len() >= 3);
}
