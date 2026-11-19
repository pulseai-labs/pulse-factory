//! AC-19 of `r2.s2.w3`, and the run that CI's `systemd live` step and `scripts/check-hosts-systemd.sh`
//! make with `--ignored` (AC-23, AC-24; SPINE.md Q7, audit 13): the real `pulse-factory controller`
//! with `PF_HOST_LAUNCHER=systemd`, run as a transient systemd user unit, and a paper seat in a
//! session host that is a unit of its own. Every test is `#[ignore]`d and named `systemd_live_*`,
//! because a seat inside `seat-fence` reaches no user manager. Without `--ignored` this file only has
//! to compile, and its one executed test (`the_live_tests_stay_ignored_by_default`) keeps the run
//! from reading as a vacuous green.
//!
//! The host server name of every test is `c0ffee` and six more hex digits, written into the state
//! directory before the controller starts. So every host unit of a test is named
//! `pf-host-c0ffee<six hex>-<session>-<n>`, in the slice `pf-hosts-c0ffee<six hex>.slice`, and
//! `scripts/check-hosts-systemd.sh` finds the units that a test left behind by that prefix. The
//! controller itself is the unit `pfc-c0ffee<six hex>`. Each test stops every unit it started when it
//! ends, pass or fail.
//!
//! The seat is the paper seat of the restart tests (`tests/restart_support/paper`). It stays alive
//! until the test writes `go`.

mod common;

use std::os::unix::fs::DirBuilderExt;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::BIN;
use common::restart::{Restart, host_bin};
use pf_host::launcher::{Launcher, StartSpec, Systemd};
use serde_json::{Value, json};

const REASON_LOST: &str = "seat lost across a controller restart";
const KEY: &str = "5f1d0c9a7b3e4d2c8a6f1e0d9c7b5a3948372615f4e3d2c1b0a99887766554a3";

static N: AtomicU32 = AtomicU32::new(0);

/// A host server name for one test: `c0ffee` and six hex digits of the process and the test count.
fn test_server() -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    format!(
        "c0ffee{:06x}",
        (std::process::id().wrapping_mul(131) + n) & 0x00ff_ffff
    )
}

