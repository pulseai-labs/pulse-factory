//! AC-9 of `r2.s2.w5`: what ends a chat and what does not. A controller stop (`SIGTERM`) stops no
//! chat: its host and child run on, and `chat.ledger` says `detach`. `halt`, `session end` and
//! `project remove --end-sessions` stop the chat through its host. A chat whose child exits has its
//! host row ended and its host gone at once. Survived chats count against `PF_CHAT_MAX_LIVE` and
//! are released when idle. The orphan sweep stops a chat host whose session ended, and the host of
//! an older incarnation.
//!
//! The real `pulse-factory controller` binary runs `ClaudeChat` over `pf-claude-replay` in a detached
//! host; the cap and the idle release run in a second controller in the test process, on the state
//! of the first, which was killed. Build `pf-host` and `pf-claude-replay` first (`cargo build -p
//! pf-host -p pf-adapter-claude --bins`). Nothing starts a model, spends a token or reaches a
//! network.

mod common;

use std::time::{Duration, Instant};

use common::chat_restart::*;
use pf_ipc::Handler;
use serde_json::json;

const PLAIN: &str = "plain_turn";

/// Wait until every host of the rig's server is gone and every host row of the session ended.
fn wait_hosts_gone(rig: &ChatRig, what: &str) {
    rig.r.wait_for(what, Duration::from_secs(20), || {
        rig.r.live_hosts().is_empty() && rig.host_rows().iter().all(|r| r.1.is_some())
    });
}

#[test]
fn a_controller_stop_stops_no_chat_and_writes_detach() {
    let mut rig = ChatRig::new(PLAIN, None, &[], &[]);
    let pid = rig.pid(0);
    let unit = rig.unit();
    let mut controller = rig.r.controller.take().unwrap();
    let out = std::process::Command::new("kill")
        .args(["-TERM", &controller.id().to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let end = Instant::now() + Duration::from_secs(20);
    while controller.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "the controller did not stop");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        alive(pid),
        "the chat's process runs after the controller stopped"
    );
    assert_eq!(rig.r.live_hosts().len(), 1);
    assert!(rig.host_rows()[0].1.is_none(), "the host row is live");
    let ledger = rig.chat_ledger();
    assert_eq!(
        ledger
            .iter()
            .filter(|l| l.contains(" chat detach session=S-1"))
            .count(),
        1,
        "{ledger:?}"
    );
    assert!(
        ledger.iter().all(|l| !l.contains("reason=shutdown")),
        "a controller stop stops no chat: {ledger:?}"
    );
    // The next start finds it.
    rig.r.start_controller();
    assert!(alive(pid));
    assert_eq!(rig.chat_lines("rebind").len(), 1);
    assert_eq!(rig.launches(), 1);
    let _ = unit;
}

