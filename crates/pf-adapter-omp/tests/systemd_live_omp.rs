//! AC-10 of `r2.s2.w4`, and the run that CI's `systemd live` step and `scripts/check-hosts-systemd.sh`
//! make with `--ignored` (AC-20, AC-21; SPINE.md Q7, audit 13): an OMP seat in a session host under
//! the real systemd launcher. The adapter is tested alone, over the test host service
//! (`common::hosts::TestHosts`) with `pf_host::launcher::Systemd`, and the stub `omp` stands for OMP:
//! a controller-level OMP run needs a real forge (`LivePorts` builds OMP seats only with
//! `GithubForge`). No test starts OMP or a model.
//!
//! Every test is `#[ignore]`d and named `systemd_live_*`, because a seat inside `seat-fence` reaches
//! no user manager. Without `--ignored` this file only has to compile, and its one executed test
//! (`the_live_tests_stay_ignored_by_default`) keeps the run from reading as a vacuous green.
//!
//! The host server name of every test is `pfompcheck` and six more hex digits. So every host unit of
//! a test is named `pf-host-pfompcheck<six hex>-<session>-<n>`, in the slice
//! `pf-hosts-pfompcheck<six hex>.slice`, and `scripts/check-hosts-systemd.sh` finds the units that a
//! test left behind by that prefix. Each test stops every unit it started when it ends, pass or fail.

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::hosts::{Dirs, Guard, TestHosts};
use common::*;
use pf_adapter_omp::OmpAdapter;
use pf_adapter_omp::pty::live_in_group;
use pf_engine::port::agent::{AgentPort, RebindOutcome};
use pf_host::launcher::{HostState, Systemd};

static N: AtomicU32 = AtomicU32::new(0);

/// A host server name for one test: `pfompcheck` and six hex digits of the process and the test count.
fn test_server() -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    format!(
        "pfompcheck{:06x}",
        (std::process::id().wrapping_mul(131) + n) & 0x00ff_ffff
    )
}

/// The user manager's runtime directory, where a systemd host binds its socket.
fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .expect("XDG_RUNTIME_DIR names the user manager's runtime directory")
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

