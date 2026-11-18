//! AC-6: the detached launcher. Its limits (no cgroup of its own, no stop-post) are in its module
//! comment; these tests show what it does give: a host that outlives its launcher.

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{KEY, Rig, pid_alive, pipes, record, wait_until};
use pf_host::client::{self, ClientError, Event};
use pf_host::launcher::{Detached, HostState, Launcher};

/// Not a test of its own: the process that launches a host and is then killed. The test below runs
/// it as a child (`--exact launcher_helper`) with `PF_TEST_HELPER_*` set; without them it returns.
#[test]
fn launcher_helper() {
    let Ok(rt) = std::env::var("PF_TEST_HELPER_RT") else {
        return;
    };
    let rig_state = std::env::var("PF_TEST_HELPER_STATE").unwrap();
    let record_path = std::env::var("PF_TEST_HELPER_RECORD").unwrap();
    let spec = pf_host::launcher::StartSpec::new(
        "t",
        "d1",
        1,
        record_path.into(),
        rig_state.into(),
        common::host_bin(),
        KEY.to_string(),
    );
    let started = Detached::new(rt.into()).start(&spec).unwrap();
    println!("STARTED {} {}", started.unit, started.pid);
    std::thread::sleep(Duration::from_secs(120));
}

fn wait_gone(rig: &Rig, unit: &str) -> bool {
    wait_until(Duration::from_secs(10), || {
        rig.launcher.state(unit) == HostState::Gone
    })
}

#[test]
fn a_host_started_by_a_process_that_is_then_sigkilled_keeps_running_and_its_child_too() {
    let rig = Rig::new();
    let record_path = rig.write_record(
        "d1",
        &record(&["/bin/sh", "-c", "echo up; sleep 60"], pipes()),
    );
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "launcher_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PF_TEST_HELPER_RT", &rig.rt)
        .env("PF_TEST_HELPER_STATE", &rig.state)
        .env("PF_TEST_HELPER_RECORD", &record_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(helper.stdout.take().unwrap());
    let (unit, host_pid) = loop {
        let mut line = String::new();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "the helper ended before it started a host"
        );
        // The test harness prints "test launcher_helper ... " before the helper's own line.
        if let Some(at) = line.find("STARTED ") {
            let (unit, pid) = line[at + "STARTED ".len()..]
                .trim()
                .split_once(' ')
                .unwrap();
            break (unit.to_string(), pid.parse::<u32>().unwrap());
        }
    };
    rig.track(&unit);
    let socket = rig.rt.join("pf-hosts/t/d1-1/host.sock");
    let (conn, hello) = client::connect(&socket, KEY).unwrap();
    let child_pid = hello.child_pid;
    assert!(pid_alive(host_pid) && pid_alive(child_pid));

    // The launching process dies without warning.
    helper.kill().unwrap();
    helper.wait().unwrap();
    std::thread::sleep(Duration::from_millis(300));

    assert!(pid_alive(host_pid), "the host died with its launcher");
    assert!(pid_alive(child_pid), "the child died with its launcher");
    assert_eq!(rig.launcher.state(&unit), HostState::Live);
    // The host's own session: it is neither the launcher's nor its process group's.
    let stat = std::fs::read_to_string(format!("/proc/{host_pid}/stat")).unwrap();
    let f: Vec<&str> = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    let (ppid, pgrp, session): (u32, u32, u32) = (
        f[1].parse().unwrap(),
        f[2].parse().unwrap(),
        f[3].parse().unwrap(),
    );
    assert_ne!(ppid, std::process::id(), "the host is a child of the test");
    assert_ne!(
        session as i32,
        unsafe { libc::getsid(0) },
        "the host shares the caller's session"
    );
    assert_ne!(pgrp, std::process::id());
    // And a new controller can attach and read what the child wrote.
    drop(conn);
    let (mut conn, _) = client::connect(&socket, KEY).unwrap();
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
fn the_directory_is_in_the_runtime_directory_at_0700_and_gone_after_the_host_ends() {
    let rig = Rig::new();
    let s = rig.start("d2", &record(&["/bin/sh", "-c", "sleep 60"], pipes()));
    let dir = rig.rt.join("pf-hosts").join("t").join("d2-1");
    assert!(dir.is_dir());
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(dir.join("host.sock").exists() && dir.join("host.pid").exists());
    rig.launcher.stop(&s.unit).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || !dir.exists()),
        "the directory stayed"
    );
    // It is gone, and so is the host: the exit file is what is left.
    assert!(rig.exit_file(&s.unit).exists());
}

#[test]
fn state_reads_live_then_gone_after_stop() {
    let rig = Rig::new();
    let s = rig.start("d3", &record(&["/bin/sh", "-c", "sleep 60"], pipes()));
    assert_eq!(rig.launcher.state(&s.unit), HostState::Live);
    rig.launcher.stop(&s.unit).unwrap();
    assert!(wait_gone(&rig, &s.unit));
    // Stopping a host that is gone is not an error.
    rig.launcher.stop(&s.unit).unwrap();
    // A name that never existed is gone.
    assert_eq!(rig.launcher.state("pf-host-t-nothing-1"), HostState::Gone);
}

