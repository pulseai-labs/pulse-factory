//! AC-5 and AC-7 of `r2.s2.w4`: an OMP seat runs in a session host on a terminal (ADR-0011's
//! amendment of 2026-11-16: the PTY moves into the host; ADR-0001: every session in its own host).
//! The adapter asks a test host service (`common::hosts::TestHosts`, over `pf_host`'s detached
//! launcher) for a host, drives the stub `omp` through the host's input and output, and reads its
//! events from the event file. No test starts OMP or a model.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::hosts::TestHosts;
use common::*;
use pf_adapter_omp::{OmpAdapter, session_dir_of};
use pf_engine::port::agent::{AgentError, AgentPort, SessionRef, SessionState};
use pf_engine::port::host::SessionHosts;
use pf_host::launcher::HostState;
use serde_json::Value;

/// The stub that reports what it sees, starts a turn, and waits.
const STUB_REPORTS: &str = r#"echo $$ > "$DIR/pid"
env > "$DIR/env"
if [ -t 0 ]; then echo tty > "$DIR/tty"; fi
stty size > "$DIR/size" 2>/dev/null
printf '%s\n' "$@" > "$DIR/argv"
printf '%s\n' "$PF_SEAT_TOKEN" > "$DIR/token"
echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
exec sleep 600"#;

/// The processes of a group that are live: not a zombie and not dead.
fn group_live(pgid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(") ") else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or("");
        let _ppid = fields.next();
        let pgrp: u32 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        if pgrp == pgid && state != "Z" && state != "X" {
            out.push(pid);
        }
    }
    out
}

fn wait_for(what: &str, within: Duration, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{what} did not happen within {within:?}");
}

fn read_pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn env_of(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// An adapter with a terminal of its own size over `hosts`.
fn sized_adapter(dir: &Path, omp: std::path::PathBuf, hosts: Arc<TestHosts>) -> OmpAdapter {
    let mut cfg = config(dir, omp);
    cfg.rows = 31;
    cfg.cols = 97;
    OmpAdapter::new(cfg, hosts)
}

fn one_launch(dir: &Path) -> (OmpAdapter, Arc<TestHosts>, SessionRef) {
    let hosts = TestHosts::owned();
    let a = sized_adapter(dir, stub_omp(dir, "18.11.3", STUB_REPORTS), hosts.clone());
    let s = a
        .launch(&plan(
            dir,
            &[
                ("env.PF_SESSION_KEY", "seat-session-key-7f3a9c"),
                ("env.MY_NAME", "my-value"),
            ],
        ))
        .unwrap();
    (a, hosts, s)
}

#[test]
fn a_launch_starts_the_stub_in_a_host_on_a_terminal_of_the_configured_size() {
    let dir = tempfile::tempdir().unwrap();
    let (a, hosts, s) = one_launch(dir.path());
    let unit = s.0.clone();
    assert!(hosts.is_live(&unit), "the host runs");
    assert_eq!(hosts.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    // The session reference is the host's unit.
    assert!(unit.starts_with("pf-host-tsrv000000ab-"), "{unit}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tty"))
            .unwrap()
            .trim(),
        "tty"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("size"))
            .unwrap()
            .trim(),
        "31 97"
    );
    let record = hosts.launched_record(&unit).unwrap().unwrap();
    assert_eq!(
        record["transport"],
        serde_json::json!({"kind": "pty", "rows": 31, "cols": 97})
    );
    assert_eq!(
        record["argv"][0],
        dir.path().join("omp").display().to_string()
    );
    assert_eq!(a.is_alive(&s), Ok(true));
    a.stop(&s).unwrap();
}