/// `systemctl --user <args>`: whether it succeeded, and its standard output.
fn sctl(args: &[&str]) -> (bool, String) {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .expect("systemctl runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn property(unit: &str, name: &str) -> String {
    sctl(&["show", "-p", name, "--value", &format!("{unit}.service")])
        .1
        .trim()
        .to_string()
}

/// The `.service` units of a name pattern that exist in the user manager, loaded or failed.
fn units(pattern: &str) -> Vec<String> {
    sctl(&[
        "list-units",
        "--all",
        "--plain",
        "--no-legend",
        "--no-pager",
        pattern,
    ])
    .1
    .lines()
    .filter_map(|l| l.split_whitespace().next())
    .filter(|u| u.ends_with(".service"))
    .map(str::to_string)
    .collect()
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

fn call(command: &str) -> Value {
    json!({ "role": "x", "tool": "bash", "input": { "command": command } })
}

/// One test's setup: a rig (state, share, paper package and its CLI), a test server name, the
/// controller as a transient unit, and the units to stop at the end.
struct Live {
    rig: Restart,
    server: String,
    /// The controller's unit, without `.service`.
    ctl: String,
    /// Host units a test started itself (a stray host, a host of another server).
    extra: Vec<String>,
}

impl Live {
    fn new() -> Live {
        // The rig's own runtime directory serves its CLI only. The controller gets the user's.
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .expect(
                "XDG_RUNTIME_DIR must name the user's runtime directory (a user manager must run)",
            );
        let rig = Restart::new();
        let server = test_server();
        let dir = rig.state.join("hosts");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .unwrap();
        std::fs::write(dir.join("server"), format!("{server}\n")).unwrap();
        let ctl = format!("pfc-{server}");
        Live {
            rig,
            server,
            ctl,
            extra: Vec::new(),
        }
    }

    /// Start the controller as a transient unit with `KillMode=control-group` (the shape of the
    /// installed unit), and wait until its socket answers.
    fn start_controller(&mut self) {
        let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap();
        let log = self.rig.log.display().to_string();
        let mut cmd = Command::new("systemd-run");
        cmd.args(["--user", "--collect", "--quiet"])
            .arg(format!("--unit={}", self.ctl))
            .args(["-p", "KillMode=control-group"])
            .args(["-p", &format!("StandardOutput=append:{log}")])
            .args(["-p", &format!("StandardError=append:{log}")]);
        let env = [
            ("PATH", std::env::var("PATH").unwrap_or_default()),
            ("HOME", self.rig.root.display().to_string()),
            ("PF_STATE_DIR", self.rig.state.display().to_string()),
            ("PF_SHARE_DIR", self.rig.share.display().to_string()),
            ("XDG_RUNTIME_DIR", xdg),
            ("PF_HOST_BIN", host_bin().display().to_string()),
            ("PF_HOST_LAUNCHER", "systemd".to_string()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".to_string()),
            ("GIT_CONFIG_NOSYSTEM", "1".to_string()),
            ("GIT_TERMINAL_PROMPT", "0".to_string()),
        ];
        for (k, v) in env {
            cmd.args(["-E", &format!("{k}={v}")]);
        }
        cmd.args(["--", BIN, "controller", "--tick-ms", "100"]);
        let out = cmd.output().expect("systemd-run runs");
        assert!(
            out.status.success(),
            "systemd-run of the controller failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        self.wait_socket();
    }

    fn wait_socket(&self) {
        self.rig
            .wait_for("the controller socket", Duration::from_secs(90), || {
                self.rig.socket_answers()
            });
    }

    /// `systemctl --user restart` of the controller's unit, then wait for its socket again.
    fn restart_controller(&self) {
        let (ok, _) = sctl(&["restart", &format!("{}.service", self.ctl)]);
        assert!(ok, "systemctl restart of {} failed", self.ctl);
        self.wait_socket();
    }

    fn stop_controller(&self) {
        let _ = sctl(&["stop", &format!("{}.service", self.ctl)]);
        self.rig.wait_for(
            "the controller socket to close",
            Duration::from_secs(30),
            || !self.rig.socket_answers(),
        );
    }

    /// The host units of this test's server.
    fn host_units(&self) -> Vec<String> {
        units(&format!("pf-host-{}-*", self.server))
    }

    /// Arm a paper run, queue `u1` and wait until its seat runs and its session names its host.
    fn seat(&mut self) -> (u32, String) {
        self.rig.arm();
        self.rig.queue("u1", 1);
        let pid = self.rig.wait_seat("u1");
        let unit = self.rig.unit_of_seat("u1");
        self.rig.wait_attached("u1");
        (pid, unit)
    }

    /// A host of `server` that no row names, as a unit of its own, from a minimal record.
    fn stray_host(&mut self, server: &str, session: &str) -> String {
        let dir = self.rig.state.join("hosts").join(server);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .unwrap();
        let record = dir.join(format!("{session}-1.record.json"));
        std::fs::write(
            &record,
            json!({
                "argv": ["/bin/sh", "-c", "sleep 600"],
                "cwd": "/",
                "env": { "PATH": "/usr/bin:/bin" },
                "transport": { "kind": "pipes" },
            })
            .to_string(),
        )
        .unwrap();
        let spec = StartSpec::new(
            server,
            session,
            1,
            record,
            self.rig.state.clone(),
            host_bin(),
            KEY.to_string(),
        );
        let started = Systemd::from_env()
            .expect("XDG_RUNTIME_DIR names the user's runtime directory")
            .start(&spec)
            .unwrap_or_else(|e| panic!("the stray unit did not start: {e}"));
        self.extra.push(started.unit.clone());
        started.unit
    }

    fn host_row_exit(&self, unit: &str) -> Option<String> {
        self.rig
            .host_rows()
            .into_iter()
            .find(|r| r.0 == unit)
            .and_then(|r| r.2)
    }

    fn invocation_in_row(&self, unit: &str) -> String {
        let db = rusqlite::Connection::open_with_flags(
            self.rig.state.join("store.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.query_row(
            "SELECT invocation_id FROM hosts WHERE unit = ?1",
            [unit],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    }
}

impl Drop for Live {
    /// The controller's unit, then every host unit of the test's server, then the extras.
    fn drop(&mut self) {
        let _ = sctl(&["stop", &format!("{}.service", self.ctl)]);
        let launcher = Systemd::from_env().ok();
        for unit in units(&format!("pf-host-{}-*", self.server)) {
            let name = unit.trim_end_matches(".service").to_string();
            match &launcher {
                Some(l) => {
                    let _ = l.stop(&name);
                }
                None => {
                    let _ = sctl(&["stop", &unit]);
                }
            }
        }
        if let Some(l) = &launcher {
            for unit in &self.extra {
                let _ = l.stop(unit);
            }
        }
        let _ = sctl(&["reset-failed", &format!("{}.service", self.ctl)]);
    }
}

/// The one test of this file that runs by default. It holds the file's own promise: every
/// `systemd_live_*` test is `#[ignore]`d, so a seat in the fence passes this file. Without it, a run
/// without `--ignored` executes no test at all, which `oss verify_step` reads as a vacuous green.
#[test]
fn the_live_tests_stay_ignored_by_default() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/systemd_live_hosts.rs"
    ))
    .unwrap();
    let lines: Vec<&str> = source.lines().collect();
    let mut live = 0;
    for (i, line) in lines.iter().enumerate() {
        let Some(name) = line.strip_prefix("fn systemd_live_") else {
            continue;
        };
        live += 1;
        let marked = lines[i.saturating_sub(3)..i]
            .iter()
            .any(|l| l.trim_start().starts_with("#[ignore"));
        assert!(
            marked,
            "systemd_live_{} is not #[ignore]d",
            name.split('(').next().unwrap_or("")
        );
    }
    assert!(live >= 5, "found only {live} live tests");
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_a_paper_seat_runs_in_a_unit_of_its_own_in_the_servers_slice() {
    let mut live = Live::new();
    live.start_controller();
    let (pid, unit) = live.seat();

    assert!(
        unit.starts_with(&format!("pf-host-{}-", live.server)),
        "the unit {unit} does not carry the test server name {}",
        live.server
    );
    assert_eq!(
        property(&unit, "Slice"),
        format!("pf-hosts-{}.slice", live.server),
        "the host unit is not in its server's slice"
    );
    // The seat's process is in the host's cgroup and not in the controller's.
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(
        cgroup.contains(&format!("{unit}.service")),
        "the seat is not in the host's cgroup: {cgroup}"
    );
    assert!(
        !cgroup.contains(&live.ctl),
        "the seat is in the controller's cgroup: {cgroup}"
    );
    assert!(
        live.host_units().contains(&format!("{unit}.service")),
        "the user manager lists no unit {unit}: {:?}",
        live.host_units()
    );
    // The row holds the unit's invocation ID.
    assert_eq!(
        live.invocation_in_row(&unit),
        property(&unit, "InvocationID")
    );
    std::fs::write(live.rig.out("u1").join("go"), "").unwrap();
    live.rig.wait_unit("u1", "done");
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_a_restart_of_the_controller_keeps_the_seat_and_rebinds_it() {
    let mut live = Live::new();
    live.start_controller();
    let (pid, unit) = live.seat();
    let invocation = property(&unit, "InvocationID");
    assert!(!invocation.is_empty());
    assert_eq!(
        live.rig.hook("u1", &call("ls -la"))["decision"],
        "allow",
        "before the restart"
    );

    live.restart_controller();

    // The same child in the same host unit: the controller's stop ended neither.
    assert!(alive(pid), "the seat died with the controller");
    assert_eq!(live.rig.seat_pid("u1"), Some(pid));
    assert_eq!(
        property(&unit, "InvocationID"),
        invocation,
        "the host unit was started again"
    );
    // The new controller attached again: the invocation ID matches the row and `peer_check`
    // accepted the unit's cgroup, so the attach was not refused.
    assert_eq!(live.invocation_in_row(&unit), invocation);
    let attach = live.rig.ledger_lines("host_attach");
    assert_eq!(
        attach.iter().filter(|l| l.contains(" attached ")).count(),
        2,
        "{attach:?}"
    );
    assert!(
        attach.iter().all(|l| !l.contains(" refused ")),
        "{attach:?}"
    );
    let survived: Vec<String> = live
        .rig
        .ledger_lines("host_tier1")
        .into_iter()
        .filter(|l| l.contains(" survived "))
        .collect();
    assert_eq!(survived.len(), 1, "{survived:?}");
    // The unit still runs, and the seat's hook call is answered by its rules.
    assert_eq!(live.rig.unit("u1").unwrap().0, "running");
    let allow = live.rig.hook("u1", &call("ls -la"));
    assert_eq!(allow["decision"], "allow", "{allow}");
    let deny = live.rig.hook("u1", &call("git push origin main"));
    assert_eq!(deny["decision"], "deny", "{deny}");
    assert_eq!(live.host_units().len(), 1, "{:?}", live.host_units());
    std::fs::write(live.rig.out("u1").join("go"), "").unwrap();
    live.rig.wait_unit("u1", "done");
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_a_kill_of_the_whole_host_unit_leaves_no_exit_file_and_parks_the_seat() {
    // C16 against a real manager: `systemctl kill` has `--kill-whom=all` by default, so it kills the
    // `ExecStopPost=` step too, and no exit file is written. A gone unit with no exit file reads as
    // "ended, cause unknown", never as survived.
    let mut live = Live::new();
    live.start_controller();
    let (_pid, unit) = live.seat();
    live.stop_controller();

    let (ok, _) = sctl(&["kill", "-s", "KILL", &format!("{unit}.service")]);
    assert!(ok, "systemctl kill of {unit} failed");
    live.rig
        .wait_for("the host unit to end", Duration::from_secs(30), || {
            !live.host_units().contains(&format!("{unit}.service"))
        });
    let exit_file = pf_host::names::exit_path(&live.rig.state, &unit);
    assert!(
        !exit_file.exists(),
        "an exit file exists: {}",
        exit_file.display()
    );

    live.start_controller();
    let reason = live.rig.wait_unit("u1", "parked");
    assert!(reason.contains(REASON_LOST), "{reason}");
    let lost: Vec<String> = live
        .rig
        .ledger_lines("host_tier1")
        .into_iter()
        .filter(|l| l.contains(" lost ") && l.contains(&unit))
        .collect();
    assert_eq!(lost.len(), 1, "{:?}", live.rig.ledger_lines("host_tier1"));
    let exit = live
        .host_row_exit(&unit)
        .expect("the row ended with an exit");
    assert!(exit.contains("left no exit file"), "{exit}");
    // Start-up went on: the socket serves, and nothing was relaunched.
    assert_eq!(live.rig.host_rows().len(), 1, "{:?}", live.rig.host_rows());
    assert!(live.host_units().is_empty(), "{:?}", live.host_units());
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_a_host_unit_with_no_row_is_stopped_as_an_orphan() {
    let mut live = Live::new();
    // A first start makes the state directory and the store. Then the controller stops, a host of
    // this server starts that no row names, and the controller starts again: its sweep stops it.
    live.start_controller();
    live.stop_controller();
    let server = live.server.clone();
    let stray = live.stray_host(&server, "sorphan00000000a");
    assert!(
        live.host_units().contains(&format!("{stray}.service")),
        "the stray unit {stray} is not listed: {:?}",
        live.host_units()
    );

    live.start_controller();
    live.rig
        .wait_for("the orphan line", Duration::from_secs(60), || {
            live.rig
                .ledger_lines("host_orphan")
                .iter()
                .any(|l| l.contains(&stray))
        });
    let lines: Vec<String> = live
        .rig
        .ledger_lines("host_orphan")
        .into_iter()
        .filter(|l| l.contains(&stray))
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    live.rig
        .wait_for("the orphan unit to stop", Duration::from_secs(30), || {
            !live.host_units().contains(&format!("{stray}.service"))
        });
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_every_unit_carries_the_test_server_name_and_another_servers_unit_is_left_alone() {
    let mut live = Live::new();
    assert!(live.server.starts_with("c0ffee"), "{}", live.server);
    live.start_controller();
    let (_pid, unit) = live.seat();
    // The name in the state directory is the one the test wrote, and the controller kept it.
    let name = pulse_factory::controller::hosts::read_host_server_name(&live.rig.state)
        .unwrap()
        .unwrap();
    assert_eq!(name, live.server);
    for u in live.host_units() {
        assert!(
            u.starts_with(&format!("pf-host-{}-", live.server)),
            "a unit of another name: {u}"
        );
    }
    // A host of another server, started while the controller runs, survives the controller's next
    // start: the sweep never lists another server's hosts (G9).
    let other_server = format!("c0ffed{}", &live.server[6..]);
    let other = live.stray_host(&other_server, "sother0000000000a");
    live.restart_controller();
    assert!(
        units(&format!("pf-host-{other_server}-*")).contains(&format!("{other}.service")),
        "the sweep stopped a unit of another server"
    );
    assert!(
        live.rig
            .ledger_lines("host_orphan")
            .iter()
            .all(|l| !l.contains(&other)),
        "{:?}",
        live.rig.ledger_lines("host_orphan")
    );
    // The seat's own unit is still the only one of this server.
    assert_eq!(live.host_units(), vec![format!("{unit}.service")]);
    let _ = Systemd::from_env().unwrap().stop(&other);
    std::fs::write(live.rig.out("u1").join("go"), "").unwrap();
    live.rig.wait_unit("u1", "done");
}
