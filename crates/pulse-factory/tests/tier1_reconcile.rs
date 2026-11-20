//! AC-5 of `r2.s2.w3`: tier 1 at controller start (ADR-0007, G5, audit 3 and 12, C16). The real
//! `pulse-factory controller` binary runs a paper run whose seats live in detached session hosts.
//! The controller is killed with `SIGKILL` and started again, and each live seat is survived, lost,
//! unreachable or an orphan. Every test uses its own state and runtime directories, and a guard stops
//! every host under the runtime directory when it ends. Nothing here starts a model or spends a token.

mod common;

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::restart::{Restart, host_bin};
use pf_host::launcher::{Detached, Launcher, StartSpec};
use serde_json::Value;

const LOST: &str = "seat lost across a controller restart";

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

#[test]
fn a_controller_killed_and_started_again_finds_its_seat_survived() {
    let mut rig = Restart::new();
    rig.start_controller();
    rig.arm();
    rig.queue("u1", 1);
    let pid = rig.wait_seat("u1");
    let unit = rig.unit_of_seat("u1");
    rig.wait_attached("u1");
    assert_eq!(rig.unit("u1").unwrap().0, "running");

    rig.kill9();
    assert!(alive(pid), "the seat died with the controller");
    rig.start_controller();

    // The same child, the same host, the unit still running, no request, one `survived` line.
    assert!(
        alive(pid),
        "the seat died: {}\n{}",
        rig.controller_ledger(),
        rig.log_text()
    );
    assert_eq!(rig.seat_pid("u1"), Some(pid));
    let (status, reason) = rig.unit("u1").unwrap();
    assert_eq!((status.as_str(), reason.as_str()), ("running", ""));
    assert_eq!(rig.status()["open_requests"], 0);
    let survived: Vec<String> = rig
        .ledger_lines("host_tier1")
        .into_iter()
        .filter(|l| l.contains(" survived "))
        .collect();
    assert_eq!(survived.len(), 1, "{survived:?}");
    assert!(survived[0].contains(&unit), "{}", survived[0]);
    // One `attached` line for the launch and one for the rebind (SPINE.md Q4: one line per
    // attach), both for this seat's unit, and no `refused` line.
    let attach: Vec<String> = rig
        .ledger_lines("host_attach")
        .into_iter()
        .filter(|l| l.contains(&unit))
        .collect();
    assert_eq!(
        attach.iter().filter(|l| l.contains(" attached ")).count(),
        2,
        "{attach:?}"
    );
    assert!(
        attach.iter().all(|l| !l.contains(" refused ")),
        "{attach:?}"
    );
    assert_eq!(rig.live_hosts().len(), 1);
    let rows = rig.host_rows();
    assert_eq!(rows.len(), 1, "no second host started: {rows:?}");
    assert!(rows[0].1.is_none(), "the row ended: {rows:?}");
    // The seat goes on: it finishes and the unit is done.
    std::fs::write(rig.out("u1").join("go"), "").unwrap();
    rig.wait_unit("u1", "done");
}

/// A running rig with `units` seats, each in a host: `(rig, [(unit, seat pid, host unit)])`.
fn rig_with(units: &[&str]) -> (Restart, Vec<(String, u32, String)>) {
    let mut rig = Restart::new();
    rig.start_controller();
    rig.arm();
    for (i, u) in units.iter().enumerate() {
        rig.queue(u, u32::try_from(i + 1).unwrap());
    }
    let seats = units
        .iter()
        .map(|u| {
            let pid = rig.wait_seat(u);
            let host = rig.unit_of_seat(u);
            rig.wait_attached(u);
            ((*u).to_string(), pid, host)
        })
        .collect();
    (rig, seats)
}

fn tier1_lines(rig: &Restart, outcome: &str) -> Vec<String> {
    rig.ledger_lines("host_tier1")
        .into_iter()
        .filter(|l| l.contains(&format!(" {outcome} ")))
        .collect()
}

/// The state of the row of a host unit: `(ended_at, exit)`.
fn row_of(rig: &Restart, unit: &str) -> (Option<String>, Option<String>) {
    let rows = rig.host_rows();
    let row = rows
        .iter()
        .find(|r| r.0 == unit)
        .unwrap_or_else(|| panic!("no row for {unit}: {rows:?}"));
    (row.1.clone(), row.2.clone())
}