#[test]
fn the_child_environment_is_the_listed_names_the_plan_and_the_adapters_own_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _hosts, s) = one_launch(dir.path());
    let env = env_of(&dir.path().join("env"));

    // A listed name that the test sets reaches the child with the test's value.
    assert_eq!(
        env.get("PATH").map(String::as_str),
        std::env::var("PATH").ok().as_deref()
    );
    // The plan's `env.*` setting, and the adapter's own names.
    assert_eq!(env.get("MY_NAME").map(String::as_str), Some("my-value"));
    assert_eq!(env["PI_EDIT_VARIANT"], "replace");
    assert_eq!(env["PF_SEAT_ROLE"], "implementer");
    assert_eq!(env["PF_ALLOWED_TOOLS"], "read,bash,edit,write,grep,glob");
    let unit = &s.0;
    let session_dir = dir.path().join("state").join("sessions").join(unit);
    assert_eq!(
        env["PF_EVENT_FILE"],
        session_dir.join("events.jsonl").display().to_string()
    );
    assert_eq!(
        env["PF_INBOX_FILE"],
        session_dir.join("inbox.jsonl").display().to_string()
    );
    assert_eq!(env["PF_SESSION_DIR"], session_dir.display().to_string());
    // The two secrets reach the child in plain text.
    assert_eq!(env["PF_SESSION_KEY"], "seat-session-key-7f3a9c");
    let token = env["PF_SEAT_TOKEN"].clone();
    assert_eq!(token.len(), 32);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()), "{token}");

    // A variable that only the test sets does not reach it: the host clears the environment.
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "cargo sets this variable for a test run"
    );
    assert!(!env.contains_key("CARGO_MANIFEST_DIR"), "{env:?}");
    // And no name outside the lists: the 24 inherited names, the plan's, the adapter's, the secrets,
    // and what the shell adds for itself.
    let allowed: BTreeSet<&str> = [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "TZ",
        "TMPDIR",
        "PF_STATE_DIR",
        "PF_SHARE_DIR",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_TERMINAL_PROMPT",
        "TERM",
        "COLORTERM",
        "SHELL",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "SSH_AUTH_SOCK",
        "MY_NAME",
        "PI_EDIT_VARIANT",
        "PF_EVENT_FILE",
        "PF_INBOX_FILE",
        "PF_SESSION_DIR",
        "PF_SEAT_ROLE",
        "PF_ALLOWED_TOOLS",
        "PF_SESSION_KEY",
        "PF_SEAT_TOKEN",
        // The shell sets these for itself.
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
    ]
    .into_iter()
    .collect();
    let stray: Vec<&String> = env
        .keys()
        .filter(|k| !allowed.contains(k.as_str()))
        .collect();
    assert!(stray.is_empty(), "variables outside the lists: {stray:?}");
    a.stop(&s).unwrap();
}

