//! AC-14 and AC-15 of `r2.s2.w5` (d30's guard and its static checks, and the chat and start demos that
//! leave no host: `run` asserts it for each script it runs), AC-14 and AC-8 (`r0.s2.w3`), AC-5 (`r0.s3.w1`), AC-3 to AC-7 (`r1.s1.w5`), AC-6 (`r1.s2.w5`) and
//! AC-9 (`r1.s2.w4`), AC-14 (`r1.s3.w3`) and AC-27 (`r1.s4.w4`): the demo scripts run against the built binary, and each fails when its journey is
//! broken. The scripts run the paper env: no model, no spend, no network beyond `127.0.0.0/8`, no `gh`,
//! and no build. d14 (`community-edition.sh`) is the exception that the five tests at the end cover: it
//! asks GitHub for a CI run, and here a stub `gh` answers.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(120);
/// The limit of `spine-base.sh`: one round, one item.
const BASE_LIMIT: Duration = Duration::from_secs(60);
/// The limit of `seat-tamper.sh`: two runs, about ten paper seats.
const TAMPER_LIMIT: Duration = Duration::from_secs(60);
/// The limit of `stranded-close.sh`: the script's own budget is 60 s, so a failure it reports for a
/// budget overrun arrives before this limit.
const STRANDED_LIMIT: Duration = Duration::from_secs(75);

/// Two tamper demos never run at once. A test that panicked leaves the mutex poisoned, so each
/// holder takes the guard with `unwrap_or_else(|e| e.into_inner())`.
static TAMPER: Mutex<()> = Mutex::new(());
/// The same for the stranded-close demos: four of them run in this file, each with two runs.
static STRANDED: Mutex<()> = Mutex::new(());

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn script(name: &str) -> PathBuf {
    root().join("scripts/demo").join(name)
}

/// The runtime directory of one demo script run: a short path, so the host sockets under it stay
/// under 107 bytes, and the script's own, so the hosts of two scripts that run at once never mix.
/// A script that leaves a detached host live under it is a failure (r2.s2.w3, AC-15): a controller
/// stop ends no seat now, so each script stops its own hosts in its exit trap.
struct Runtime(PathBuf);

