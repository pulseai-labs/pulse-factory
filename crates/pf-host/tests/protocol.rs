//! AC-2: the host protocol, version 1, against a real host started by the detached launcher.

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{KEY, Raw, Rig, attach_line, pid_alive, pipes, pty, record, wait_until};
use pf_host::client::{self, ClientError, Conn, Event};
use pf_host::launcher::{HostState, Launcher};
use pf_host::protocol::{ExitInfo, FRAME_MAX, SignalKind};

fn connect(started: &pf_host::launcher::Started) -> (Conn, pf_host::protocol::Hello) {
    client::connect(&started.socket, KEY).expect("attach")
}

/// Read events until `done` says the bytes so far are enough, or the time is up.
fn read_until(conn: &mut Conn, done: impl Fn(&[u8]) -> bool, within: Duration) -> Vec<u8> {
    let end = Instant::now() + within;
    let mut bytes = Vec::new();
    while !done(&bytes) {
        assert!(
            Instant::now() < end,
            "waited {within:?}; got {:?}",
            String::from_utf8_lossy(&bytes)
        );
        match conn.next_event() {
            Ok(Event::Out { data, .. }) => bytes.extend_from_slice(&data),
            Ok(_) => {}
            Err(ClientError::Timeout) => {}
            Err(e) => panic!("read failed: {e}"),
        }
    }
    bytes
}

/// Read to the `exit` message. Returns the output and the exit.
fn read_to_exit(conn: &mut Conn) -> (Vec<u8>, ExitInfo) {
    let end = Instant::now() + Duration::from_secs(10);
    let mut bytes = Vec::new();
    loop {
        assert!(Instant::now() < end, "no exit within 10 s");
        match conn.next_event() {
            Ok(Event::Out { data, .. }) => bytes.extend_from_slice(&data),
            Ok(Event::Exit(exit)) => return (bytes, exit),
            Ok(_) => {}
            Err(ClientError::Timeout) => {}
            Err(e) => panic!("read failed: {e}"),
        }
    }
}

#[test]
fn a_new_connection_gets_only_version_before_attach() {
    let rig = Rig::new();
    let s = rig.start("p1", &record(&["/bin/sh", "-c", "sleep 30"], pipes()));
    let mut raw = Raw::connect(&s.socket);
    let first = raw.json();
    assert_eq!(first["t"], "version");
    assert_eq!(first["proto_min"], 1);
    assert_eq!(first["proto_max"], 1);
    assert_eq!(
        first.as_object().unwrap().len(),
        3,
        "only the version: {first}"
    );
    // Nothing else leaves the host until a valid attach.
    assert_eq!(
        raw.line(),
        None,
        "the host sent a second line before attach"
    );
}

#[test]
fn attach_with_the_right_key_gets_hello_with_every_field_then_the_stream() {
    let rig = Rig::new();
    let record_json = record(
        &["/bin/sh", "-c", "printf stream-ok; exec sleep 30"],
        pipes(),
    );
    let s = rig.start("p2", &record_json);
    let (mut conn, hello) = connect(&s);
    assert_eq!(hello.proto, 1);
    assert_eq!(hello.session, "p2");
    assert_eq!(hello.incarnation, 1);
    assert_eq!(
        hello.invocation_id, "",
        "the detached launcher clears INVOCATION_ID"
    );
    assert!(
        pid_alive(hello.child_pid),
        "the child pid in hello is not a live process"
    );
    // The child is the host's own child.
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", hello.child_pid)).unwrap();
    let ppid: u32 = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(ppid, s.pid, "the child's parent is not the host");
    assert!(hello.exit.is_none());
    // The record hash is the SHA-256 of the record file, as `sha256sum` prints it.
    let path = rig.tmp.path.join("rec").join("p2.json");
    let out = Command::new("sha256sum").arg(&path).output().unwrap();
    let want = String::from_utf8(out.stdout).unwrap();
    assert_eq!(hello.record_sha256, want.split_whitespace().next().unwrap());
    // The stream starts at cursor 0.
    conn.read_from(0).unwrap();
    match conn.next_event().unwrap() {
        Event::Out { at, data } => {
            assert_eq!(at, 0);
            assert_eq!(data, b"stream-ok");
        }
        other => panic!("expected output, got {other:?}"),
    }
}

#[test]
fn a_version_the_host_does_not_speak_is_refused_with_error() {
    let rig = Rig::new();
    let s = rig.start("p3", &record(&["/bin/sh", "-c", "sleep 30"], pipes()));
    let mut raw = Raw::connect(&s.socket);
    assert_eq!(raw.json()["t"], "version");
    raw.send(format!("{{\"t\":\"attach\",\"proto\":2,\"key\":\"{KEY}\"}}\n").as_bytes());
    let err = raw.json();
    assert_eq!(err["t"], "error");
    assert_eq!(err["code"], "version");
    assert!(raw.closed(), "the host kept the connection open");
}