#[test]
fn the_record_holds_only_hashes_of_the_secrets_and_the_shape_the_seat_key_rebuild_reads() {
    let dir = tempfile::tempdir().unwrap();
    let (a, hosts, s) = one_launch(dir.path());
    let unit = &s.0;
    let env = env_of(&dir.path().join("env"));
    let token = &env["PF_SEAT_TOKEN"];
    let record = hosts.launched_record(unit).unwrap().unwrap();
    let text = record.to_string();
    for plain in [token.as_str(), "seat-session-key-7f3a9c"] {
        assert!(
            !text.contains(plain),
            "the record holds a secret in plain text"
        );
    }
    let hash = |v: &str| format!("sha256:{}", pf_engine::claim::sha256_hex(v.as_bytes()));
    assert_eq!(
        record["env"]["PF_SESSION_KEY"],
        hash("seat-session-key-7f3a9c")
    );
    assert_eq!(record["env"]["PF_SEAT_TOKEN"], hash(token));
    let names: BTreeSet<&str> = record["secret_names"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(names, BTreeSet::from(["PF_SEAT_TOKEN", "PF_SESSION_KEY"]));
    // The settings hold the plan's, with the session key as a hash.
    assert_eq!(
        record["settings"]["env.PF_SESSION_KEY"],
        hash("seat-session-key-7f3a9c")
    );
    assert_eq!(record["settings"]["env.MY_NAME"], "my-value");

    // What `SeatPort::rebuild` reads.
    let binding = &record["run_binding"];
    assert_eq!(binding["run_id"], "run-1");
    assert_eq!(binding["unit_id"], "u1");
    assert_eq!(binding["stage"], "implement");
    assert_eq!(binding["role"], "implementer");
    assert_eq!(binding["model"], "zai/glm-5.3-flash");
    assert_eq!(binding["attempt"], 1);
    assert_eq!(record["runtime"]["id"], "omp");
    assert_eq!(record["resume"], serde_json::json!({"kind": "none"}));

    // The runtime: the installed version, the absolute binary and its hash.
    let omp = dir.path().join("omp");
    assert_eq!(record["runtime"]["version"], "18.11.3");
    assert_eq!(record["runtime"]["binary_path"], omp.display().to_string());
    assert!(Path::new(record["runtime"]["binary_path"].as_str().unwrap()).is_absolute());
    assert_eq!(
        record["runtime"]["sha256"],
        pf_engine::claim::sha256_hex(&std::fs::read(&omp).unwrap())
    );
    a.stop(&s).unwrap();
}

#[test]
fn the_argv_and_the_files_hold_the_unit_and_never_the_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let (a, hosts, s) = one_launch(dir.path());
    let unit = &s.0;
    let record = hosts.launched_record(unit).unwrap().unwrap();
    assert!(!record.to_string().contains("{unit}"), "{record}");
    let argv: Vec<String> = record["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let session_dir = dir.path().join("state").join("sessions").join(unit);
    let at = argv.iter().position(|w| w == "--session-dir").unwrap();
    assert_eq!(
        argv[at + 1],
        session_dir.join("omp-sessions").display().to_string()
    );
    // The service made both files before the stub started.
    assert!(session_dir.join("events.jsonl").is_file());
    assert!(session_dir.join("inbox.jsonl").is_file());
    assert_eq!(a.event_file(&s), Some(session_dir.join("events.jsonl")));
    assert_eq!(a.inbox_file(&s), Some(session_dir.join("inbox.jsonl")));
    assert_eq!(session_dir_of(&a, &s), Some(session_dir));
    // The launch sent the template, and the service filled it.
    let sent = hosts.launches.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].files.iter().all(|f| f.contains("{unit}")),
        "{:?}",
        sent[0].files
    );
    assert_eq!(sent[0].session_kind, "seat");
    assert_eq!(sent[0].session_key, "run-1/u1");
    drop(sent);
    a.stop(&s).unwrap();
}

#[test]
fn the_first_position_is_saved_before_launch_returns() {
    let dir = tempfile::tempdir().unwrap();
    let (a, hosts, s) = one_launch(dir.path());
    let (seq, position) = hosts.stored(&s.0).expect("a position was saved");
    assert_eq!(seq, 1);
    // The two lines the stub wrote are read: the offset is past them.
    let events = std::fs::read(a.event_file(&s).unwrap()).unwrap();
    assert_eq!(position["offset"].as_u64(), Some(events.len() as u64));
    assert_eq!(position["first_turn_started"], true);
    assert_eq!(position["pending"], serde_json::json!([]));
    assert_eq!(position["push_seq"], 0);
    // No push directory, so no baseline.
    assert!(position["baseline"].is_null());
    a.stop(&s).unwrap();
}

#[test]
fn a_failed_first_save_stops_the_host_and_fails_the_launch() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = TestHosts::owned();
    hosts
        .fail_saves
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let a = sized_adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_REPORTS),
        hosts.clone(),
    );
    let err = a.launch(&plan(dir.path(), &[])).unwrap_err();
    let AgentError::Failed(why) = err else {
        panic!("expected Failed")
    };
    assert!(why.contains("position"), "{why}");
    let unit = hosts.units.lock().unwrap()[0].clone();
    assert_eq!(
        hosts.state_of(&unit),
        HostState::Gone,
        "the host was stopped"
    );
    let pid = read_pid(&dir.path().join("pid"));
    assert!(
        group_live(pid).is_empty(),
        "no process of the stub's group lives"
    );
}

