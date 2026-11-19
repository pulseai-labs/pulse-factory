//! AC-5: the host's attach checks (the host's half of the `session-trust` peer check and audit trail).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::{KEY, Raw, Rig, attach_line, pipes, record, wait_until};
use pf_host::client::{self, ClientError, Conn, Event};
use pf_host::launcher::{LaunchError, Launcher};

fn refusals_log(rig: &Rig, session: &str) -> String {
    let path = rig
        .rt
        .join("pf-hosts")
        .join("t")
        .join(format!("{session}-1"))
        .join("refusals.log");
    std::fs::read_to_string(path).unwrap_or_default()
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_wrong_key_is_refused_and_so_is_a_missing_one() {
    let rig = Rig::new();
    let s = rig.start("a1", &record(&["/bin/cat"], pipes()));
    match client::connect(&s.socket, &"9".repeat(64)) {
        Err(ClientError::Refused { code, .. }) => assert_eq!(code, "key"),
        other => panic!("a wrong key was not refused: {:?}", other.map(|_| ())),
    }
    // An attach with no `key` field at all.
    let mut raw = Raw::connect(&s.socket);
    raw.json();
    raw.send(b"{\"t\":\"attach\",\"proto\":1}\n");
    let err = raw.json();
    assert_eq!(
        (err["t"].as_str(), err["code"].as_str()),
        (Some("error"), Some("key"))
    );
    assert!(raw.closed());
    // Anything but `attach` first is a refusal too, and no `hello` leaves the host.
    let mut raw = Raw::connect(&s.socket);
    raw.json();
    raw.send(b"{\"t\":\"read\",\"from\":0}\n");
    let err = raw.json();
    assert_eq!(err["t"], "error");
    assert!(raw.closed());
    // The right key still works afterwards.
    client::connect(&s.socket, KEY).expect("the right key");
}

#[test]
fn a_connection_that_never_attaches_is_closed_after_the_attach_wait_and_counted() {
    let rig = Rig::new();
    let s = rig.start_with("a2", &record(&["/bin/cat"], pipes()), |spec| {
        spec.attach_wait = Duration::from_millis(300);
    });
    let mut raw = Raw::connect(&s.socket);
    assert_eq!(raw.json()["t"], "version");
    // The host says why, then closes. It does so close to the wait, not at once and not never.
    let started = std::time::Instant::now();
    let err = raw.json();
    assert_eq!(err["t"], "error");
    assert_eq!(err["code"], "attach_timeout");
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "closed after {:?}",
        started.elapsed()
    );
    assert!(raw.closed());
    assert!(wait_until(Duration::from_secs(2), || refusals_log(
        &rig, "a2"
    )
    .contains("check=attach_timeout")));
    rig.launcher.stop(&s.unit).unwrap();
    let file = rig.wait_exit_file(&s.unit, Duration::from_secs(5));
    assert_eq!(file["refusals"], 1);
}

#[test]
fn a_peer_in_another_cgroup_is_refused() {
    let rig = Rig::new();
    // The host is told the controller is in a cgroup this test is not in.
    let s = rig.start_with("a3", &record(&["/bin/cat"], pipes()), |spec| {
        spec.controller_cgroup = Some("/pf-test/not-this-cgroup".to_string());
    });
    match client::connect(&s.socket, KEY) {
        Err(ClientError::Refused { code, .. }) => assert_eq!(code, "peer"),
        other => panic!(
            "a peer in another cgroup was accepted: {:?}",
            other.map(|_| ())
        ),
    }
    assert!(wait_until(Duration::from_secs(2), || refusals_log(
        &rig, "a3"
    )
    .contains("check=peer")));
    // The same host given this test's own cgroup accepts it, so the refusal was the cgroup check.
    if let Some(own) = pf_host::peer::cgroup_of(std::process::id() as i32) {
        let s = rig.start_with("a3b", &record(&["/bin/cat"], pipes()), |spec| {
            spec.controller_cgroup = Some(own);
        });
        client::connect(&s.socket, KEY).expect("a peer in the controller's cgroup");
    }
}

#[test]
fn each_refusal_is_one_log_line_with_no_key_text_and_counts_in_the_exit_file() {
    let rig = Rig::new();
    let s = rig.start_with("a4", &record(&["/bin/cat"], pipes()), |spec| {
        spec.attach_wait = Duration::from_millis(300);
    });
    let wrong = "d00dfeed".repeat(8);
    let _ = client::connect(&s.socket, &wrong);
    let _ = client::connect_with(&s.socket, KEY, Some(7));
    let mut raw = Raw::connect(&s.socket);
    raw.json();
    raw.send(b"{\"t\":\"attach\",\"proto\":1}\n");
    raw.json();
    let mut idle = Raw::connect(&s.socket);
    idle.json();
    assert!(wait_until(Duration::from_secs(3), || refusals_log(
        &rig, "a4"
    )
    .lines()
    .count()
        == 4));
    let log = refusals_log(&rig, "a4");
    assert_eq!(log.lines().count(), 4, "{log}");
    for line in log.lines() {
        assert!(
            line.contains(&format!("peer_pid={}", std::process::id())),
            "{line}"
        );
        assert!(line.contains("check="), "{line}");
        assert!(
            !line.contains(&wrong) && !line.contains(KEY),
            "a key in the log: {line}"
        );
    }
    for check in ["check=key", "check=version", "check=attach_timeout"] {
        assert!(log.contains(check), "no {check} in {log}");
    }
    let path = rig.rt.join("pf-hosts/t/a4-1/refusals.log");
    assert_eq!(mode(&path), 0o600);
    rig.launcher.stop(&s.unit).unwrap();
    let file = rig.wait_exit_file(&s.unit, Duration::from_secs(5));
    assert_eq!(file["refusals"], 4);
}