#[test]
fn halt_stops_every_chat_through_its_host() {
    let rig = ChatRig::new(PLAIN, None, &[], &[]);
    let (p2, _) = rig.new_project("beta");
    let s2 = rig.start_session(&p2);
    let pid = rig.pid(0);
    assert_eq!(rig.r.live_hosts().len(), 2, "two chat hosts");
    let out = rig.r.cli(&["halt", "--reason", "test"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait_hosts_gone(&rig, "every chat host to stop");
    assert!(!alive(pid), "the child stopped");
    for row in rig.r.host_rows() {
        assert!(row.1.is_some(), "host row {} ended", row.0);
    }
    let stops: Vec<String> = rig
        .chat_ledger()
        .into_iter()
        .filter(|l| l.contains(" chat stop ") && l.contains("reason=halt"))
        .collect();
    assert_eq!(stops.len(), 2, "{stops:?}");
    // Both sessions stay live and idle: the next message resumes them.
    assert_eq!(rig.row().state, "idle");
    assert!(rig.row().ended_at.is_none());
    let _ = s2;
}

#[test]
fn session_end_stops_the_host() {
    let rig = ChatRig::new(PLAIN, None, &[], &[]);
    let pid = rig.pid(0);
    rig.json(&["session", "end", &rig.session.clone()]);
    wait_hosts_gone(&rig, "the chat host to stop");
    assert!(!alive(pid));
    assert!(rig.row().ended_at.is_some());
}

#[test]
fn project_remove_with_end_sessions_stops_the_host() {
    let rig = ChatRig::new(PLAIN, None, &[], &[]);
    let pid = rig.pid(0);
    let out = rig
        .r
        .cli(&["project", "remove", &rig.project, "--end-sessions"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait_hosts_gone(&rig, "the chat host to stop");
    assert!(!alive(pid));
    assert!(rig.row().ended_at.is_some());
}

#[test]
fn a_chat_whose_child_exits_has_its_row_ended_and_its_host_gone_at_once() {
    let rig = ChatRig::new(PLAIN, None, &[], &[]);
    let pid = rig.pid(0);
    // The replay waits for more input when the recording ends, so the test ends the child. No turn
    // runs first: the usage call after a turn waits for the recording, which has no answer.
    let out = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    rig.wait_items("the exit notice", |i| {
        i.iter()
            .any(|x| x.kind == "notice" && x.payload["code"] == "exited")
    });
    let committed = Instant::now();
    rig.r
        .wait_for("the host row to end", Duration::from_secs(5), || {
            rig.host_rows().iter().all(|r| r.1.is_some())
        });
    assert!(
        committed.elapsed() < Duration::from_secs(2),
        "the row ended {:?} after the exit was committed",
        committed.elapsed()
    );
    rig.r
        .wait_for("the host to end", Duration::from_secs(5), || {
            rig.r.live_hosts().is_empty()
        });
    assert!(!alive(pid));
    let stops: Vec<String> = rig
        .chat_ledger()
        .into_iter()
        .filter(|l| l.contains(" chat stop ") && l.contains("reason=exited"))
        .collect();
    assert_eq!(stops.len(), 1, "{stops:?}");
}

/// Class C1 of PR #79, round 1. The reply is stored and the replay holds before the next line, so
/// the host's end of output is the cursor of the last stored line. A child that ends there gives an
/// exit at the committed position, and that is a first exit, not one the store has already seen:
/// it stores the notice, the ledger line and the session's new state, and the host ends.
#[test]
fn a_chat_that_exits_right_after_a_stored_item_stores_its_exit() {
    let rig = ChatRig::new(
        PLAIN,
        None,
        &[("assistant", "g1"), ("stream_event", "g2")],
        &[],
    );
    rig.send("hello");
    rig.wait_progress(0, host_lines_before(PLAIN, "assistant"));
    let pid = rig.pid(0);
    rig.release("g1");
    rig.wait_items("the reply", |i| {
        i.iter().any(|x| x.kind == "assistant_text")
    });
    // The replay has written the reply line and holds before the next `stream_event` line.
    std::thread::sleep(Duration::from_millis(500));
    let before = rig.row().host_cursor.expect("the reply moved the position");
    let out = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    rig.wait_items("the exit notice", |i| {
        i.iter()
            .any(|x| x.kind == "notice" && x.payload["code"] == "exited")
    });
    rig.r
        .wait_for("the host to end", Duration::from_secs(10), || {
            rig.r.live_hosts().is_empty()
        });
    assert_eq!(rig.notices("exited").len(), 1, "one exit notice");
    let stops: Vec<String> = rig
        .chat_ledger()
        .into_iter()
        .filter(|l| l.contains(" chat stop ") && l.contains("reason=exited"))
        .collect();
    assert_eq!(stops.len(), 1, "{stops:?}");
    // The exit sits at the position the reply moved to: no byte came between them.
    assert_eq!(
        rig.row().host_cursor.expect("the cursor")["committed"],
        before["committed"],
        "the exit is at the committed position"
    );
}

/// The other half of class C1: an exit the store holds, read again, stores nothing more. The
/// controller dies with the child's exit unacknowledged. The test writes what a controller that
/// stored the exit and died before the acknowledgement leaves: the `exited` notice, and `exited` in
/// the cursor. The restart reads the exit at the committed position, stores no second notice, writes
/// no ledger line, and acknowledges it, so the host ends.
#[test]
fn an_exit_the_store_holds_is_read_again_without_a_second_notice() {
    let mut rig = ChatRig::new(
        PLAIN,
        None,
        &[("assistant", "g1"), ("stream_event", "g2")],
        &[],
    );
    rig.send("hello");
    rig.wait_progress(0, host_lines_before(PLAIN, "assistant"));
    let pid = rig.pid(0);
    rig.release("g1");
    rig.wait_items("the reply", |i| {
        i.iter().any(|x| x.kind == "assistant_text")
    });
    std::thread::sleep(Duration::from_millis(500));
    rig.r.kill9();
    let out = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output()
        .unwrap();
    assert!(out.status.success());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !rig.r.live_hosts().is_empty(),
        "no controller acknowledged the exit, so the host waits"
    );
    {
        let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.execute_batch(
            "INSERT INTO chat_items (session_id, seq, kind, payload, created_at) \
             SELECT 'S-1', MAX(seq) + 1, 'notice', \
                    json_object('code', 'exited', 'text', 'the chat process exited'), \
                    '2026-01-01T00:00:00Z' \
             FROM chat_items WHERE session_id = 'S-1'; \
             UPDATE chat_sessions SET host_cursor = json_set(host_cursor, '$.exited', json('true')) \
             WHERE id = 'S-1';",
        )
        .unwrap();
    }
    rig.r.start_controller();
    rig.r
        .wait_for("the host to end", Duration::from_secs(10), || {
            rig.r.live_hosts().is_empty()
        });
    assert_eq!(rig.notices("exited").len(), 1, "no second exit notice");
    let stops: Vec<String> = rig
        .chat_ledger()
        .into_iter()
        .filter(|l| l.contains(" chat stop ") && l.contains("reason=exited"))
        .collect();
    assert!(stops.is_empty(), "a re-read exit writes no line: {stops:?}");
}

#[test]
fn survived_chats_count_against_the_cap_and_a_third_start_is_refused_with_both_named() {
    let mut rig = ChatRig::new(PLAIN, None, &[], &[]);
    let (p2, _) = rig.new_project("beta");
    let s2 = rig.start_session(&p2);
    let (p3, _) = rig.new_project("gamma");
    rig.r.kill9();
    let controller = rig.in_process(|cfg| cfg.chat_max_live = 2);
    let mut live = controller.live_chat_sessions();
    live.sort();
    assert_eq!(live, ["S-1", s2.as_str()], "both survived");
    let refused = controller.handle(pf_ipc::Request::new(
        "session_start",
        json!({"project_id": p3, "command_id": "c-third"}),
    ));
    assert!(!refused.ok, "the third start is refused");
    let why = refused.error.unwrap_or_default();
    assert!(why.contains("cap (2)"), "{why}");
    assert!(
        why.contains("S-1") && why.contains(&s2),
        "both are named: {why}"
    );
    drop(controller);
}

#[test]
fn a_survived_chat_idle_past_the_window_is_released_and_its_host_stops() {
    let mut rig = ChatRig::new(PLAIN, None, &[], &[]);
    let pid = rig.pid(0);
    rig.r.kill9();
    let controller = rig.in_process(|cfg| cfg.chat_idle = Duration::from_millis(600));
    assert_eq!(controller.live_chat_sessions(), ["S-1"]);
    // The idle clock started at the rebind: not yet released.
    controller.sweep_chats();
    assert_eq!(
        controller.live_chat_sessions(),
        ["S-1"],
        "released too early"
    );
    std::thread::sleep(Duration::from_millis(900));
    controller.sweep_chats();
    assert!(
        controller.live_chat_sessions().is_empty(),
        "released after the window"
    );
    wait_hosts_gone(&rig, "the released chat's host to stop");
    assert!(!alive(pid));
    let stops: Vec<String> = rig
        .chat_ledger()
        .into_iter()
        .filter(|l| l.contains(" chat stop ") && l.contains("reason=idle"))
        .collect();
    assert_eq!(stops.len(), 1, "{stops:?}");
    assert_eq!(rig.notices("idle_release").len(), 1);
    drop(controller);
}

#[test]
fn the_orphan_sweep_stops_the_host_of_an_ended_session() {
    let mut rig = ChatRig::new(PLAIN, None, &[], &[]);
    let pid = rig.pid(0);
    let unit = rig.unit();
    rig.r.kill9();
    // The session ended while the controller was down.
    {
        let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
        db.execute(
            "UPDATE chat_sessions SET ended_at = '2026-11-20T00:00:00Z' WHERE id = 'S-1'",
            [],
        )
        .unwrap();
    }
    rig.r.start_controller();
    wait_hosts_gone(&rig, "the orphan to stop");
    assert!(!alive(pid));
    let orphans = rig.r.ledger_lines(&format!("host_orphan {unit} orphan"));
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    assert!(
        rig.chat_lines("rebind").is_empty(),
        "an ended session is not rebound"
    );
}

#[test]
fn the_orphan_sweep_stops_the_host_of_an_older_incarnation_with_one_line() {
    let mut rig = ChatRig::new(PLAIN, None, &[], &[]);
    let pid = rig.pid(0);
    let unit = rig.unit();
    rig.r.kill9();
    // A newer incarnation of the same session has a live row whose host is gone.
    let second = unit.replace("-1", "-2");
    assert_ne!(second, unit);
    {
        let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
        db.execute_batch(
            "INSERT INTO launch_records (session_kind, session_id, incarnation, revision, launcher, \
               host_unit, host_socket, record, record_sha256, command_id, created_at) \
             SELECT session_kind, session_id, 2, revision, launcher, host_unit, host_socket, record, \
               record_sha256, command_id || '-2', created_at FROM launch_records \
             WHERE session_kind = 'chat' AND revision = 1;",
        )
        .unwrap();
        db.execute(
            "INSERT INTO hosts (unit, session_kind, session_id, incarnation, launched_revision, \
               launched_record_sha256, launcher, socket, invocation_id, host_binary, host_sha256, \
               protocol, started_at, command_id) \
             SELECT ?1, session_kind, session_id, 2, launched_revision, launched_record_sha256, \
               launcher, socket, invocation_id, host_binary, host_sha256, protocol, started_at, \
               command_id || '-2' FROM hosts WHERE unit = ?2",
            [&second, &unit],
        )
        .unwrap();
    }
    rig.r.start_controller();
    wait_hosts_gone(&rig, "the older incarnation's host to stop");
    assert!(!alive(pid), "the older incarnation's child stopped");
    let orphans = rig.r.ledger_lines(&format!("host_orphan {unit} orphan"));
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    // The newer incarnation's host is gone: the chat is lost and waits for a resume.
    assert_eq!(rig.row().state, "idle");
    assert_eq!(rig.notices("host_lost").len(), 1);
}