#[test]
fn input_reaches_a_pipes_child_and_its_output_comes_back() {
    let rig = Rig::new();
    let s = rig.start("p4", &record(&["/bin/cat"], pipes()));
    let (mut conn, _) = connect(&s);
    conn.read_from(0).unwrap();
    conn.input(b"ping\n").unwrap();
    let got = read_until(
        &mut conn,
        |b| b.ends_with(b"ping\n"),
        Duration::from_secs(5),
    );
    assert_eq!(got, b"ping\n");
    conn.ack(5).unwrap();
}

#[test]
fn a_resize_reaches_a_pty_child() {
    let rig = Rig::new();
    let script = "stty size; read x; stty size; read y";
    let s = rig.start("p5", &record(&["/bin/sh", "-c", script], pty(24, 80)));
    let (mut conn, _) = connect(&s);
    conn.read_from(0).unwrap();
    let first = read_until(&mut conn, |b| contains(b, b"24 80"), Duration::from_secs(5));
    assert!(contains(&first, b"24 80"));
    conn.resize(50, 132).unwrap();
    conn.input(b"go\n").unwrap();
    let rest = read_until(
        &mut conn,
        |b| contains(b, b"50 132"),
        Duration::from_secs(5),
    );
    assert!(
        contains(&rest, b"50 132"),
        "{:?}",
        String::from_utf8_lossy(&rest)
    );
}