#[test]
fn list_names_the_servers_live_hosts() {
    let rig = Rig::new();
    let a = rig.start("l1", &record(&["/bin/sh", "-c", "sleep 60"], pipes()));
    let b = rig.start("l-2", &record(&["/bin/sh", "-c", "sleep 60"], pipes()));
    let other = rig.start_with(
        "l3",
        &record(&["/bin/sh", "-c", "sleep 60"], pipes()),
        |spec| {
            spec.server = "u".to_string();
        },
    );
    let hosts = rig.launcher.list("t").unwrap();
    let names: Vec<(&str, &str, u64)> = hosts
        .iter()
        .map(|h| (h.unit.as_str(), h.session.as_str(), h.incarnation))
        .collect();
    assert_eq!(
        names,
        vec![("pf-host-t-l-2-1", "l-2", 1), ("pf-host-t-l1-1", "l1", 1)],
        "{hosts:?}"
    );
    assert!(hosts.iter().all(|h| h.state == HostState::Live));
    assert_eq!(rig.launcher.list("u").unwrap().len(), 1);
    assert!(rig.launcher.list("nobody").unwrap().is_empty());
    rig.launcher.stop(&a.unit).unwrap();
    assert!(wait_until(Duration::from_secs(5), || rig
        .launcher
        .list("t")
        .unwrap()
        .len()
        == 1));
    assert_eq!(rig.launcher.list("t").unwrap()[0].unit, b.unit);
    let _ = other;
}

#[test]
fn a_host_whose_child_exited_with_no_client_attached_reports_the_exit_in_hello() {
    let rig = Rig::new();
    let s = rig.start(
        "d5",
        &record(&["/bin/sh", "-c", "echo gone; exit 5"], pipes()),
    );
    let ok = wait_until(Duration::from_secs(5), || {
        client::connect(&s.socket, KEY)
            .map(|(_, h)| h.exit.is_some())
            .unwrap_or(false)
    });
    assert!(ok, "no hello carried the exit");
    let (mut conn, hello) = client::connect(&s.socket, KEY).unwrap();
    let exit = hello.exit.unwrap();
    assert_eq!((exit.code, exit.signal, exit.at), (Some(5), None, 5));
    // The host is still there, holding the output.
    assert_eq!(rig.launcher.state(&s.unit), HostState::Live);
    conn.read_from(0).unwrap();
    assert_eq!(
        conn.next_event().unwrap(),
        Event::Out {
            at: 0,
            data: b"gone\n".to_vec()
        }
    );
}

#[test]
fn sigterm_to_the_host_ends_the_childs_whole_group_and_leaves_the_exit_file() {
    let rig = Rig::new();
    let script = "sh -c 'echo inner:$$; exec sleep 60'; echo not-reached";
    let s = rig.start("d6", &record(&["/bin/sh", "-c", script], pipes()));
    let (mut conn, hello) = client::connect(&s.socket, KEY).unwrap();
    conn.read_from(0).unwrap();
    let inner: u32 = loop {
        match conn.next_event() {
            Ok(Event::Out { data, .. }) => {
                break String::from_utf8(data)
                    .unwrap()
                    .trim()
                    .strip_prefix("inner:")
                    .unwrap()
                    .parse()
                    .unwrap();
            }
            Ok(_) | Err(ClientError::Timeout) => {}
            Err(e) => panic!("{e}"),
        }
    };
    assert!(pid_alive(inner) && pid_alive(hello.child_pid));
    unsafe { libc::kill(s.pid as i32, libc::SIGTERM) };
    assert!(wait_gone(&rig, &s.unit), "the host did not end");
    assert!(!pid_alive(hello.child_pid), "the child survived the host");
    assert!(!pid_alive(inner), "the child's group survived the host");
    let file = rig.wait_exit_file(&s.unit, Duration::from_secs(5));
    assert_eq!(file["reason"], "terminated");
    assert_eq!(file["exit"]["signal"], libc::SIGTERM);
}

#[test]
fn a_child_that_ignores_sigterm_is_killed_after_five_seconds() {
    let rig = Rig::new();
    let script = "trap '' TERM; echo ready; while :; do sleep 1; done";
    let s = rig.start("d7", &record(&["/bin/sh", "-c", script], pipes()));
    let (mut conn, hello) = client::connect(&s.socket, KEY).unwrap();
    conn.read_from(0).unwrap();
    assert!(matches!(conn.next_event().unwrap(), Event::Out { .. }));
    let started = Instant::now();
    unsafe { libc::kill(s.pid as i32, libc::SIGTERM) };
    assert!(wait_gone(&rig, &s.unit), "the host did not end");
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(4),
        "the child was killed after {took:?}, before the grace"
    );
    assert!(!pid_alive(hello.child_pid));
    let file = rig.wait_exit_file(&s.unit, Duration::from_secs(5));
    assert_eq!(file["exit"]["signal"], libc::SIGKILL);
}