#[test]
fn the_hosts_directory_is_0700() {
    let rig = Rig::new();
    let s = rig.start("a5", &record(&["/bin/cat"], pipes()));
    let dir = s.socket.parent().unwrap();
    assert_eq!(mode(dir), 0o700);
    assert_eq!(mode(&s.socket), 0o600);
    assert_eq!(dir, rig.rt.join("pf-hosts").join("t").join("a5-1"));
}

fn read_all_output(conn: &mut Conn, until: &[u8]) -> String {
    conn.read_from(0).unwrap();
    let mut text = Vec::new();
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while !text.windows(until.len()).any(|w| w == until) {
        assert!(
            std::time::Instant::now() < end,
            "no {:?} in {:?}",
            until,
            String::from_utf8_lossy(&text)
        );
        match conn.next_event() {
            Ok(Event::Out { data, .. }) => text.extend_from_slice(&data),
            Ok(_) | Err(ClientError::Timeout) => {}
            Err(e) => panic!("{e}"),
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

#[test]
fn the_secrets_file_is_gone_before_the_child_starts_and_the_child_never_sees_the_key() {
    let rig = Rig::new();
    let unit = "pf-host-t-a6-1";
    let secrets = rig
        .rt
        .join("pulse-factory/secrets")
        .join(format!("{unit}.json"));
    let host_dir = rig.rt.join("pf-hosts/t/a6-1");
    // The child looks for the secrets file, prints its environment, and searches every file it can
    // read under the host's directory and the host's own command line and environment.
    let script = format!(
        "if test -e {sec}; then echo SECRETS-PRESENT; else echo SECRETS-GONE; fi; env; \
         if grep -rq {key} {dir} 2>/dev/null; then echo KEY-IN-DIR; else echo KEY-NOT-IN-DIR; fi; \
         echo END-OF-REPORT; sleep 30",
        sec = secrets.display(),
        key = KEY,
        dir = host_dir.display(),
    );
    let s = rig.start_with(
        "a6",
        &record(&["/bin/sh", "-c", &script], pipes()),
        |spec| {
            spec.secrets.insert(
                "PF_SECRET_TOKEN".to_string(),
                "tok-8c1f-only-for-the-child".to_string(),
            );
        },
    );
    assert_eq!(s.unit, unit);
    let (mut conn, _) = client::connect(&s.socket, KEY).unwrap();
    let report = read_all_output(&mut conn, b"END-OF-REPORT");
    assert!(report.contains("SECRETS-GONE"), "{report}");
    assert!(
        report.contains("PF_SECRET_TOKEN=tok-8c1f-only-for-the-child"),
        "the child lacks its secret"
    );
    assert!(
        report.contains("PF_TEST_VISIBLE=yes"),
        "the child lacks the record's variable"
    );
    assert!(report.contains("KEY-NOT-IN-DIR"), "{report}");
    assert!(
        !report.contains(KEY),
        "the child's environment holds the attach key"
    );
    assert!(!report.contains("NOTIFY_SOCKET") && !report.contains("INVOCATION_ID"));
    assert!(!secrets.exists());
    // The host's own argv and environment hold neither the key nor the secret value.
    for file in ["cmdline", "environ"] {
        let bytes = std::fs::read(format!("/proc/{}/{file}", s.pid)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(KEY),
            "the host's {file} holds the attach key"
        );
        assert!(
            !text.contains("tok-8c1f"),
            "the host's {file} holds a secret value"
        );
    }
}

#[test]
fn a_launch_that_fails_deletes_the_secrets_file_too() {
    let rig = Rig::new();
    let spec = rig.spec("a7", &record(&["/no/such/program"], pipes()));
    let err: LaunchError = rig
        .launcher
        .start(&spec)
        .expect_err("the host cannot start its child");
    assert!(err.to_string().contains("cannot start"), "{err}");
    let dir = rig.rt.join("pulse-factory/secrets");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "a secrets file was left"
    );
    // The host removed its own directory.
    assert!(!rig.rt.join("pf-hosts/t/a7-1").exists());
    let _ = attach_line;
}