static RUNTIMES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Runtime {
    fn new() -> Runtime {
        let n = RUNTIMES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("pfd{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Runtime(dir)
    }

    /// The live hosts under this directory: the `host.pid` of every `pf-hosts/<server>/<unit>`
    /// whose PID still has its recorded start time.
    fn live_hosts(&self) -> Vec<(PathBuf, u32)> {
        let mut out = Vec::new();
        let Ok(servers) = std::fs::read_dir(self.0.join("pf-hosts")) else {
            return out;
        };
        for server in servers.flatten() {
            let Ok(units) = std::fs::read_dir(server.path()) else {
                continue;
            };
            for unit in units.flatten() {
                let Ok(text) = std::fs::read_to_string(unit.path().join("host.pid")) else {
                    continue;
                };
                let mut it = text.split_whitespace();
                let (Some(Ok(pid)), Some(Ok(start))) = (
                    it.next().map(str::parse::<u32>),
                    it.next().map(str::parse::<u64>),
                ) else {
                    continue;
                };
                if pf_host::peer::start_time(pid) == Some(start) {
                    out.push((unit.path(), pid));
                }
            }
        }
        out
    }
}

impl Drop for Runtime {
    /// A host that a failed script left is stopped here, so one failure leaves no process behind.
    fn drop(&mut self) {
        for (_, pid) in self.live_hosts() {
            pf_host::peer::kill_group(pid, 9);
            pf_host::peer::kill_pid(pid, 9);
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Fail when a script left a detached host of its own server live.
fn assert_no_host_left(name: &str, rt: &Runtime) {
    let left = rt.live_hosts();
    assert!(
        left.is_empty(),
        "{name} left {} detached host(s) live: {left:?}",
        left.len()
    );
}

fn run(name: &str, env: &[(&str, &str)]) -> (Output, Duration) {
    let _one_tamper_demo =
        (name == "seat-tamper.sh").then(|| TAMPER.lock().unwrap_or_else(|e| e.into_inner()));
    let _one_stranded_demo =
        (name == "stranded-close.sh").then(|| STRANDED.lock().unwrap_or_else(|e| e.into_inner()));
    let mut cmd = Command::new("bash");
    let rt = Runtime::new();
    cmd.arg(script(name))
        .env("PF_BIN", env!("CARGO_BIN_EXE_pulse-factory"))
        .env("XDG_RUNTIME_DIR", &rt.0)
        .env_remove("PF_SEAT_TOKEN")
        .env_remove("PF_STATE_DIR")
        .env_remove("PF_SHARE_DIR");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let out = cmd.output().expect("bash starts");
    let took = start.elapsed();
    assert_no_host_left(name, &rt);
    (out, took)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A copy of `scripts/demo/paper` that a test may break.
fn paper_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join("paper");
    copy_dir(&root().join("scripts/demo/paper"), &paper);
    (dir, paper)
}

/// A copy of `scripts/demo/paper-b` that a test may break.
fn paper_b_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join("paper-b");
    copy_dir(&root().join("scripts/demo/paper-b"), &paper);
    (dir, paper)
}

fn edit(paper: &Path, rel: &str, f: impl FnOnce(String) -> String) {
    let path = paper.join(rel);
    let text = std::fs::read_to_string(&path).unwrap();
    let new = f(text.clone());
    assert_ne!(new, text, "the edit of {rel} changed nothing");
    std::fs::write(&path, new).unwrap();
}

/// Run a paper demo whose every seat hangs, wait until a detached host is live, then send the script
/// `SIGTERM` (an interrupted demo, or one that fails with a seat still running). The script's exit
/// trap must stop its own server's hosts: a controller stop ends no seat now (G6). The test fails when a
/// host of the script is still live 15 s after the script ended.
fn interrupted_demo_leaves_no_host(name: &str, package: &str) {
    interrupted_demo_with_env(name, package, &[]);
}

/// [`interrupted_demo_leaves_no_host`] for a script that needs more binaries than `PF_BIN`.
fn interrupted_demo_with_env(name: &str, package: &str, env: &[(&str, &str)]) {
    let _tamper =
        (name == "seat-tamper.sh").then(|| TAMPER.lock().unwrap_or_else(|e| e.into_inner()));
    let _stranded =
        (name == "stranded-close.sh").then(|| STRANDED.lock().unwrap_or_else(|e| e.into_inner()));
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join(package);
    copy_dir(&root().join("scripts/demo").join(package), &paper);
    for entry in std::fs::read_dir(paper.join("seats")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sh") {
            let text = std::fs::read_to_string(&path).unwrap();
            let (shebang, rest) = text.split_once('\n').expect("a seat has a shebang line");
            std::fs::write(&path, format!("{shebang}\nsleep 600\n{rest}\n")).unwrap();
        }
    }
    let rt = Runtime::new();
    let mut child = Command::new("bash")
        .arg(script(name))
        .env("PF_BIN", env!("CARGO_BIN_EXE_pulse-factory"))
        .env("XDG_RUNTIME_DIR", &rt.0)
        .env("PAPER_SRC", &paper)
        .envs(env.iter().copied())
        .env_remove("PF_SEAT_TOKEN")
        .env_remove("PF_STATE_DIR")
        .env_remove("PF_SHARE_DIR")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("bash starts");
    let start = Instant::now();
    while rt.live_hosts().is_empty() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "{name} ended before a seat host was live"
        );
        assert!(
            start.elapsed() < Duration::from_secs(40),
            "{name} started no host in 40 s"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    pf_host::peer::kill_pid(child.id(), 15);
    child.wait().unwrap();
    let end = Instant::now();
    while !rt.live_hosts().is_empty() && end.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_no_host_left(name, &rt);
}

#[test]
fn every_demo_script_that_starts_a_controller_stops_its_hosts_in_its_exit_trap() {
    // The five tests below run the paper demos. The chat and UI demos start no paper seat, so they
    // cannot be made to leave a host here: this test holds them to the same helper by text.
    let mut starters = 0;
    for entry in std::fs::read_dir(root().join("scripts/demo")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "sh") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let starts = text.lines().any(|l| {
            let l = l.trim_start();
            !l.starts_with('#')
                && (l.contains("\"$PF_BIN\" controller") || l.contains("\"$pf_bin\" controller"))
        });
        if !starts {
            continue;
        }
        starts_one(&path, &text);
        starters += 1;
    }
    assert!(
        starters >= 14,
        "found only {starters} scripts that start a controller"
    );
}

fn starts_one(path: &Path, text: &str) {
    let name = path.file_name().unwrap().to_string_lossy();
    assert!(
        text.contains("lib/hosts.sh"),
        "{name} does not source lib/hosts.sh"
    );
    let cleanup = text
        .lines()
        .skip_while(|l| !l.starts_with("cleanup() "))
        .take_while(|l| !l.starts_with('}') || l.starts_with("cleanup()"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        cleanup.contains("stop_hosts"),
        "{name}: its cleanup does not call stop_hosts"
    );
    assert!(
        text.contains("trap cleanup EXIT"),
        "{name} has no exit trap named cleanup"
    );
}

/// d30 (`host-restart.sh`, `r2.s2.w5`, AC-14 and AC-15): the script needs a real systemd user
/// manager, so a seat inside the fence cannot run it. A runtime directory and a bus that do not exist
/// stop it at its guard, in a fenced seat and in CI alike: one line on stderr, exit 1, at once.
#[test]
fn host_restart_with_no_user_manager_exits_1_within_5_s_with_the_guard_text() {
    let start = Instant::now();
    let out = Command::new("bash")
        .arg(script("host-restart.sh"))
        .env("XDG_RUNTIME_DIR", "/nonexistent")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .env_remove("PF_SEAT_TOKEN")
        .env_remove("PF_STATE_DIR")
        .env_remove("PF_SHARE_DIR")
        .output()
        .expect("bash starts");
    let took = start.elapsed();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{:?}\n{}",
        out.status,
        stderr(&out)
    );
    assert_eq!(
        stderr(&out).trim_end(),
        "host-restart: no systemd user manager (run outside the seat fence)"
    );
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(took < Duration::from_secs(5), "{took:?}");
}

/// The code lines of a script: no blank line and no comment.
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

#[test]
fn host_restart_builds_nothing_makes_a_temp_directory_and_takes_its_binaries_by_variable() {
    let text = std::fs::read_to_string(script("host-restart.sh")).unwrap();
    for bad in [
        "cargo build",
        "cargo run",
        "cargo install",
        "gh pr",
        "gh api",
    ] {
        assert!(!text.contains(bad), "host-restart.sh holds {bad:?}");
    }
    assert!(
        text.contains("mktemp -d"),
        "host-restart.sh makes a temp directory"
    );
    for var in ["PF_BIN", "PF_HOST_BIN", "PF_REPLAY_BIN", "PF_STREAM_BIN"] {
        assert!(
            text.contains(&format!("{var}=\"${{{var}:-")),
            "host-restart.sh takes {var} with a default"
        );
    }
    // The guard comes first: before the binaries, the tools and the temp directory.
    let code = code_lines(&text);
    assert_eq!(code[0], "set -euo pipefail");
    assert!(
        code[1].starts_with("if ! systemctl --user show-environment"),
        "the guard is the first command: {}",
        code[1]
    );
    // The budget is one wall-time limit of 60 s.
    assert!(
        text.contains("BUDGET=60"),
        "host-restart.sh has no 60 s budget"
    );
}

#[test]
fn host_restart_stops_its_hosts_in_its_exit_trap() {
    let path = script("host-restart.sh");
    let text = std::fs::read_to_string(&path).unwrap();
    starts_one(&path, &text);
    // It also stops the units themselves, and fails a run that leaves one.
    let cleanup = text
        .lines()
        .skip_while(|l| !l.starts_with("cleanup() "))
        .take_while(|l| !l.starts_with('}') || l.starts_with("cleanup()"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        cleanup.contains("pf-host-$server-"),
        "the trap stops no host unit"
    );
    assert!(
        cleanup.contains("pfc-$server"),
        "the trap stops no controller unit"
    );
    assert!(
        cleanup.contains("units left behind"),
        "the trap checks for left units"
    );
}

#[test]
fn host_restart_calls_systemctl_only_on_its_own_units() {
    let text = std::fs::read_to_string(script("host-restart.sh")).unwrap();
    assert!(
        !text.contains("pulse-factory.service"),
        "host-restart.sh names the installed controller's unit"
    );
    let mut calls = 0;
    for line in code_lines(&text) {
        if line.contains("systemctl --user") {
            calls += 1;
            assert!(
                [
                    "show-environment",
                    "list-units",
                    "pfc-$server",
                    "pf-host-$server-"
                ]
                .iter()
                .any(|ok| line.contains(ok)),
                "a systemctl call that names no unit of this script's server: {line}"
            );
        }
        if line.contains("--unit=") {
            assert!(
                line.contains("--unit=\"pfc-$server\""),
                "a unit of another name: {line}"
            );
        }
    }
    assert!(calls >= 8, "found only {calls} systemctl calls");
}

/// A stand-in for a detached host: a bash process in its own session and group. Its first argument
/// is its unit directory, its second is how it answers `SIGTERM`:
///
/// - `delay`: it ends its child and exits one second after the signal, and writes `termed` first;
/// - `ignore`: it ignores the signal, and so does its child.
///
/// It writes its `host.pid` (PID and start time, as the real host does) only after it installed its
/// trap, so a test that waits for the file never signals a host that is not ready. Its child's PID
/// is in `child.pid`.
const FAKE_HOST: &str = r#"
unit=$1
mode=$2
if [ "$mode" = delay ]; then
  trap 'touch "$unit/termed"; kill "$child" 2>/dev/null; sleep 1; exit 0' TERM
  sleep 600 & child=$!
else
  trap '' TERM
  ( trap '' TERM; exec sleep 600 ) & child=$!
fi
echo "$child" > "$unit/child.pid"
start="$(sed 's/.*) //' "/proc/$$/stat" | cut -d' ' -f20)"
echo "$$ $start" > "$unit/host.pid.tmp"
mv "$unit/host.pid.tmp" "$unit/host.pid"
while :; do wait "$child"; sleep 0.1; done
"#;

/// Start a `FAKE_HOST` as unit `unit` of server `server` under `rt`, and return its PID once its
/// `host.pid` exists. `setsid --fork` makes it a session leader that init adopts, like a real host.
fn start_fake_host(rt: &Runtime, server: &str, unit: &str, mode: &str) -> u32 {
    let unit_dir = rt.0.join("pf-hosts").join(server).join(unit);
    std::fs::create_dir_all(&unit_dir).unwrap();
    let script = rt.0.join(format!("fake-host-{server}-{unit}.sh"));
    std::fs::write(&script, FAKE_HOST).unwrap();
    let status = Command::new("setsid")
        .arg("--fork")
        .arg("bash")
        .arg(&script)
        .arg(&unit_dir)
        .arg(mode)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("setsid starts");
    assert!(status.success());
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(unit_dir.join("host.pid")) {
            return text.split_whitespace().next().unwrap().parse().unwrap();
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the fake host {unit} wrote no host.pid in 10 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Run `stop_hosts` of `scripts/demo/lib/hosts.sh` for `server`, as a script's exit trap does.
fn run_stop_hosts(rt: &Runtime, server: &str) -> Output {
    let state = rt.0.join("state");
    std::fs::create_dir_all(state.join("hosts")).unwrap();
    std::fs::write(state.join("hosts/server"), server).unwrap();
    Command::new("bash")
        .arg("-c")
        .arg(r#". "$1"; stop_hosts"#)
        .arg("stop-hosts")
        .arg(script("lib/hosts.sh"))
        .env("PF_STATE_DIR", &state)
        .env("XDG_RUNTIME_DIR", &rt.0)
        .output()
        .expect("bash starts")
}

/// Wait until `pid` is gone or a zombie. A process that got `SIGKILL` needs a moment to leave the
/// table, so this polls, but the signal is already sent: the wait has no bearing on the outcome.
fn assert_gone(what: &str, pid: u32) {
    let start = Instant::now();
    while pf_host::peer::start_time(pid).is_some() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{what} (PID {pid}) is still live"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Send `SIGKILL` to a host's group and to the host when it goes out of scope.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        pf_host::peer::kill_group(self.0, 9);
        pf_host::peer::kill_pid(self.0, 9);
    }
}

fn child_pid(rt: &Runtime, server: &str, unit: &str) -> u32 {
    let text = std::fs::read_to_string(
        rt.0.join("pf-hosts")
            .join(server)
            .join(unit)
            .join("child.pid"),
    )
    .unwrap();
    text.trim().parse().unwrap()
}

#[test]
fn stop_hosts_waits_for_a_host_that_exits_late() {
    // r2.s2.w3, PR #76 red check: `stop_hosts` sent `SIGTERM` and returned, so a script's caller found
    // the host live on a slow runner. This host exits one second after the signal; no timing of the
    // test decides the result, because `stop_hosts` returns only when the host is gone.
    let rt = Runtime::new();
    let host = start_fake_host(&rt, "srv", "unit-1", "delay");
    assert_eq!(rt.live_hosts().len(), 1, "the fake host is live before");
    let out = run_stop_hosts(&rt, "srv");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        rt.live_hosts(),
        Vec::<(PathBuf, u32)>::new(),
        "a host is live after stop_hosts returned"
    );
    assert!(
        rt.0.join("pf-hosts/srv/unit-1/termed").exists(),
        "the host did not end through its SIGTERM path"
    );
    assert_gone("the host", host);
}

#[test]
fn stop_hosts_kills_a_host_and_its_child_that_ignore_sigterm() {
    // The host and its child ignore `SIGTERM`: after the 5 s wait, `SIGKILL` goes to the host's group
    // and to the host.
    let rt = Runtime::new();
    let host = start_fake_host(&rt, "srv", "unit-1", "ignore");
    let child = child_pid(&rt, "srv", "unit-1");
    assert!(pf_host::peer::start_time(child).is_some());
    let out = run_stop_hosts(&rt, "srv");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        rt.live_hosts(),
        Vec::<(PathBuf, u32)>::new(),
        "a host is live after stop_hosts returned"
    );
    assert_gone("the host", host);
    assert_gone("the host's child", child);
}

#[test]
fn stop_hosts_touches_only_hosts_of_its_own_server_with_a_matching_start_time() {
    // Two rules that stay: a host of another server is never signalled, and a `host.pid` whose start
    // time differs names another process now, so it is skipped.
    let rt = Runtime::new();
    let own = start_fake_host(&rt, "srv", "unit-1", "delay");
    let other_server = start_fake_host(&rt, "other", "unit-1", "delay");
    let reused = start_fake_host(&rt, "srv", "unit-2", "delay");
    // `Runtime` stops only the hosts whose `host.pid` matches, and this one will not match.
    let _stop_reused = KillOnDrop(reused);
    let pidfile = rt.0.join("pf-hosts/srv/unit-2/host.pid");
    let text = std::fs::read_to_string(&pidfile).unwrap();
    let (pid, start) = text.trim().split_once(' ').unwrap();
    let wrong = start.parse::<u64>().unwrap() + 1;
    std::fs::write(&pidfile, format!("{pid} {wrong}\n")).unwrap();
    let out = run_stop_hosts(&rt, "srv");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_gone("the own host", own);
    let live: Vec<u32> = rt.live_hosts().into_iter().map(|(_, pid)| pid).collect();
    assert_eq!(live, vec![other_server], "{live:?}");
    assert!(pf_host::peer::start_time(reused).is_some());
    assert!(!rt.0.join("pf-hosts/other/unit-1/termed").exists());
    assert!(!rt.0.join("pf-hosts/srv/unit-2/termed").exists());
}

#[test]
fn an_interrupted_golden_journey_a_stops_its_hosts() {
    interrupted_demo_leaves_no_host("golden-journey-a.sh", "paper");
}

#[test]
fn an_interrupted_golden_journey_b_stops_its_hosts() {
    interrupted_demo_leaves_no_host("golden-journey-b.sh", "paper-b");
}

#[test]
fn an_interrupted_spine_base_stops_its_hosts() {
    interrupted_demo_leaves_no_host("spine-base.sh", "paper-base");
}

#[test]
fn an_interrupted_seat_tamper_stops_its_hosts() {
    interrupted_demo_leaves_no_host("seat-tamper.sh", "paper-tamper");
}

#[test]
fn an_interrupted_stranded_close_stops_its_hosts() {
    interrupted_demo_leaves_no_host("stranded-close.sh", "paper-stranded");
}

#[test]
fn every_demo_script_exits_0_within_its_limit() {
    for (name, limit) in [
        ("golden-journey-a.sh", LIMIT),
        ("golden-journey-b.sh", LIMIT),
        ("arm-dry-run.sh", LIMIT),
        ("spine-base.sh", BASE_LIMIT),
        ("seat-tamper.sh", TAMPER_LIMIT),
        ("stranded-close.sh", STRANDED_LIMIT),
    ] {
        let (out, took) = run(name, &[]);
        assert!(
            out.status.success(),
            "{name}: {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status,
            stdout(&out),
            stderr(&out)
        );
        assert!(took < limit, "{name} took {took:?}");
    }
}

/// A copy of `scripts/demo/paper-base` that a test may break.
fn paper_base_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join("paper-base");
    copy_dir(&root().join("scripts/demo/paper-base"), &paper);
    (dir, paper)
}

/// A copy of `scripts/demo/paper-tamper` that a test may break.
fn paper_tamper_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join("paper-tamper");
    copy_dir(&root().join("scripts/demo/paper-tamper"), &paper);
    (dir, paper)
}