fn cgroup(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// One controller lifetime over the user manager: a host service of the server `server`.
fn lifetime(dirs: &Dirs, server: &str) -> (Arc<TestHosts>, Guard) {
    let hosts = TestHosts::over(dirs, server, Arc::new(Systemd::new(dirs.rt())), "systemd");
    let guard = Guard(hosts.clone());
    (hosts, guard)
}

fn adapter_for(dir: &std::path::Path, omp: PathBuf, hosts: Arc<TestHosts>) -> OmpAdapter {
    adapter_over(dir, omp, hosts)
}

/// The stub keeps a background child that ignores `SIGHUP`, as a seat's tools can.
const STUB_WITH_CHILD: &str = r#"echo $$ > "$DIR/pid"
(trap '' HUP; exec sleep 600) &
echo $! > "$DIR/child.pid"
echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
wait"#;

fn usage_line(input: u64) -> String {
    serde_json::json!({
        "ev": "message_end", "t": 11, "role": "assistant",
        "usage": {"input": input, "output": 0, "cacheRead": 0, "cacheWrite": 0,
                  "cost": {"total": 0.0}}
    })
    .to_string()
}

/// Every `systemd_live_*` test is `#[ignore]`d: a seat in the fence reaches no user manager. Without
/// this test, a run without `--ignored` executes no test at all, which `oss verify_step` reads as a
/// vacuous green.
#[test]
fn the_live_tests_stay_ignored_by_default() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/systemd_live_omp.rs"
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
    assert!(live >= 2, "found only {live} live tests");
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_an_omp_seat_runs_in_its_own_unit_and_a_second_adapter_rebinds_it_and_reads_an_event_once()
 {
    let dir = tempfile::tempdir().unwrap();
    let dirs = Dirs::with_runtime(runtime_dir());
    let server = test_server();
    let omp = stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN);

    // Lifetime 1: the stub runs on a terminal in a unit of its own, in the server's slice.
    let (hosts_a, _guard_a) = lifetime(&dirs, &server);
    let a = adapter_for(dir.path(), omp.clone(), hosts_a.clone());
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    let unit = s.0.clone();
    assert!(
        unit.starts_with(&format!("pf-host-{server}-")),
        "a unit of another name: {unit}"
    );
    let child = a.process_id(&s).expect("a child");
    let cg = cgroup(child);
    assert!(cg.ends_with(&format!("{unit}.service")), "{cg}");
    assert!(
        cg.contains(&format!("pf-hosts-{server}.slice")),
        "the seat is not in its server's slice: {cg}"
    );
    assert_eq!(property(&unit, "ActiveState"), "active");
    let invocation = property(&unit, "InvocationID");
    assert!(!invocation.is_empty());
    let events = a.event_file(&s).unwrap();
    assert_eq!(a.usage(&s).unwrap().input_tokens, 0);

    // The controller dies: no `stop`. The unit and the stub stay.
    drop(a);
    append(&events, &[&usage_line(7)]);

    // Lifetime 2: a second adapter over a second host service rebinds the same unit, with the same
    // child and the same invocation, and reads the event the stub wrote in between, once.
    let (hosts_b, _guard_b) = lifetime(&dirs, &server);
    let b = adapter_for(dir.path(), omp, hosts_b);
    assert_eq!(b.rebind(&s).unwrap(), RebindOutcome::Survived);
    assert_eq!(b.process_id(&s), Some(child), "the same child");
    assert_eq!(property(&unit, "InvocationID"), invocation);
    assert_eq!(b.usage(&s).unwrap().input_tokens, 7);
    b.drain_events(&s).unwrap();
    b.drain_events(&s).unwrap();
    assert_eq!(
        b.usage(&s).unwrap().input_tokens,
        7,
        "the line is read once"
    );

    // `stop` leaves no process of the group live and the unit gone.
    b.stop(&s).unwrap();
    assert!(
        live_in_group(child).is_empty(),
        "{:?}",
        live_in_group(child)
    );
    assert_eq!(hosts_a.state_of(&unit), HostState::Gone);
    assert!(
        !units(&format!("pf-host-{server}-*")).contains(&format!("{unit}.service")),
        "the unit is still listed"
    );
}

#[test]
#[ignore = "systemd_live: needs a systemd user manager"]
fn systemd_live_stop_leaves_no_process_of_the_group_live_and_the_unit_gone() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = Dirs::with_runtime(runtime_dir());
    let server = test_server();
    let (hosts, _guard) = lifetime(&dirs, &server);
    let a = adapter_for(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_WITH_CHILD),
        hosts.clone(),
    );
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    let unit = s.0.clone();
    let leader: u32 = std::fs::read_to_string(dir.path().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let child: u32 = std::fs::read_to_string(dir.path().join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(live_in_group(leader).contains(&child), "the group runs");

    a.stop(&s).unwrap();
    // No polling: the group is gone when `stop` returns.
    assert_eq!(live_in_group(leader), Vec::<u32>::new());
    assert!(
        !std::path::Path::new(&format!("/proc/{child}")).exists()
            || std::fs::read_to_string(format!("/proc/{child}/stat"))
                .is_ok_and(|t| t.contains(") Z ")),
        "the stub's child is live"
    );
    // The unit is gone, and a second stop is `Ok`.
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while hosts.state_of(&unit) != HostState::Gone && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(hosts.state_of(&unit), HostState::Gone);
    assert!(
        !units(&format!("pf-host-{server}-*")).contains(&format!("{unit}.service")),
        "the unit is still listed"
    );
    assert_eq!(a.stop(&s), Ok(()));
    assert_eq!(a.is_alive(&s), Ok(false));
}