fn exit_file(rig: &Restart, unit: &str) -> PathBuf {
    rig.state
        .join("hosts")
        .join("exits")
        .join(format!("{unit}.json"))
}

#[test]
fn a_seat_whose_child_ended_while_the_controller_was_down_is_survived_and_its_stage_ends() {
    let (mut rig, seats) = rig_with(&["u1"]);
    let (_, pid, unit) = seats[0].clone();
    rig.kill9();
    // The child ends while nobody listens: the host holds its exit.
    std::fs::write(rig.out("u1").join("go"), "").unwrap();
    rig.wait_for("the seat to end", Duration::from_secs(10), || !alive(pid));
    rig.start_controller();

    // Survived, not lost: the stage ends as for any seat whose process ended, with no `seat_lost`.
    rig.wait_unit("u1", "done");
    assert_eq!(rig.status()["open_requests"], 0);
    let survived = tier1_lines(&rig, "survived");
    assert_eq!(survived.len(), 1, "{survived:?}");
    assert!(survived[0].contains(&unit), "{}", survived[0]);
    assert!(tier1_lines(&rig, "lost").is_empty());
    assert!(!rig.controller_ledger().contains("seat_lost"));
}

#[test]
fn a_host_stopped_while_the_controller_was_down_is_lost_with_the_exit_it_left() {
    let (mut rig, seats) = rig_with(&["u1"]);
    let (_, _, unit) = seats[0].clone();
    rig.kill9();
    rig.detached().stop(&unit).unwrap();
    let file = exit_file(&rig, &unit);
    assert!(file.exists(), "a stopped host leaves its exit file");
    let left: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    rig.start_controller();

    let reason = rig.wait_unit("u1", "parked");
    assert!(reason.contains(LOST), "{reason}");
    // The row ended with the file's exit, and the session ended.
    let (ended, exit) = row_of(&rig, &unit);
    assert!(ended.is_some(), "the row is live");
    assert_eq!(serde_json::from_str::<Value>(&exit.unwrap()).unwrap(), left);
    let status = rig.status();
    assert_eq!(status["sessions"][0]["ended"], true, "{status}");
    assert_eq!(tier1_lines(&rig, "lost").len(), 1);
    assert!(rig.live_hosts().is_empty());
}

#[test]
fn a_detached_host_killed_while_the_controller_was_down_is_ended_cause_unknown_and_start_up_goes_on()
 {
    let (mut rig, seats) = rig_with(&["u1"]);
    let (_, _, unit) = seats[0].clone();
    rig.kill9();
    rig.sigkill_host(&unit);
    assert!(
        !exit_file(&rig, &unit).exists(),
        "a killed host leaves no exit file (C16)"
    );
    // `start_controller` waits for the socket: start-up served it after the lost seat.
    rig.start_controller();

    let reason = rig.wait_unit("u1", "parked");
    assert!(reason.contains(LOST), "{reason}");
    let (ended, exit) = row_of(&rig, &unit);
    assert!(ended.is_some());
    let exit: Value = serde_json::from_str(&exit.unwrap()).unwrap();
    assert_eq!(exit["reason"], "unknown", "{exit}");
    assert_eq!(exit["written_by"], "controller");
    assert!(
        exit["note"].as_str().unwrap().contains("left no exit file"),
        "{exit}"
    );
    let lost = tier1_lines(&rig, "lost");
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert!(lost[0].contains("reason=unknown"), "{}", lost[0]);
    assert!(tier1_lines(&rig, "survived").is_empty());
}