#[test]
fn spine_base_reports_what_it_checked() {
    let (out, took) = run("spine-base.sh", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(took < BASE_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "base push: before item.close, push spine-base1 to origin when origin lacks it",
        "run demo-base-1 ended: every item merged",
        "spine-base: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

#[test]
fn the_base_push_demo_fails_without_the_base_push() {
    // With `base_push` removed, the controller pushes nothing, the close seat does not find the
    // spine branch on origin and writes its stop file, and the unit parks: the demo fails at once.
    let (dir, paper) = paper_base_copy();
    edit(&paper, "workflow/workflow.yaml", |t| {
        let start = t
            .find("        base_push:\n")
            .expect("the demo names a base push");
        let end = t[start..]
            .find("        before:")
            .expect("a `before` follows")
            + start;
        format!("{}{}", &t[..start], &t[end..])
    });
    let (out, took) = run("spine-base.sh", &[("PAPER_SRC", paper.to_str().unwrap())]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("FAIL") && stderr(&out).contains("stuck"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("spine branch not on origin"),
        "the seat's own words are in the failure: {}",
        stderr(&out)
    );
    assert!(
        took < BASE_LIMIT,
        "it fails at once, not at the limit: {took:?}"
    );
    drop(dir);
}

#[test]
fn seat_tamper_reports_what_it_checked() {
    let (out, took) = run("seat-tamper.sh", &[]);
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(took < TAMPER_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "base push: before item.close, push spine-tamper1 to origin when origin lacks it",
        "ended: every item merged",
        "refused: origin of",
        "seat-tamper: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

/// Run `seat-tamper.sh` on a copy of the package that a test broke. The demo must fail.
fn tamper_fails_with(paper: &Path) -> (Output, Duration) {
    let (out, took) = run("seat-tamper.sh", &[("PAPER_SRC", paper.to_str().unwrap())]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(took < TAMPER_LIMIT, "{took:?}");
    (out, took)
}

#[test]
fn seat_tamper_fails_when_a_denied_write_lands() {
    // The seat writes a file only on allow. Without that line it writes after a deny too, so the
    // verdict file of item-1 holds the text of item-2.
    let (dir, paper) = paper_tamper_copy();
    edit(&paper, "seats/implementer.sh", |t| {
        t.lines()
            .filter(|l| l.trim() != "[ \"$decision\" = allow ] || return 0")
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let (out, _) = tamper_fails_with(&paper);
    assert!(
        stderr(&out).contains("the verdict file of item-1 changed"),
        "{}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn seat_tamper_fails_when_a_seat_written_hook_runs() {
    // The close seat pushes through `origin` with its hooks on. It then runs the pre-push hook and
    // the receive-pack wrapper that the implementer wrote into the shared git config.
    let (dir, paper) = paper_tamper_copy();
    edit(&paper, "seats/close.sh", |t| {
        t.replacen(
            "git -c core.hooksPath=/dev/null push -q \"$forge\" \"$PAPER_BRANCH\"",
            "git push -q origin \"$PAPER_BRANCH\"",
            1,
        )
    });
    let (out, _) = tamper_fails_with(&paper);
    assert!(
        stderr(&out).contains("a seat-written hook or transport command ran"),
        "{}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn seat_tamper_fails_without_the_base_push() {
    // With `base_push` removed, the controller pushes nothing. The forge holds only the decoy tag
    // `refs/tags/refs/heads/spine-tamper1`, which `ls-remote` matches by its tail. The close seat
    // matches the full name, does not find the branch, writes its stop file, and the unit parks.
    let (dir, paper) = paper_tamper_copy();
    edit(&paper, "workflow/workflow.yaml", |t| {
        let start = t
            .find("        base_push:\n")
            .expect("the demo names a base push");
        let end = t[start..]
            .find("        before:")
            .expect("a `before` follows")
            + start;
        format!("{}{}", &t[..start], &t[end..])
    });
    let (out, _) = tamper_fails_with(&paper);
    assert!(
        stderr(&out).contains("FAIL") && stderr(&out).contains("stuck"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("spine branch not on origin"),
        "the seat's own words are in the failure: {}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn seat_tamper_fails_when_the_hook_is_not_called() {
    // A seat that never asks the hook is denied nothing: it writes nothing, and no audit line
    // exists. The audit check of item-1 runs before the check of item-2, so it names the failure.
    let (dir, paper) = paper_tamper_copy();
    edit(&paper, "seats/implementer.sh", |t| {
        t.lines()
            .map(|l| {
                if l.trim() == "out=\"$(printf '%s\\n' \"$call\" | /bin/sh -c \"$PF_HOOK_CMD\")\"" {
                    "  out='{\"decision\":\"deny\",\"reason\":\"no hook\"}'"
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let (out, _) = tamper_fails_with(&paper);
    assert!(
        stderr(&out).contains("the audit log of item-1 names no refused git config"),
        "{}",
        stderr(&out)
    );
    drop(dir);
}

/// A copy of `scripts/demo/paper-stranded` that a test may break.
fn paper_stranded_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join("paper-stranded");
    copy_dir(&root().join("scripts/demo/paper-stranded"), &paper);
    (dir, paper)
}

#[test]
fn stranded_close_reports_what_it_checked() {
    let (out, took) = run("stranded-close.sh", &[]);
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(took < STRANDED_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "close: up to 1 nudge on no done signal",
        "ended: every item merged",
        "nudged: the seat finished after 1 nudge",
        "stop request R-1 for item-1: close of item-1 on branch work/item-1: the close seat ended before the item PR opened.",
        "stranded: the stop named item-1, work/item-1 and both staged files",
        "stranded-close: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

/// Run `stranded-close.sh` on a copy of the package, or with a stand-in `stranded.sh`, that a test
/// broke. The demo must fail, and its message must name the run that failed.
fn stranded_fails_with(env: &[(&str, &str)], run_name: &str) -> Output {
    let (out, took) = run("stranded-close.sh", env);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(took < STRANDED_LIMIT, "{took:?}");
    assert!(
        stderr(&out).contains(&format!("stranded-close: FAIL: {run_name}:")),
        "the failure names {run_name}: {}",
        stderr(&out)
    );
    out
}

#[test]
fn stranded_close_fails_when_the_close_stage_drops_no_signal() {
    // Without `no_signal` the seat's first turn end parks the unit with the generic reason, in run 1.
    let (dir, paper) = paper_stranded_copy();
    edit(&paper, "workflow/workflow.yaml", |t| {
        let start = t
            .find("        no_signal:\n")
            .expect("the close stage declares no_signal");
        let end = t[start..]
            .find("        stall_minutes:")
            .expect("a `stall_minutes` follows")
            + start;
        format!("{}{}", &t[..start], &t[end..])
    });
    let out = stranded_fails_with(&[("PAPER_SRC", paper.to_str().unwrap())], "run 1 (nudged)");
    assert!(
        stderr(&out).contains("seat ended with no done signal"),
        "the park reason is in the failure: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("launch 1: mode=nudged"),
        "the seat's own log is in the failure: {}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn stranded_close_fails_when_the_paper_seat_ignores_the_nudge_in_run_1() {
    // The seat reads the nudge and ends its turn again with no PR: `stranded.sh` makes a stop, and
    // run 1 has no stop to expect.
    let (dir, paper) = paper_stranded_copy();
    edit(&paper, "seats/close.sh", |t| {
        t.replacen(
            "  finish # after the nudge\n",
            "  end_turn # ignores the nudge\n",
            1,
        )
    });
    let out = stranded_fails_with(&[("PAPER_SRC", paper.to_str().unwrap())], "run 1 (nudged)");
    assert!(
        stderr(&out).contains("the run is stuck: item-1"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("close of item-1 on branch work/item-1"),
        "the stop text is in the failure: {}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn stranded_close_fails_when_stranded_sh_writes_no_stop_file_in_run_2() {
    // A stand-in that exits 0 and writes nothing: the unit parks with the generic reason, which is a
    // `park` request, not a `stop`.
    let dir = tempfile::tempdir().unwrap();
    let silent = dir.path().join("stranded.sh");
    std::fs::write(&silent, "#!/bin/sh\nexit 0\n").unwrap();
    let out = stranded_fails_with(
        &[("STRANDED_SH", silent.to_str().unwrap())],
        "run 2 (stranded)",
    );
    assert!(
        stderr(&out).contains("the open request is a park"),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("ended: every item merged"),
        "run 1 passed: {}",
        stdout(&out)
    );
    drop(dir);
}

#[test]
fn the_golden_journey_reports_what_it_checked() {
    let (out, _) = run("golden-journey-a.sh", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("every item merged"), "{text}");
    assert!(text.contains("total cost 9000"), "{text}");
}

#[test]
fn golden_journey_b_reports_what_it_checked() {
    let (out, took) = run("golden-journey-b.sh", &[]);
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "acked R-2@2 by claim 1; merged",
        "paper spine demo-g2 closed",
        "acked at revision 2",
        "total cost 9000",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

/// The limit of `pair-api.sh`: two controller starts and about thirty `curl` calls.
const PAIR_API_LIMIT: Duration = Duration::from_secs(60);

#[test]
fn pair_api_passes() {
    // d11: pairing, the verbs, the 401 answers, the revoke, one peer blocked while another pairs,
    // the request ledger, and a restart on a new key. `127.0.0.2` and `127.0.0.3` are on the
    // loopback interface on Linux.
    let (out, took) = run("pair-api.sh", &[]);
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < PAIR_API_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "step 1: paired D-1",
        "step 5: 127.0.0.3 blocked after 5 bad codes",
        "step 6: 127.0.0.2 paired D-2 while 127.0.0.3 was blocked",
        "step 7: api.ledger has 13 lines and no secret is in any log",
        "server key changed: re-pair",
        "pair-api: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

/// The limit of `chat-api.sh`: one controller start, a replayed turn, three short readers and about
/// twenty `curl` calls.
const CHAT_API_LIMIT: Duration = Duration::from_secs(60);

#[test]
fn chat_api_passes() {
    // d18: the chat verbs and the chat stream over the API, against the real adapter on a replay of
    // a recorded session. The replay binary is built beside the controller's; a missing one fails
    // the test, never skips it (`cargo build -p pf-adapter-claude --bin pf-claude-replay`).
    let target_dir = Path::new(env!("CARGO_BIN_EXE_pulse-factory"))
        .parent()
        .unwrap()
        .to_path_buf();
    let replay = target_dir.join("pf-claude-replay");
    assert!(
        replay.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bin pf-claude-replay` first",
        replay.display()
    );
    let (out, took) = run(
        "chat-api.sh",
        &[
            ("PF_REPLAY_BIN", replay.to_str().unwrap()),
            ("PF_STREAM_BIN", env!("CARGO_BIN_EXE_pf-stream-reader")),
        ],
    );
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < CHAT_API_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "step 1:",
        "step 2:",
        "step 3:",
        "step 4:",
        "step 5:",
        "step 6:",
        "step 7:",
        "step 8:",
        "step 9:",
        "step 10:",
        "chat-api: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

/// The limit of `needs-you-api.sh`: one controller start, a paper spine to its spine PR, a few
/// seconds of confirm-code waits and about forty `curl` calls. The script's own limit is 100 seconds.
const NEEDS_YOU_API_LIMIT: Duration = Duration::from_secs(120);

#[test]
fn needs_you_api_passes() {
    // d20: a paired device answers a stop-rule question and acks the spine PR with one-use confirm
    // codes, and a process under a chat is refused on `arm`, `claim` and `ack`. The replay binary is
    // built beside the controller's; a missing one fails the test, never skips it
    // (`cargo build -p pf-adapter-claude --bin pf-claude-replay`).
    let target_dir = Path::new(env!("CARGO_BIN_EXE_pulse-factory"))
        .parent()
        .unwrap()
        .to_path_buf();
    let replay = target_dir.join("pf-claude-replay");
    assert!(
        replay.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bin pf-claude-replay` first",
        replay.display()
    );
    let (out, took) = run(
        "needs-you-api.sh",
        &[("PF_REPLAY_BIN", replay.to_str().unwrap())],
    );
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < NEEDS_YOU_API_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "step 1:",
        "step 2:",
        "step 3:",
        "step 4:",
        "step 5:",
        "step 6:",
        "step 7:",
        "step 8:",
        "step 9:",
        "needs-you-api: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

/// The limit of `needs-you-options.sh` (d32): one controller start, one replayed chat turn, a wait of 10 s
/// for a notice to come again, four requests and about sixty `curl` calls. The script's own limit is
/// 60 seconds.
const NEEDS_YOU_OPTIONS_LIMIT: Duration = Duration::from_secs(75);

/// The binaries d32 takes by variable. The replay binary is built beside the controller's; a missing
/// one fails the test, never skips it (`cargo build -p pf-adapter-claude --bin pf-claude-replay`).
fn options_binaries() -> [(&'static str, String); 2] {
    let target_dir = Path::new(env!("CARGO_BIN_EXE_pulse-factory"))
        .parent()
        .unwrap()
        .to_path_buf();
    let replay = target_dir.join("pf-claude-replay");
    assert!(
        replay.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bin pf-claude-replay` first",
        replay.display()
    );
    [
        ("PF_REPLAY_BIN", replay.to_str().unwrap().to_string()),
        (
            "PF_STREAM_BIN",
            env!("CARGO_BIN_EXE_pf-stream-reader").to_string(),
        ),
    ]
}

#[test]
fn needs_you_options_passes() {
    // d32: a paired device takes a seat's option, a note, a workflow's option and a merge ack, with a
    // notice of each request on the app channel and in the project's Orchestrator chat.
    let binaries = options_binaries();
    let env: Vec<(&str, &str)> = binaries.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let (out, took) = run("needs-you-options.sh", &env);
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < NEEDS_YOU_OPTIONS_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "step 1:",
        "step 2:",
        "step 3:",
        "step 4:",
        "step 5:",
        "step 6:",
        "step 7:",
        "step 8:",
        "needs-you-options: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    let last = text.lines().last().unwrap_or_default();
    assert!(last.starts_with("needs-you-options: ok"), "{last}");
}

/// A copy of `scripts/demo/paper-options` that a test may break.
fn paper_options_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join("paper-options");
    copy_dir(&root().join("scripts/demo/paper-options"), &paper);
    (dir, paper)
}

/// Run d32 on a broken copy of its package, and return its output.
fn options_fails_with(paper: &Path) -> Output {
    let binaries = options_binaries();
    let mut env: Vec<(&str, &str)> = binaries.iter().map(|(k, v)| (*k, v.as_str())).collect();
    env.push(("PAPER_SRC", paper.to_str().unwrap()));
    let (out, _) = run("needs-you-options.sh", &env);
    out
}

#[test]
fn needs_you_options_fails_when_the_red_gate_declares_no_options() {
    let (_dir, paper) = paper_options_copy();
    edit(&paper, "workflow/workflow.yaml", |t| {
        let start = t
            .find("            options:\n")
            .expect("the package declares options");
        let end = t[start..]
            .find("        settings:")
            .expect("the settings follow")
            + start;
        format!("{}{}", &t[..start], &t[end..])
    });
    let out = options_fails_with(&paper);
    assert!(!out.status.success(), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.contains("needs-you-options: FAIL: step 5: the red gate's block is not the workflow's"),
        "{err}"
    );
}

#[test]
fn needs_you_options_fails_when_the_stop_names_no_options() {
    let (_dir, paper) = paper_options_copy();
    edit(&paper, "seats/implementer.sh", |t| {
        t.lines()
            .filter(|l| {
                !l.contains("Options:")
                    && !l.contains("- [recommended]")
                    && !l.contains("- Write the word stop")
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let out = options_fails_with(&paper);
    assert!(!out.status.success(), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.contains("needs-you-options: FAIL: step 3: the stop's block is not the seat's"),
        "{err}"
    );
}

#[test]
fn an_interrupted_needs_you_options_stops_its_hosts() {
    let binaries = options_binaries();
    let env: Vec<(&str, &str)> = binaries.iter().map(|(k, v)| (*k, v.as_str())).collect();
    interrupted_demo_with_env("needs-you-options.sh", "paper-options", &env);
}

/// The limit of `start-api.sh`: one controller start, one replayed chat turn, a 4 s wait for a confirm
/// code to expire and about forty `curl` calls. The script's own limit is 150 seconds.
const START_API_LIMIT: Duration = Duration::from_secs(150);

#[test]
fn start_api_passes() {
    // d22: the chat proposes a start, a paired device confirms the card with a one-use code and arms
    // the run. The replay binary is built beside the controller's; a missing one fails the test, never
    // skips it (`cargo build -p pf-adapter-claude --bin pf-claude-replay`).
    let target_dir = Path::new(env!("CARGO_BIN_EXE_pulse-factory"))
        .parent()
        .unwrap()
        .to_path_buf();
    let replay = target_dir.join("pf-claude-replay");
    assert!(
        replay.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bin pf-claude-replay` first",
        replay.display()
    );
    let (out, took) = run(
        "start-api.sh",
        &[
            ("PF_REPLAY_BIN", replay.to_str().unwrap()),
            ("PF_STREAM_BIN", env!("CARGO_BIN_EXE_pf-stream-reader")),
        ],
    );
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < START_API_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "step 1:",
        "step 2:",
        "step 3:",
        "step 4:",
        "step 5:",
        "step 6:",
        "step 7:",
        "start-api: ok",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

#[test]
fn start_api_runs_with_the_binaries_on_path_and_no_pf_variable_set() {
    // The script's header names the defaults `pulse-factory`, `pf-claude-replay` and `pf-stream-reader`
    // on PATH. `start-env.sh` is a child process that needs `PF_BIN` and `PF_REPLAY_BIN` in its
    // environment, so the defaults must reach it exported (r1.s4.w2 review, B1).
    let target_dir = Path::new(env!("CARGO_BIN_EXE_pulse-factory"))
        .parent()
        .unwrap()
        .to_path_buf();
    let replay = target_dir.join("pf-claude-replay");
    assert!(
        replay.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bin pf-claude-replay` first",
        replay.display()
    );
    let bin_dir = std::env::temp_dir().join(format!("pf-start-api-path-{}", std::process::id()));
    std::fs::create_dir_all(&bin_dir).unwrap();
    for (name, from) in [
        ("pulse-factory", env!("CARGO_BIN_EXE_pulse-factory")),
        ("pf-claude-replay", replay.to_str().unwrap()),
        ("pf-stream-reader", env!("CARGO_BIN_EXE_pf-stream-reader")),
    ] {
        let link = bin_dir.join(name);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(from, &link).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let rt = Runtime::new();
    let start = Instant::now();
    let out = Command::new("bash")
        .arg(script("start-api.sh"))
        .env("PATH", path)
        .env("XDG_RUNTIME_DIR", &rt.0)
        .env_remove("PF_BIN")
        .env_remove("PF_REPLAY_BIN")
        .env_remove("PF_STREAM_BIN")
        .env_remove("PF_SEAT_TOKEN")
        .env_remove("PF_STATE_DIR")
        .env_remove("PF_SHARE_DIR")
        .output()
        .expect("bash starts");
    let took = start.elapsed();
    assert_no_host_left("start-api.sh", &rt);
    let _ = std::fs::remove_dir_all(&bin_dir);
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < START_API_LIMIT, "{took:?}");
    assert!(stdout(&out).contains("start-api: ok"), "{}", stdout(&out));
}

/// The limit of `chat-policy.sh`: one controller, two launches of the replay, and two waits for the
/// adapter's 30 s control timeout (the replay does not answer the context-usage request).
const CHAT_POLICY_LIMIT: Duration = Duration::from_secs(240);

#[test]
fn chat_policy_passes() {
    // d21 (`r1.s4.w4`): the scoped settings, the deny rules at launch and the self-started turn cap,
    // against the real adapter on a replay of the recording of qualification case 19. The replay
    // binary is built beside the controller's; a missing one fails the test, never skips it.
    let target_dir = Path::new(env!("CARGO_BIN_EXE_pulse-factory"))
        .parent()
        .unwrap()
        .to_path_buf();
    let replay = target_dir.join("pf-claude-replay");
    assert!(
        replay.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bin pf-claude-replay` first",
        replay.display()
    );
    let (out, took) = run(
        "chat-policy.sh",
        &[("PF_REPLAY_BIN", replay.to_str().unwrap())],
    );
    assert!(
        out.status.success(),
        "{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    assert!(took < CHAT_POLICY_LIMIT, "{took:?}");
    let text = stdout(&out);
    for want in [
        "chat-policy: ok step 1:",
        "chat-policy: ok step 2:",
        "chat-policy: ok step 3:",
        "chat-policy: ok step 4:",
        "chat-policy: ok step 5:",
        "chat-policy: ok\n",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // It builds nothing and leaves nothing outside its temporary directory.
    let body = std::fs::read_to_string(script("chat-policy.sh")).unwrap();
    for bad in ["cargo build", "cargo run", "cargo install", "gh api"] {
        assert!(!body.contains(bad), "chat-policy.sh holds {bad:?}");
    }
    assert!(body.contains("mktemp -d"));
}

#[test]
fn the_scripts_run_no_build_and_hold_no_real_forge_call() {
    for name in [
        "golden-journey-a.sh",
        "golden-journey-b.sh",
        "arm-dry-run.sh",
        "spine-base.sh",
        "seat-tamper.sh",
        "stranded-close.sh",
        "pair-api.sh",
        "chat-api.sh",
        "needs-you-api.sh",
        "needs-you-options.sh",
        "start-api.sh",
    ] {
        let text = std::fs::read_to_string(script(name)).unwrap();
        for bad in [
            "cargo build",
            "cargo run",
            "cargo install",
            "gh pr",
            "gh api",
        ] {
            assert!(!text.contains(bad), "{name} holds {bad:?}");
        }
        assert!(text.contains("mktemp -d"), "{name} makes a temp directory");
        assert!(text.contains("PF_BIN"), "{name} takes PF_BIN");
    }
    // `start-env.sh` makes its paper env under the directory its caller names, so it has no `mktemp`.
    let env = std::fs::read_to_string(script("start-env.sh")).unwrap();
    for bad in [
        "cargo build",
        "cargo run",
        "cargo install",
        "gh pr",
        "gh api",
    ] {
        assert!(!env.contains(bad), "start-env.sh holds {bad:?}");
    }
    assert!(env.contains("PF_BIN"), "start-env.sh takes PF_BIN");
}

#[test]
fn a_seat_script_that_exits_non_zero_fails_the_golden_journey() {
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/implementer.sh", |t| {
        t.replacen("set -eu\n", "set -eu\nexit 3\n", 1)
    });
    let (out, took) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("FAIL"), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("stuck") && stderr(&out).contains("no done signal"),
        "{}",
        stderr(&out)
    );
    assert!(took < LIMIT, "it fails at once, not at the limit: {took:?}");
    drop(dir);
}

#[test]
fn a_changed_usage_number_fails_the_golden_journey() {
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/implementer.sh", |t| {
        t.replace("\"cost_micros\":2000", "\"cost_micros\":2001")
    });
    let (out, _) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("role implementer cost"),
        "{}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn a_usage_reading_counted_twice_fails_the_total() {
    // A seat that writes its usage line twice has its cost counted twice: the run total is not
    // the fixed sum. (A role's cost fails first, so the message names the role.)
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/verifier.sh", |t| {
        let line = t
            .lines()
            .find(|l| l.starts_with("echo '{\"usage\""))
            .unwrap()
            .to_string();
        t.replacen(&line, &format!("{line}\n{line}"), 1)
    });
    let (out, _) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("role verifier cost"),
        "{}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn a_push_that_the_seat_does_not_report_fails_the_golden_journey() {
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/close.sh", |t| {
        t.lines()
            .filter(|l| !(l.contains("push") && l.contains("PF_PAPER_EVENTS")))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let (out, _) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    drop(dir);
}

#[test]
fn arm_dry_run_with_a_failing_gh_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(&gh, "#!/bin/sh\necho 'gh: boom' >&2\nexit 1\n").unwrap();
    let (out, _) = run("arm-dry-run.sh", &[("GH_STUB", gh.to_str().unwrap())]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("arm refused") || stderr(&out).contains("arm --dry-run exited"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn arm_dry_run_fails_when_the_summary_changes() {
    // A workflow that differs from the shipped one in a cap: the script asserts the shipped numbers.
    let dir = tempfile::tempdir().unwrap();
    let wf = dir.path().join("ossify-spine");
    copy_dir(&root().join("workflows/ossify-spine"), &wf);
    let file = wf.join("workflow.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("model_sessions: 2"));
    std::fs::write(
        &file,
        text.replace("model_sessions: 2", "model_sessions: 3"),
    )
    .unwrap();
    let (out, _) = run("arm-dry-run.sh", &[("WORKFLOW_DIR", wf.to_str().unwrap())]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("model_sessions 2"),
        "{}",
        stderr(&out)
    );
}

fn journey_b_with(paper: &Path) -> (Output, Duration) {
    run(
        "golden-journey-b.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    )
}

#[test]
fn a_spine_close_seat_that_exits_non_zero_fails_golden_journey_b() {
    let (dir, paper) = paper_b_copy();
    edit(&paper, "seats/spine_close.sh", |t| {
        t.replacen("set -eu\n", "set -eu\nexit 3\n", 1)
    });
    let (out, took) = journey_b_with(&paper);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("FAIL") && stderr(&out).contains("stuck"),
        "{}",
        stderr(&out)
    );
    assert!(took < LIMIT, "it fails at once, not at the limit: {took:?}");
    drop(dir);
}

#[test]
fn a_spine_pr_with_red_checks_fails_golden_journey_b_at_once() {
    // The merge parks with `merge_failed`; no `merge_ack` request opens.
    let (dir, paper) = paper_b_copy();
    edit(&paper, "seats/spine_close.sh", |t| {
        t.replacen("\"checks\":\"green\"", "\"checks\":\"red\"", 1)
    });
    let (out, took) = journey_b_with(&paper);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("stuck"), "{}", stderr(&out));
    assert!(took < LIMIT, "{took:?}");
    drop(dir);
}

#[test]
fn a_changed_usage_number_fails_golden_journey_b() {
    for (seat, from, to) in [
        ("reviewer", "\"cost_micros\":800", "\"cost_micros\":801"),
        (
            "spine_close",
            "\"cost_micros\":1200",
            "\"cost_micros\":1201",
        ),
        (
            "implementer",
            "\"cost_micros\":2000",
            "\"cost_micros\":2001",
        ),
    ] {
        let (dir, paper) = paper_b_copy();
        edit(&paper, &format!("seats/{seat}.sh"), |t| t.replace(from, to));
        let (out, _) = journey_b_with(&paper);
        assert!(!out.status.success(), "{seat}: {}", stdout(&out));
        assert!(
            stderr(&out).contains(&format!("role {seat} cost")),
            "{seat}: {}",
            stderr(&out)
        );
        drop(dir);
    }
}

#[test]
fn a_spine_pr_that_names_another_base_fails_golden_journey_b() {
    // The PR's base is not the base branch of the run: the merge parks with `merge_failed`.
    let (dir, paper) = paper_b_copy();
    edit(&paper, "seats/spine_close.sh", |t| {
        t.replacen(
            "\"$PAPER_BRANCH\" \"$PAPER_BASE\" \\\n  > \"$(dirname \"$forge\")/prs/100.json\"",
            "\"$PAPER_BRANCH\" \"$PAPER_BRANCH\" \\\n  > \"$(dirname \"$forge\")/prs/100.json\"",
            1,
        )
    });
    let (out, took) = journey_b_with(&paper);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("stuck"), "{}", stderr(&out));
    assert!(took < LIMIT, "{took:?}");
    drop(dir);
}

// ------------------------------------------------------------------ d14: community-edition.sh

/// A git repo with one commit, a stub `gh` and the stub's answers. `list` is what `gh run list`
/// prints, and `view` is what `gh run view` prints.
struct Ce {
    dir: tempfile::TempDir,
    sha: String,
}

impl Ce {
    fn new(list: &str, view: &str) -> Ce {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("git starts");
            assert!(out.status.success(), "git {args:?}: {}", stderr(&out));
            stdout(&out).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let sha = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("list.json"), list).unwrap();
        std::fs::write(dir.path().join("view.json"), view).unwrap();
        let gh = bin.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\nd='{}'\necho \"$@\" >> \"$d/gh.calls\"\ncase \"$1 $2\" in\n  'run list') cat \"$d/list.json\" ;;\n  'run view') cat \"$d/view.json\" ;;\n  *) echo 'gh: unexpected call' >&2; exit 9 ;;\nesac\n",
                dir.path().display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Ce { dir, sha }
    }

    fn run(&self) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new("bash")
            .arg(script("community-edition.sh"))
            .env("PATH", path)
            .env("PF_REPO_DIR", self.dir.path().join("repo"))
            .env("PF_CI_REPO", "acme/widgets")
            .output()
            .expect("bash starts")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("gh.calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The one `gh run list` call that every case makes, and no call that waits or re-runs.
    fn assert_one_list_call(&self) {
        let calls = self.calls();
        let want = format!(
            "run list -R acme/widgets --workflow ci --commit {} --json databaseId,status,conclusion",
            self.sha
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("run list")).count(),
            1,
            "{calls:?}"
        );
        assert!(calls.contains(&want), "{calls:?}");
        for c in &calls {
            assert!(
                !c.contains("rerun") && !c.contains("watch") && !c.contains("--exit-status"),
                "d14 never waits or re-runs: {c}"
            );
        }
    }
}

const SMOKE_GREEN: &str = r#"{"jobs":[{"name":"check","conclusion":"success","steps":[{"name":"Test","conclusion":"success"},{"name":"Smoke","conclusion":"success"}]}]}"#;

#[test]
fn community_edition_says_push_with_no_run() {
    let ce = Ce::new("[]", "{}");
    let out = ce.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stderr(&out).contains(&format!(
            "community-edition: no ci run for {}: push",
            ce.sha
        )),
        "{}",
        stderr(&out)
    );
    ce.assert_one_list_call();
}

#[test]
fn community_edition_says_still_running() {
    for status in ["in_progress", "queued"] {
        let list = format!(r#"[{{"databaseId":4242,"status":"{status}","conclusion":""}}]"#);
        let ce = Ce::new(&list, "{}");
        let out = ce.run();
        assert_eq!(out.status.code(), Some(1), "{status}");
        assert!(
            stderr(&out).contains(&format!(
                "community-edition: ci run 4242 for {} is still running",
                ce.sha
            )),
            "{status}: {}",
            stderr(&out)
        );
        ce.assert_one_list_call();
    }
}

#[test]
fn community_edition_fails_a_failed_run() {
    for conclusion in ["failure", "cancelled"] {
        let list =
            format!(r#"[{{"databaseId":77,"status":"completed","conclusion":"{conclusion}"}}]"#);
        let ce = Ce::new(&list, SMOKE_GREEN);
        let out = ce.run();
        assert_eq!(out.status.code(), Some(1), "{conclusion}");
        let err = stderr(&out);
        assert!(
            err.contains(conclusion) && err.contains("77") && err.contains(&ce.sha),
            "{conclusion}: {err}"
        );
        // A failed run is not looked into: the job list is never asked for.
        assert!(
            !ce.calls().iter().any(|c| c.starts_with("run view")),
            "{:?}",
            ce.calls()
        );
        ce.assert_one_list_call();
    }
}

#[test]
fn community_edition_fails_without_a_smoke_step() {
    let list = r#"[{"databaseId":88,"status":"completed","conclusion":"success"}]"#;
    for view in [
        // No Smoke step at all.
        r#"{"jobs":[{"name":"check","conclusion":"success","steps":[{"name":"Test","conclusion":"success"}]}]}"#,
        // A Smoke step that is not green.
        r#"{"jobs":[{"name":"check","conclusion":"failure","steps":[{"name":"Smoke","conclusion":"failure"}]}]}"#,
        // A Smoke step that was skipped.
        r#"{"jobs":[{"name":"check","conclusion":"success","steps":[{"name":"Smoke","conclusion":"skipped"}]}]}"#,
        // No jobs.
        r#"{"jobs":[]}"#,
    ] {
        let ce = Ce::new(list, view);
        let out = ce.run();
        assert_eq!(out.status.code(), Some(1), "{view}");
        assert!(
            stderr(&out).contains("community-edition: ci run 88 has no green Smoke step"),
            "{view}: {}",
            stderr(&out)
        );
        ce.assert_one_list_call();
    }
}

#[test]
fn community_edition_passes_a_green_run() {
    // The newest run is first. An older failed run for the same commit does not count.
    let list = r#"[{"databaseId":99,"status":"completed","conclusion":"success"},{"databaseId":50,"status":"completed","conclusion":"failure"}]"#;
    let ce = Ce::new(list, SMOKE_GREEN);
    let out = ce.run();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        stdout(&out),
        stderr(&out)
    );
    assert_eq!(
        stdout(&out),
        format!("community-edition: ok ({}, run 99)\n", ce.sha)
    );
    ce.assert_one_list_call();
    assert!(
        ce.calls()
            .iter()
            .any(|c| c.starts_with("run view 99 -R acme/widgets")),
        "{:?}",
        ce.calls()
    );
}