#[test]
fn a_resize_on_a_pipes_child_is_an_error() {
    let rig = Rig::new();
    let s = rig.start("p5b", &record(&["/bin/cat"], pipes()));
    let (mut conn, _) = connect(&s);
    conn.resize(10, 10).unwrap();
    match conn.next_event().unwrap() {
        Event::Error { code, .. } => assert_eq!(code, "not_pty"),
        other => panic!("expected an error, got {other:?}"),
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn an_interrupt_reaches_the_childs_whole_group() {
    let rig = Rig::new();
    // The outer shell runs an inner shell in the foreground; the inner one prints its pid and
    // becomes `sleep`. A signal sent to the outer pid alone would leave the sleep running.
    let script = "sh -c 'echo inner:$$; exec sleep 60'; echo not-reached";
    let s = rig.start("p6", &record(&["/bin/sh", "-c", script], pipes()));
    let (mut conn, _) = connect(&s);
    conn.read_from(0).unwrap();
    let got = read_until(&mut conn, |b| b.ends_with(b"\n"), Duration::from_secs(5));
    let text = String::from_utf8(got).unwrap();
    let inner: u32 = text.trim().strip_prefix("inner:").unwrap().parse().unwrap();
    assert!(pid_alive(inner));
    conn.signal(SignalKind::Interrupt).unwrap();
    let (_, exit) = read_to_exit(&mut conn);
    assert_eq!(exit.signal, Some(libc::SIGINT), "exit: {exit:?}");
    assert!(
        wait_until(Duration::from_secs(3), || !pid_alive(inner)),
        "the inner process {inner} survived the interrupt"
    );
}

#[test]
fn exit_reports_the_code_and_the_signal() {
    let rig = Rig::new();
    let s = rig.start(
        "p7",
        &record(&["/bin/sh", "-c", "echo bye; exit 7"], pipes()),
    );
    let (mut conn, _) = connect(&s);
    conn.read_from(0).unwrap();
    let (out, exit) = read_to_exit(&mut conn);
    assert_eq!(out, b"bye\n");
    assert_eq!(
        exit,
        ExitInfo {
            code: Some(7),
            signal: None,
            at: 4
        }
    );

    let s = rig.start("p7b", &record(&["/bin/sh", "-c", "kill -9 $$"], pipes()));
    let (mut conn, _) = connect(&s);
    conn.read_from(0).unwrap();
    let (out, exit) = read_to_exit(&mut conn);
    assert!(out.is_empty());
    assert_eq!(
        exit,
        ExitInfo {
            code: None,
            signal: Some(9),
            at: 0
        }
    );
}

#[test]
fn snapshot_answers_unsupported() {
    let rig = Rig::new();
    let s = rig.start("p8", &record(&["/bin/cat"], pipes()));
    let (mut conn, _) = connect(&s);
    conn.snapshot().unwrap();
    match conn.next_event().unwrap() {
        Event::Error { code, .. } => assert_eq!(code, "unsupported"),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn an_exit_is_held_across_a_client_disconnect_and_reported_in_the_next_hello() {
    let rig = Rig::new();
    let s = rig.start(
        "p9",
        &record(&["/bin/sh", "-c", "echo late; exit 3"], pipes()),
    );
    // The first client attaches and leaves without reading.
    let (first, hello) = connect(&s);
    drop(first);
    let mut seen = hello.exit;
    // Wait until the host has seen the child end; a later hello then carries the exit.
    let ok = wait_until(Duration::from_secs(5), || {
        if seen.is_some() {
            return true;
        }
        seen = connect(&s).1.exit;
        seen.is_some()
    });
    assert!(ok, "no hello carried the exit");
    assert_eq!(
        seen,
        Some(ExitInfo {
            code: Some(3),
            signal: None,
            at: 5
        })
    );
    // The unread output is still there.
    let (mut conn, hello) = connect(&s);
    assert!(hello.exit.is_some());
    conn.read_from(0).unwrap();
    let (out, exit) = read_to_exit(&mut conn);
    assert_eq!(out, b"late\n");
    assert_eq!(exit.code, Some(3));
}

#[test]
fn after_the_last_ack_and_exit_ack_the_host_writes_its_exit_file_and_ends_at_once() {
    let rig = Rig::new();
    // A long hold: the host must end on the acknowledgement, not on the hold.
    let s = rig.start_with(
        "p10",
        &record(&["/bin/sh", "-c", "echo bye"], pipes()),
        |spec| {
            spec.exit_hold = Duration::from_secs(600);
        },
    );
    let (mut conn, _) = connect(&s);
    conn.read_from(0).unwrap();
    let (out, exit) = read_to_exit(&mut conn);
    assert_eq!(out, b"bye\n");
    let started = Instant::now();
    conn.ack(exit.at).unwrap();
    conn.exit_ack().unwrap();
    let file = rig.wait_exit_file(&s.unit, Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(file["written_by"], "host");
    assert_eq!(file["reason"], "exit_acked");
    assert_eq!(file["exit"]["code"], 0);
    assert_eq!(file["session"], "p10");
    assert_eq!(file["unread_bytes"], 0);
    assert!(
        wait_until(Duration::from_secs(5), || rig.launcher.state(&s.unit)
            == HostState::Gone),
        "the host did not end"
    );
    let mode = std::fs::metadata(rig.exit_file(&s.unit)).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(mode.permissions().mode() & 0o777, 0o600);
}

#[test]
fn with_no_consumer_the_host_ends_after_the_exit_hold() {
    let rig = Rig::new();
    let s = rig.start_with(
        "p11",
        &record(&["/bin/sh", "-c", "echo bye"], pipes()),
        |spec| {
            spec.exit_hold = Duration::from_secs(1);
        },
    );
    let file = rig.wait_exit_file(&s.unit, Duration::from_secs(10));
    assert_eq!(file["reason"], "hold_expired");
    assert_eq!(file["unread_bytes"], 4);
    assert!(wait_until(Duration::from_secs(5), || rig
        .launcher
        .state(&s.unit)
        == HostState::Gone));
}

#[test]
fn a_frame_never_exceeds_256_kib() {
    let rig = Rig::new();
    let script = "head -c 1048576 /dev/zero";
    let s = rig.start("p12", &record(&["/bin/sh", "-c", script], pipes()));
    let (mut conn, _) = connect(&s);
    conn.read_from(0).unwrap();
    let mut total = 0usize;
    let mut biggest = 0usize;
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < end);
        match conn.next_event() {
            Ok(Event::Out { data, .. }) => {
                biggest = biggest.max(data.len());
                total += data.len();
            }
            Ok(Event::Exit(_)) => break,
            Ok(_) | Err(ClientError::Timeout) => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(total, 1 << 20);
    assert!(biggest <= FRAME_MAX, "a frame held {biggest} bytes");
}

#[test]
fn a_second_valid_attach_supersedes_the_first() {
    let rig = Rig::new();
    let s = rig.start("p13", &record(&["/bin/cat"], pipes()));
    let (mut a, _) = connect(&s);
    a.read_from(0).unwrap();
    let (mut b, _) = connect(&s);
    b.read_from(0).unwrap();
    // The first connection gets `error` `superseded` and is closed.
    match a.next_event() {
        Err(ClientError::Superseded) => {}
        other => panic!("expected the superseded error, got {other:?}"),
    }
    // Whatever the superseded connection sends changes nothing.
    let _ = a.input(b"zzz\n");
    let _ = a.ack(1_000_000);
    b.input(b"ok\n").unwrap();
    let got = read_until(
        &mut b,
        |bytes| bytes.ends_with(b"ok\n"),
        Duration::from_secs(5),
    );
    assert_eq!(
        got, b"ok\n",
        "the superseded connection's input reached the child"
    );
    // The ack of the superseded connection freed nothing: a new read from 0 still has the bytes.
    let (mut c, _) = connect(&s);
    c.read_from(0).unwrap();
    match c.next_event().unwrap() {
        Event::Out { at, data } => {
            assert_eq!(at, 0);
            assert_eq!(data, b"ok\n");
        }
        other => panic!("expected output from 0, got {other:?}"),
    }
}

#[test]
fn a_wrong_key_is_an_error_and_the_connection_closes() {
    let rig = Rig::new();
    let s = rig.start("p14", &record(&["/bin/cat"], pipes()));
    match client::connect(&s.socket, &"0".repeat(64)) {
        Err(ClientError::Refused { code, .. }) => assert_eq!(code, "key"),
        other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
    }
    let mut raw = Raw::connect(&s.socket);
    raw.json();
    raw.send(&attach_line(&"1".repeat(64)));
    assert_eq!(raw.json()["t"], "error");
    assert!(raw.closed());
}
