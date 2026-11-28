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
#[path = "common/settle_rig.rs"]
mod settle_rig;

use std::cell::Cell;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::BIN;
use common::chat::cli_json;
use common::restart::{Restart, host_bin};
use common::{code, stderr};
use pf_engine::port::store::Store;
use pf_host::launcher::{Launcher, StartSpec, Systemd};
use pulse_factory::controller::hosts::HostLauncher;
use serde_json::{Value, json};
use settle_rig::{HostSeed, Link, SRig, WHY};

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

// ------------------------------------------------------------------ the caller check under systemd
//
// `r2.s3.w2`, AC-10 (audit C13, C14; review C8 of AI #130). The controller of these tests runs in the
// test process, with the `systemd` launcher and the user's runtime directory, over a recording chat
// (`settle_rig`). The chat's host unit is a real transient unit, `pf-host-c0ffee<six hex>-S-1-1`,
// whose main process is a small shell. That shell is the chat's process, and it starts the callers
// the tests need, so that each caller runs in the cgroup the test wants and has the ancestry the
// test wants:
//
// * `below`: a child of the chat's process, in the host unit's cgroup;
// * `orphan`: a process whose parent exited, in the host unit's cgroup, with no chat on its chain;
// * `scoped`: a child of the chat's process that `systemd-run --scope` moved out of the unit's
//   cgroup.
//
// Each caller has an empty environment but the path to the controller's state, unless a test adds
// to it. A caller writes its answer to a file that the test reads.

/// A single-quoted shell word.
fn quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The unit that stands in for a chat's host, and the caller factory that runs in it.
struct ChatUnit {
    unit: String,
    dir: PathBuf,
    jobs: Cell<u32>,
}

impl ChatUnit {
    /// Start the unit. Its factory waits for `mode.<n>` files and starts `job.<n>` as each one says.
    fn start() -> ChatUnit {
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .expect(
                "XDG_RUNTIME_DIR must name the user's runtime directory (a user manager must run)",
            );
        let server = test_server();
        let unit = format!("pf-host-{server}-S-1-1");
        let dir = std::env::temp_dir().join(format!("pfl-{}-{server}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The factory is a file that `/bin/sh` runs, not a command line: `systemd-run` expands every
        // `$` of a command line itself (`${m##*.}` became an empty string, so the job file `job.` was
        // never found), and it leaves a script file alone.
        let factory = dir.join("factory.sh");
        std::fs::write(
            &factory,
            r#"d=$1
while :; do
  for m in "$d"/mode.*; do
    [ -e "$m" ] || continue
    n=${m##*.}; mode=$(cat "$m"); rm -f "$m"
    case $mode in
      below) sh "$d/job.$n" & ;;
      orphan) ( sh "$d/job.$n" & ) ;;
      scoped) systemd-run --user --scope --quiet -- sh "$d/job.$n" & ;;
    esac
  done
  sleep 0.1
done
"#,
        )
        .unwrap();
        let out = Command::new("systemd-run")
            .args(["--user", "--collect", "--quiet"])
            .arg(format!("--unit={unit}"))
            .args(["-p", "KillMode=control-group"])
            .args(["--", "/bin/sh"])
            .arg(&factory)
            .arg(&dir)
            .output()
            .expect("systemd-run runs");
        assert!(
            out.status.success(),
            "systemd-run of the chat unit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        ChatUnit {
            unit,
            dir,
            jobs: Cell::new(0),
        }
    }