#[test]
fn an_early_exit_and_a_first_turn_timeout_each_fail_the_launch_with_the_screen_and_leave_nothing() {
    // An early exit.
    let dir = tempfile::tempdir().unwrap();
    let hosts = TestHosts::owned();
    let a = sized_adapter(
        dir.path(),
        stub_omp(
            dir.path(),
            "18.11.3",
            "echo $$ > \"$DIR/pid\"\necho \"fatal: no API key $PF_SEAT_TOKEN\"\nexit 3",
        ),
        hosts.clone(),
    );
    // The first-turn deadline is long (30 s), so a launch that notices the exit only at the
    // deadline cannot pass the time bound below, though its reason would still read "exited".
    let started = Instant::now();
    let err = a
        .launch(&plan(dir.path(), &[("first_turn_seconds", "30")]))
        .unwrap_err();
    let elapsed = started.elapsed();
    let AgentError::Failed(why) = err else {
        panic!("expected Failed")
    };
    assert!(
        elapsed < Duration::from_secs(15),
        "an early exit fails the launch at once, not at the first-turn deadline: {elapsed:?}"
    );
    assert!(why.contains("exited with 3"), "{why}");
    assert!(
        why.contains("no API key"),
        "the screen is in the reason: {why}"
    );
    // The seat's own token, which the stub printed, is masked in the reason.
    assert!(
        why.split_whitespace()
            .all(|w| !(w.len() == 32 && w.bytes().all(|b| b.is_ascii_hexdigit()))),
        "a 32-digit token is in the reason: {why}"
    );
    let unit = hosts.units.lock().unwrap()[0].clone();
    assert_eq!(hosts.state_of(&unit), HostState::Gone, "no host is left");
    assert!(group_live(read_pid(&dir.path().join("pid"))).is_empty());

    // A first-turn timeout.
    let dir = tempfile::tempdir().unwrap();
    let hosts = TestHosts::owned();
    let a = sized_adapter(
        dir.path(),
        stub_omp(
            dir.path(),
            "18.11.3",
            "echo $$ > \"$DIR/pid\"\n(trap '' HUP; exec sleep 600) &\necho 'Loading...'\nwait",
        ),
        hosts.clone(),
    );
    let err = a
        .launch(&plan(dir.path(), &[("first_turn_seconds", "1")]))
        .unwrap_err();
    let AgentError::Failed(why) = err else {
        panic!("expected Failed")
    };
    assert!(why.contains("no first turn in 1 s"), "{why}");
    assert!(
        why.contains("Loading"),
        "the screen is in the reason: {why}"
    );
    let unit = hosts.units.lock().unwrap()[0].clone();
    assert_eq!(hosts.state_of(&unit), HostState::Gone, "no host is left");
    assert!(
        group_live(read_pid(&dir.path().join("pid"))).is_empty(),
        "no process of the stub's group lives"
    );
}

#[test]
fn interrupt_reaches_the_stub_as_escape_and_process_id_is_the_stubs_pid() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"echo $$ > "$DIR/pid"
echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
stty raw -echo
dd bs=1 count=1 2>/dev/null | od -An -tx1 > "$DIR/key"
exec sleep 600"#;
    let hosts = TestHosts::owned();
    let a = sized_adapter(dir.path(), stub_omp(dir.path(), "18.11.3", body), hosts);
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    let pid = read_pid(&dir.path().join("pid"));
    assert_eq!(a.process_id(&s), Some(pid));
    a.interrupt(&s).unwrap();
    wait_for(
        "the stub's read of one key",
        Duration::from_secs(10),
        || std::fs::read_to_string(dir.path().join("key")).is_ok_and(|t| t.contains("1b")),
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("key"))
            .unwrap()
            .trim(),
        "1b"
    );
    a.stop(&s).unwrap();
    assert_eq!(a.process_id(&s), None, "a stopped seat has no process");
}

#[test]
fn write_terminal_sends_its_bytes_to_the_stub() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
stty raw -echo
dd bs=1 count=3 2>/dev/null > "$DIR/typed"
exec sleep 600"#;
    let a = adapter(dir.path(), stub_omp(dir.path(), "18.11.3", body));
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    a.write_terminal(&s, b"abc").unwrap();
    wait_for("the typed bytes", Duration::from_secs(10), || {
        std::fs::read(dir.path().join("typed")).is_ok_and(|t| t == b"abc")
    });
    a.stop(&s).unwrap();
}