/// Make the host of `unit` unreachable: kill it without a trace, then put in its place a socket that
/// accepts and never speaks, and a pid file that names a live process (`holder`). The listener
/// thread keeps every connection open.
fn make_unreachable(rig: &Restart, unit: &str) -> Child {
    rig.sigkill_host(unit);
    let server = rig.server();
    let (session, inc) = pf_host::names::parse_unit(&server, unit).unwrap();
    let dir = rig.hosts_dir(&server).join(format!("{session}-{inc}"));
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
fn a_host_that_never_sends_a_valid_hello_is_unreachable_and_no_second_host_starts() {
    let (mut rig, seats) = rig_with(&["u1"]);
    let (_, _, unit) = seats[0].clone();
    let out = rig.cli(&["settings", "set", "session.attach_wait_ms", "100"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    rig.kill9();
    let mut holder = make_unreachable(&rig, &unit);
    rig.start_controller();

    let reason = rig.wait_unit("u1", "parked");
    assert!(reason.starts_with("host unreachable: "), "{reason}");
    let unreachable = tier1_lines(&rig, "unreachable");
    assert_eq!(unreachable.len(), 1, "{unreachable:?}");
    assert!(unreachable[0].contains(&unit), "{}", unreachable[0]);
    // Nothing was relaunched: the registry holds the one row it held, and no other host runs.
    assert_eq!(rig.host_rows().len(), 1, "{:?}", rig.host_rows());
    assert!(
        rig.live_hosts().iter().all(|h| h.unit == unit),
        "a second host started: {:?}",
        rig.live_hosts()
    );
    let _ = holder.kill();
    let _ = holder.wait();
}

#[test]
fn the_socket_is_not_bound_until_tier_1_has_finished() {
    let (mut rig, seats) = rig_with(&["u1"]);
    let (_, _, unit) = seats[0].clone();
    // The built-in wait is 3 s: tier 1 spends it on the host that never answers.
    rig.kill9();
    let mut holder = make_unreachable(&rig, &unit);
    let started = Instant::now();
    rig.spawn_controller();
    while started.elapsed() < Duration::from_millis(1500) {
        assert!(
            !rig.socket_answers(),
            "the socket answered while tier 1 ran"
        );
        assert!(
            !rig.controller_exited(),
            "the controller exited: {}",
            rig.log_text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The lock is held at once, so a second controller refuses while tier 1 runs.
    let second = rig.cli(&["controller"]);
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already running"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    rig.wait_for("the socket", Duration::from_secs(30), || {
        rig.socket_answers()
    });
    assert!(
        started.elapsed() >= Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(tier1_lines(&rig, "unreachable").len(), 1);
    let _ = holder.kill();
    let _ = holder.wait();
}

/// Start a host of `server` that no row names, from a copy of a live record file.
fn stray_host(rig: &Restart, server: &str, session: &str, from_unit: &str) -> String {
    stray_host_with(rig, server, session, from_unit, None)
}

/// A stray host from a copy of a live record file, with the record body edited first when `edit` is
/// given (the edited body is sealed again, so its hash is its own).
fn stray_host_with(
    rig: &Restart,
    server: &str,
    session: &str,
    from_unit: &str,
    edit: Option<fn(&mut Value)>,
) -> String {
    let own = rig.server();
    let (s, inc) = pf_host::names::parse_unit(&own, from_unit).unwrap();
    let dir = rig.state.join("hosts").join(&own);
    // The record file of a host sits under its own server's name.
    let record_dir = rig.state.join("hosts").join(server);
    std::fs::create_dir_all(&record_dir).unwrap();
    let record = record_dir.join(format!("{session}-1.record.json"));
    let source = dir.join(format!("{s}-{inc}.record.json"));
    match edit {
        None => {
            std::fs::copy(source, &record).unwrap();
        }
        Some(edit) => {
            let mut body: Value =
                serde_json::from_str(&std::fs::read_to_string(source).unwrap()).unwrap();
            edit(&mut body);
            let (canonical, _hash) =
                pf_engine::port::store::NewLaunchRecord::seal(&body.to_string()).unwrap();
            std::fs::write(&record, canonical).unwrap();
        }
    }
    let spec = StartSpec::new(
        server,
        session,
        1,
        record,
        rig.state.clone(),
        host_bin(),
        "ab".repeat(32),
    );
    Detached::new(rig.rt.clone()).start(&spec).unwrap().unit
}

#[test]
fn orphans_are_stopped_or_removed_with_one_line_each_and_another_servers_host_is_left_alone() {
    let (mut rig, seats) = rig_with(&["u1", "u2"]);
    let server = rig.server();
    let (unit1, unit2) = (seats[0].2.clone(), seats[1].2.clone());
    rig.kill9();

    // 1. A host of this server that no row names.
    let no_row = stray_host(&rig, &server, "sorphan00000000a", &unit1);
    // 2. A host whose seat session ended while the controller was down.
    let db = rusqlite::Connection::open(rig.state.join("store.sqlite")).unwrap();
    let ended = db
        .execute("UPDATE sessions SET ended = 1 WHERE unit_id = 'u2'", [])
        .unwrap();
    assert_eq!(ended, 1);
    drop(db);
    // 3. A stale detached directory: a pid file that names a process that is gone.
    let stale = rig.hosts_dir(&server).join("sstale0000000000a-1");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("host.pid"), "999999 1\n").unwrap();
    // 4. A key file that no live row names.
    let key = rig
        .state
        .join("hosts")
        .join(&server)
        .join("sdead00000000000b-1.key");
    std::fs::write(&key, "00".repeat(32)).unwrap();
    // 5. A host of another server in the same runtime directory.
    let other = stray_host(&rig, "aaaaaaaaaaaa", "sother0000000000a", &unit1);

    rig.start_controller();
    rig.wait_for("the orphan lines", Duration::from_secs(30), || {
        rig.ledger_lines("host_orphan").len() >= 4
    });

    let orphans = rig.ledger_lines("host_orphan");
    let count = |needle: &str| orphans.iter().filter(|l| l.contains(needle)).count();
    assert_eq!(count(&no_row), 1, "{orphans:?}");
    assert_eq!(count(&unit2), 1, "{orphans:?}");
    assert_eq!(count("sstale0000000000a"), 1, "{orphans:?}");
    assert_eq!(count("sdead00000000000b"), 1, "{orphans:?}");
    assert_eq!(orphans.len(), 4, "{orphans:?}");
    assert!(
        orphans.iter().all(|l| l.contains(" orphan ")),
        "{orphans:?}"
    );
    // Each is stopped or removed.
    assert!(!stale.exists(), "the stale directory stays");
    assert!(!key.exists(), "the stray key file stays");
    let live: Vec<String> = rig.live_hosts().into_iter().map(|h| h.unit).collect();
    assert!(!live.contains(&no_row), "{live:?}");
    assert!(!live.contains(&unit2), "{live:?}");
    assert!(
        row_of(&rig, &unit2).0.is_some(),
        "the row of the ended session is live"
    );
    // The seat of u1 survived, and the other server's host runs.
    assert!(live.contains(&unit1), "{live:?}");
    let other_state = rig.detached().state(&other);
    assert_eq!(
        other_state,
        pf_host::launcher::HostState::Live,
        "another server's host was stopped"
    );
}

/// An OMP seat's host runs its child on a terminal. The sweep reads a host of this server by its
/// unit name and its row, never by its transport, so a terminal host that no live session row
/// claims is stopped as one orphan (audit 3).
#[test]
fn a_terminal_host_that_no_row_names_is_stopped_by_the_orphan_sweep_with_one_line() {
    let (mut rig, seats) = rig_with(&["u1"]);
    let server = rig.server();
    let unit1 = seats[0].2.clone();
    rig.kill9();

    // A `/bin/sh` stub on a terminal: the shape of an OMP seat.
    let stray = stray_host_with(
        &rig,
        &server,
        "sterminal0000000a",
        &unit1,
        Some(|body: &mut Value| {
            body["argv"] = serde_json::json!(["/bin/sh", "-c", "exec sleep 600"]);
            body["transport"] = serde_json::json!({"kind": "pty", "rows": 24, "cols": 80});
        }),
    );
    assert_eq!(
        rig.detached().state(&stray),
        pf_host::launcher::HostState::Live,
        "the terminal host runs before the controller starts"
    );

    rig.start_controller();
    rig.wait_for("the orphan line", Duration::from_secs(30), || {
        !rig.ledger_lines("host_orphan").is_empty()
    });
    let orphans = rig.ledger_lines("host_orphan");
    assert_eq!(
        orphans.iter().filter(|l| l.contains(&stray)).count(),
        1,
        "{orphans:?}"
    );
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    assert!(orphans[0].contains(" orphan "), "{orphans:?}");
    let live: Vec<String> = rig.live_hosts().into_iter().map(|h| h.unit).collect();
    assert!(!live.contains(&stray), "{live:?}");
    assert!(live.contains(&unit1), "the seat of u1 survived: {live:?}");
    assert_eq!(
        rig.detached().state(&stray),
        pf_host::launcher::HostState::Gone
    );
}