    /// The factory's PID: the chat's process.
    fn main_pid(&self) -> u32 {
        for _ in 0..100 {
            if let Ok(pid) = property(&self.unit, "MainPID").parse::<u32>()
                && pid != 0
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the unit {} has no main process", self.unit);
    }

    /// The host row for the unit: the one the user manager runs, under the `systemd` launcher.
    fn seed(&self) -> HostSeed {
        HostSeed {
            unit: self.unit.clone(),
            launcher: "systemd".to_string(),
            invocation: property(&self.unit, "InvocationID"),
        }
    }

    /// Run `cmd`, with the environment it holds and no other, as a caller of the kind `mode` names.
    /// Answers the caller's number for [`ChatUnit::result`].
    fn job(&self, mode: &str, cmd: &Command) -> u32 {
        let n = self.jobs.get();
        self.jobs.set(n + 1);
        let mut line = String::from("env -i");
        for (k, v) in cmd.get_envs() {
            if let Some(v) = v {
                line.push_str(&format!(
                    " {}={}",
                    quoted(&k.to_string_lossy()),
                    quoted(&v.to_string_lossy())
                ));
            }
        }
        line.push(' ');
        line.push_str(&quoted(&cmd.get_program().to_string_lossy()));
        for a in cmd.get_args() {
            line.push(' ');
            line.push_str(&quoted(&a.to_string_lossy()));
        }
        // An orphan lets its parent exit before it calls.
        let wait = if mode == "orphan" { "sleep 1\n" } else { "" };
        let (out, rc) = (
            self.dir.join(format!("out.{n}")),
            self.dir.join(format!("rc.{n}")),
        );
        let script = format!(
            "#!/bin/sh\n{wait}{line} > {out} 2>&1\necho $? > {rc}.tmp\nmv {rc}.tmp {rc}\n",
            out = quoted(&out.display().to_string()),
            rc = quoted(&rc.display().to_string()),
        );
        std::fs::write(self.dir.join(format!("job.{n}")), script).unwrap();
        let tmp = self.dir.join(format!("mode.{n}.tmp"));
        std::fs::write(&tmp, mode).unwrap();
        std::fs::rename(&tmp, self.dir.join(format!("mode.{n}"))).unwrap();
        n
    }

    /// The exit code and the output of caller `n`, once it ended.
    fn result(&self, n: u32) -> (i32, String) {
        let rc = self.dir.join(format!("rc.{n}"));
        for _ in 0..600 {
            if let Ok(code) = std::fs::read_to_string(&rc) {
                let out =
                    std::fs::read_to_string(self.dir.join(format!("out.{n}"))).unwrap_or_default();
                return (code.trim().parse().unwrap(), out);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("caller {n} did not end");
    }
}

impl Drop for ChatUnit {
    fn drop(&mut self) {
        let _ = sctl(&["stop", &format!("{}.service", self.unit)]);
        let _ = sctl(&["reset-failed", &format!("{}.service", self.unit)]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An armed run whose unit is parked on a stall, a live grant, and a chat whose host is `unit`.
fn live_rig(unit: &ChatUnit) -> SRig {
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("a user manager runs"));
    let rig = SRig::linked_on(
        &["u1"],
        Some(10),
        Link::Own,
        Some(unit.seed()),
        move |cfg| {
            cfg.host_launcher = HostLauncher::Systemd;
            cfg.runtime_dir = runtime;
        },
    );
    assert_eq!(rig.session, "S-1", "the unit is named for session S-1");
    rig.rec
        .pids
        .lock()
        .unwrap()
        .insert(rig.chat.clone(), unit.main_pid());
    rig
}

/// The test binary as a caller that talks to the controller's socket itself, with a seat token.
fn helper_with_token(rig: &SRig, verb: &str, args: &Value, token: Option<&str>) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args([
        "--exact",
        "settle_rig::helper_calls_the_controller",
        "--nocapture",
        "--test-threads=1",
    ])
    .env_clear()
    .env("PATH", rig.live.sb.path_env())
    .env("PF_TEST_HELPER_STATE", &rig.live.sb.state)
    .env("PF_TEST_HELPER_VERB", verb)
    .env("PF_TEST_HELPER_ARGS", args.to_string());
    if let Some(token) = token {
        c.env("PF_SEAT_TOKEN", token);
    }
    c
}

/// What the controller said to the helper, as the refusal text.
fn helper_refusal(out: &str) -> String {
    let line = out
        .lines()
        .find_map(|l| l.split_once("HELPER_RESPONSE:").map(|(_, r)| r.to_string()))
        .unwrap_or_else(|| panic!("no response from the helper: {out}"));
    let resp: Value = serde_json::from_str(&line).unwrap();
    settle_rig::why(&resp)
}

fn settle_cmd(rig: &SRig, id: &str, rev: u32) -> Command {
    rig.live.sb.cmd(&[
        "settle",
        id,
        "--rev",
        &rev.to_string(),
        "--act",
        "clear",
        "--reason",
        WHY,
    ])
}

fn request_is_open_and_unit_parked(rig: &SRig, id: &str) {
    let r = rig.live.controller.store().request(id).unwrap().unwrap();
    assert_eq!(r.status.as_str(), "open", "the request stays open");
    assert_eq!(
        rig.live.unit_status("u1"),
        "parked",
        "the unit keeps its park"
    );
    assert_eq!(
        rig.admissions("settle"),
        0,
        "no settle admission in the run"
    );
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_a_process_below_the_orchestrator_chat_settles_a_stall() {
    let unit = ChatUnit::start();
    let rig = live_rig(&unit);
    let (id, rev) = rig.request("u1");
    let n = unit.job("below", &settle_cmd(&rig, &id, rev));
    let (rc, out) = unit.result(n);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("settled {id}@{rev}: act=clear")),
        "{out}"
    );
    assert_eq!(rig.admissions("settle"), 1);
    rig.tick();
    assert_ne!(rig.live.unit_status("u1"), "parked", "the unit resumed");
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_a_seat_in_the_chat_host_units_cgroup_is_refused() {
    let unit = ChatUnit::start();
    let rig = live_rig(&unit);
    let (id, rev) = rig.request("u1");
    // A seat process in the chat host unit's cgroup, with no chat on its chain. Its environment holds
    // no seat token, so the token step cannot be the one that refuses it: the ancestry step does,
    // before the cgroup step is reached.
    let args = settle_rig::settle_args(&id, rev, "clear", None, WHY);
    let n = unit.job("orphan", &helper_with_token(&rig, "settle", &args, None));
    let (rc, out) = unit.result(n);
    assert_eq!(
        rc, 0,
        "the helper answers whatever the controller says: {out}"
    );
    let why = helper_refusal(&out);
    assert!(
        why.starts_with("settle refused:")
            && why.contains("the caller runs under no chat session")
            && !why.contains("PF_SEAT_TOKEN"),
        "{why}"
    );
    request_is_open_and_unit_parked(&rig, &id);
    // The same seat with its token is refused at the token step, before the ancestry.
    let n = unit.job(
        "orphan",
        &helper_with_token(&rig, "settle", &args, Some("seat-token-value-1234")),
    );
    let (rc, out) = unit.result(n);
    assert_eq!(rc, 0, "{out}");
    let why = helper_refusal(&out);
    assert!(
        why.starts_with("settle refused:") && why.contains("PF_SEAT_TOKEN"),
        "{why}"
    );
    request_is_open_and_unit_parked(&rig, &id);
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_a_descendant_of_the_chat_that_moved_out_of_its_cgroup_is_refused() {
    let unit = ChatUnit::start();
    let rig = live_rig(&unit);
    let (id, rev) = rig.request("u1");
    // The caller is a descendant of the chat's process, and `systemd-run --scope` moved it out of the
    // host unit's cgroup into a scope of its own: the ancestry names the chat, the cgroup does not.
    let n = unit.job("scoped", &settle_cmd(&rig, &id, rev));
    let (rc, out) = unit.result(n);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("settle refused:") && out.contains("must both name the same chat session"),
        "{out}"
    );
    request_is_open_and_unit_parked(&rig, &id);
    assert_eq!(rig.refused_settles().len(), 1, "the refusal is audited");
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_grant_verbs_refuse_a_caller_in_the_chat_host_units_cgroup_and_admit_the_operators_cli()
 {
    let unit = ChatUnit::start();
    let rig = live_rig(&unit);
    let live_grant = |rig: &SRig| {
        cli_json(&rig.live, &["grant", "list"])["grants"][0]["live"]
            .as_bool()
            .unwrap()
    };
    assert!(live_grant(&rig), "the rig gave the grant");

    // The operator's CLI runs in no chat host unit: it ends the grant.
    let out = rig
        .live
        .cli(&["grant", "revoke", "--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!live_grant(&rig));

    // A caller in the chat host unit's cgroup, with no chat ancestor and an empty environment, is
    // refused on `grant clear`, and the grant stays ended.
    let clear = rig
        .live
        .sb
        .cmd(&["grant", "clear", "--project", &rig.project]);
    let (rc, out) = unit.result(unit.job("orphan", &clear));
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("grant_clear refused: the caller runs in the cgroup of chat host unit"),
        "{out}"
    );
    assert!(!live_grant(&rig), "the refused clear gave no grant");

    // The operator's CLI gives it again.
    let out = rig.live.cli(&["grant", "clear", "--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(live_grant(&rig));

    // The same caller is refused on `grant revoke`, and the grant stays live.
    let revoke = rig
        .live
        .sb
        .cmd(&["grant", "revoke", "--project", &rig.project]);
    let (rc, out) = unit.result(unit.job("orphan", &revoke));
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("grant_revoke refused: the caller runs in the cgroup of chat host unit"),
        "{out}"
    );
    assert!(live_grant(&rig), "the refused revoke ended nothing");
}
