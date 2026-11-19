//! AC-9, and the run CI's `systemd live` step and `scripts/check-hosts-systemd.sh` make with
//! `--ignored`: a real transient systemd user unit per host. Every test is `#[ignore]`d and named
//! `systemd_live_*`, because a seat inside `seat-fence` reaches no user manager (SPINE.md, the
//! measured fact; Q7). Without `--ignored` this file only has to compile.
//!
//! The units are named `pf-host-pfcheck-<session>-1`, in the slice `pf-hosts-pfcheck.slice`, and
//! each test stops its units when it ends.

mod common;

use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{KEY, Tmp, host_bin, pid_alive, pipes, record, wait_until};
use pf_host::client::{self, Event, PeerCheckError};
use pf_host::launcher::{HostState, Launcher, StartSpec, Started, Systemd};

const SERVER: &str = "pfcheck";
const SECRET: &str = "tok-9d02-live-secret";

static N: AtomicU32 = AtomicU32::new(0);

/// A fresh session name, so tests that run side by side never share a unit.
fn session(tag: &str) -> String {
    format!(
        "{tag}{}x{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

fn sctl(args: &[&str]) -> String {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .expect("systemctl runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn property(unit: &str, name: &str) -> String {
    sctl(&["show", "-p", name, "--value", &format!("{unit}.service")])
        .trim()
        .to_string()
}

/// What the user journal holds for a unit, for a failure message: a collected unit has no status
/// left to ask for.
fn unit_journal(unit: &str) -> String {
    let out = Command::new("journalctl")
        .args(["--user", "--no-pager", "-o", "cat", "-u"])
        .arg(format!("{unit}.service"))
        .output();
    match out {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => format!("(journalctl did not run: {e})"),
    }
}

/// A test's setup: a state directory, the real launcher, and the units to stop at the end.
struct Live {
    tmp: Tmp,
    launcher: Systemd,
    units: RefCell<Vec<String>>,
}

impl Live {
    fn new() -> Live {
        let launcher = Systemd::from_env().expect(
            "XDG_RUNTIME_DIR must name the user's runtime directory (a user manager must run)",
        );
        Live {
            tmp: Tmp::new(),
            launcher,
            units: RefCell::new(Vec::new()),
        }
    }

    fn spec(&self, session: &str, record_json: &str) -> StartSpec {
        let path = self.tmp.path.join(format!("{session}.json"));
        std::fs::write(&path, record_json).unwrap();
        StartSpec::new(
            SERVER,
            session,
            1,
            path,
            self.tmp.path.join("state"),
            host_bin(),
            KEY.to_string(),
        )
    }

    fn start(
        &self,
        session: &str,
        record_json: &str,
        tweak: impl FnOnce(&mut StartSpec),
    ) -> Started {
        let mut spec = self.spec(session, record_json);
        tweak(&mut spec);
        let started = self
            .launcher
            .start(&spec)
            .unwrap_or_else(|e| panic!("the unit did not start: {e}"));
        self.units.borrow_mut().push(started.unit.clone());
        started
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        for unit in self.units.borrow().iter() {
            let _ = self.launcher.stop(unit);
        }
    }
}

fn cgroup(pid: u32) -> String {
    pf_host::peer::cgroup_of(pid as i32).unwrap_or_else(|| panic!("no cgroup for pid {pid}"))
}

/// The one test of this file that runs by default. It holds the file's own promise: every
/// `systemd_live_*` test is `#[ignore]`d, so a seat in the fence passes this file. Without it, a run
/// without `--ignored` executes no test at all, which `oss verify_step` reads as a vacuous green.
#[test]
fn the_live_tests_stay_ignored_by_default() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/systemd_live.rs"
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
    assert!(live >= 7, "found only {live} live tests");
}

/// Not a test of its own: the process that launches a unit and is then killed.
#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_launcher_helper() {
    let Ok(record_path) = std::env::var("PF_TEST_HELPER_RECORD") else {
        return;
    };
    let spec = StartSpec::new(
        SERVER,
        &std::env::var("PF_TEST_HELPER_SESSION").unwrap(),
        1,
        record_path.into(),
        std::env::var("PF_TEST_HELPER_STATE").unwrap().into(),
        host_bin(),
        KEY.to_string(),
    );
    let started = Systemd::from_env().unwrap().start(&spec).unwrap();
    println!("STARTED {} {}", started.unit, started.pid);
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_host_survives_the_sigkill_of_the_process_that_launched_it() {
    let live = Live::new();
    let session = session("surv");
    let spec = live.spec(
        &session,
        &record(&["/bin/sh", "-c", "echo up; sleep 60"], pipes()),
    );
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "systemd_live_launcher_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PF_TEST_HELPER_RECORD", &spec.record_path)
        .env("PF_TEST_HELPER_SESSION", &session)
        .env("PF_TEST_HELPER_STATE", &spec.state_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(helper.stdout.take().unwrap());
    let (unit, pid) = loop {
        let mut line = String::new();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "the helper ended before it started a unit"
        );
        if let Some(at) = line.find("STARTED ") {
            let (unit, pid) = line[at + "STARTED ".len()..]
                .trim()
                .split_once(' ')
                .unwrap();
            break (unit.to_string(), pid.parse::<u32>().unwrap());
        }
    };
    live.units.borrow_mut().push(unit.clone());
    helper.kill().unwrap();
    helper.wait().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(pid_alive(pid), "the host died with its launcher");
    assert_eq!(live.launcher.state(&unit), HostState::Live);
    let socket = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap())
        .join(&unit)
        .join("host.sock");
    let (mut conn, hello) = client::connect(&socket, KEY).expect("attach after the launcher died");
    assert!(pid_alive(hello.child_pid));
    conn.read_from(0).unwrap();
    assert_eq!(
        conn.next_event().unwrap(),
        Event::Out {
            at: 0,
            data: b"up\n".to_vec()
        }
    );
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_show_reports_the_invocation_id_that_hello_matches() {
    let live = Live::new();
    let s = live.start(
        &session("inv"),
        &record(&["/bin/sh", "-c", "sleep 60"], pipes()),
        |_| {},
    );
    let shown = property(&s.unit, "InvocationID");
    assert_eq!(shown.len(), 32, "systemctl shows {shown:?}");
    assert_eq!(s.invocation_id, shown);
    let (_, hello) = client::connect(&s.socket, KEY).unwrap();
    assert_eq!(
        hello.invocation_id, shown,
        "hello's invocation id is not the unit's"
    );
    assert_eq!(property(&s.unit, "MainPID"), s.pid.to_string());
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_the_hosts_cgroup_is_its_units_and_peer_check_tells_units_apart() {
    let live = Live::new();
    let a = live.start(
        &session("cga"),
        &record(&["/bin/sh", "-c", "sleep 60"], pipes()),
        |_| {},
    );
    let b = live.start(
        &session("cgb"),
        &record(&["/bin/sh", "-c", "sleep 60"], pipes()),
        |_| {},
    );
    let cg_a = cgroup(a.pid);
    let cg_b = cgroup(b.pid);
    assert!(cg_a.ends_with(&format!("{}.service", a.unit)), "{cg_a}");
    assert!(
        cg_a.contains("pf-hosts-pfcheck.slice"),
        "the host is not in its server's slice: {cg_a}"
    );
    assert_ne!(cg_a, cg_b);
    // The child is in the host's cgroup, so a session has a cgroup of its own.
    let (conn_a, hello_a) = client::connect(&a.socket, KEY).unwrap();
    assert_eq!(cgroup(hello_a.child_pid), cg_a);
    conn_a.peer_check(&cg_a).expect("the host's own cgroup");
    match conn_a.peer_check(&cg_b) {
        Err(PeerCheckError::WrongCgroup { found, .. }) => assert_eq!(found, cg_a),
        other => panic!("another unit's cgroup was accepted: {other:?}"),
    }
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_list_names_the_unit() {
    let live = Live::new();
    let s = live.start(
        &session("lst"),
        &record(&["/bin/sh", "-c", "sleep 60"], pipes()),
        |_| {},
    );
    let hosts = live.launcher.list(SERVER).unwrap();
    let found = hosts
        .iter()
        .find(|h| h.unit == s.unit)
        .unwrap_or_else(|| panic!("{} not in {hosts:?}", s.unit));
    assert_eq!(found.state, HostState::Live);
    assert!(found.session.starts_with("lst"));
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_kill_leaves_an_exit_file_from_stop_post_and_no_process_of_the_unit() {
    let live = Live::new();
    let s = live.start(
        &session("kil"),
        &record(&["/bin/sh", "-c", "sleep 60"], pipes()),
        |_| {},
    );
    let (_, hello) = client::connect(&s.socket, KEY).unwrap();
    let child = hello.child_pid;
    // `--kill-whom=main`: the host process dies by SIGKILL (an OOM kill, a `kill -9` of its PID),
    // then the stop-post step runs. The default `--kill-whom=all` kills the unit's whole cgroup,
    // and that also kills the stop-post step, so no exit file would be written.
    sctl(&[
        "kill",
        "--kill-whom=main",
        "-s",
        "KILL",
        &format!("{}.service", s.unit),
    ]);
    assert!(
        wait_until(Duration::from_secs(15), || live.launcher.state(&s.unit)
            == HostState::Gone),
        "the unit did not end"
    );
    let file = live
        .tmp
        .path
        .join("state/hosts/exits")
        .join(format!("{}.json", s.unit));
    assert!(
        wait_until(Duration::from_secs(5), || file.exists()),
        "no exit file at {}\nthe exits directory holds: {:?}\nthe unit's journal:\n{}",
        file.display(),
        std::fs::read_dir(file.parent().unwrap())
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
            .map_err(|e| e.to_string()),
        unit_journal(&s.unit)
    );
    let body: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(body["written_by"], "stop-post");
    assert_eq!(body["service_result"], "signal", "{body}");
    assert!(!pid_alive(s.pid), "the host survived");
    assert!(!pid_alive(child), "the child survived");
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_no_secrets_file_survives_and_the_unit_properties_hold_no_secret() {
    let live = Live::new();
    let name = session("sec");
    let script = "env; echo END-OF-REPORT; sleep 60";
    let s = live.start(
        &name,
        &record(&["/bin/sh", "-c", script], pipes()),
        |spec| {
            spec.secrets
                .insert("PF_SECRET_TOKEN".to_string(), SECRET.to_string());
        },
    );
    let runtime = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap());
    assert!(
        !runtime
            .join("pulse-factory/secrets")
            .join(format!("{}.json", s.unit))
            .exists()
    );
    let all = sctl(&["show", &format!("{}.service", s.unit)]);
    let env_only = property(&s.unit, "Environment");
    for text in [&all, &env_only] {
        assert!(
            !text.contains(SECRET),
            "a secret value in the unit's properties"
        );
        assert!(
            !text.contains(KEY),
            "the attach key in the unit's properties"
        );
    }
    // The child still has its secret, in its environment only.
    let (mut conn, _) = client::connect(&s.socket, KEY).unwrap();
    conn.read_from(0).unwrap();
    let mut text = Vec::new();
    while !String::from_utf8_lossy(&text).contains("END-OF-REPORT") {
        if let Event::Out { data, .. } = conn.next_event().unwrap() {
            text.extend_from_slice(&data);
        }
    }
    let text = String::from_utf8_lossy(&text);
    assert!(
        text.contains(&format!("PF_SECRET_TOKEN={SECRET}")),
        "the child lacks its secret"
    );
    assert!(!text.contains(KEY));
}