#[test]
fn is_alive_is_false_after_the_stub_exits_and_a_drain_then_ends_the_host_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
while [ ! -e "$DIR/go" ]; do sleep 0.05; done
echo '{"ev":"agent_end","t":3}' >> "$PF_EVENT_FILE"
exit 0"#;
    let hosts = TestHosts::owned();
    let a = sized_adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", body),
        hosts.clone(),
    );
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    let unit = s.0.clone();
    assert_eq!(a.is_alive(&s), Ok(true));
    std::fs::write(dir.path().join("go"), b"").unwrap();
    wait_for("the stub's exit", Duration::from_secs(10), || {
        a.is_alive(&s) == Ok(false)
    });
    assert_eq!(a.status(&s).unwrap().state, SessionState::Ended);
    // The events after the exit are read, and when nothing is pending the adapter sends the exit
    // acknowledge: the host ends at once, not after the hold (30 s here).
    let started = Instant::now();
    loop {
        a.drain_events(&s).unwrap();
        if hosts.state_of(&unit) == HostState::Gone {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the host still runs after the exit and a drain"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(a.is_alive(&s), Ok(false));
    // The last turn end was read.
    assert!(a.status(&s).unwrap().turn_ended);
}

#[test]
fn hand_off_stops_the_old_host_before_the_new_one_starts() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = TestHosts::owned();
    let a = sized_adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_REPORTS),
        hosts.clone(),
    );
    let first = a.launch(&plan(dir.path(), &[])).unwrap();
    let mut successor = plan(dir.path(), &[]);
    successor.attempt = 2;
    let second = a.hand_off(&first, &successor).unwrap();
    assert_ne!(first, second);
    let log = hosts.log();
    let position = |needle: String| log.iter().position(|l| *l == needle).unwrap_or(usize::MAX);
    let stop_old = position(format!("stop {}", first.0));
    let start_new = position(format!("launch {}", second.0));
    assert!(stop_old < start_new, "stop the old host first: {log:?}");
    assert_eq!(a.is_alive(&first), Ok(false));
    assert_eq!(a.is_alive(&second), Ok(true));
    assert!(!hosts.is_live(&first.0));
    a.stop(&second).unwrap();
}

#[test]
fn a_session_the_adapter_does_not_know_is_stopped_and_not_alive() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(dir.path(), stub_omp(dir.path(), "18.11.3", STUB_REPORTS));
    let nope = SessionRef("pf-host-tsrv000000ab-snone-1".to_string());
    assert_eq!(a.stop(&nope), Ok(()));
    assert_eq!(a.is_alive(&nope), Ok(false));
    assert_eq!(a.process_id(&nope), None);
}

/// The stub of the moved stop test: a background child that ignores `SIGHUP`.
const STUB_BACKGROUND_CHILD: &str = r#"env > "$DIR/env"
(trap '' HUP; exec sleep 600) &
echo $! > "$DIR/child.pid"
echo $$ > "$DIR/pid"
echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
wait"#;

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

/// Moved from `seat_facts.rs` (the flake of PR #74: it polled the child for 2.5 s after a `SIGKILL`
/// that it did not wait for). `stop` now returns only when the group is gone, so the test asserts
/// once, with no polling, and runs ten times in one test.
#[test]
fn omp_sets_the_edit_variant_and_stop_leaves_no_child_alive() {
    for round in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        let a = adapter(
            dir.path(),
            stub_omp(dir.path(), "18.11.3", STUB_BACKGROUND_CHILD),
        );
        let s = a.launch(&plan(dir.path(), &[])).unwrap();
        let env = std::fs::read_to_string(dir.path().join("env")).unwrap();
        assert!(
            env.lines().any(|l| l == "PI_EDIT_VARIANT=replace"),
            "round {round}: {env}"
        );
        let child = read_pid(&dir.path().join("child.pid"));
        let leader = read_pid(&dir.path().join("pid"));
        assert!(alive(child), "round {round}: the stub's child runs");
        assert!(
            !group_live(leader).is_empty(),
            "round {round}: the group runs"
        );
        a.stop(&s).unwrap();
        // No polling: the group is gone when `stop` returns.
        assert!(
            !alive(child),
            "round {round}: stop must leave no child of the stub alive"
        );
        assert_eq!(
            group_live(leader),
            Vec::<u32>::new(),
            "round {round}: a process of the group is live after stop"
        );
        assert_eq!(a.stop(&s), Ok(()), "round {round}: a second stop is Ok");
        assert_eq!(a.is_alive(&s), Ok(false));
    }
}
