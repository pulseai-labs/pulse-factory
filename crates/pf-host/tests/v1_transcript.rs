//! AC-3: the golden transcript of protocol version 1 (`tests/fixtures/v1-transcript.jsonl`).
//!
//! The file holds scenarios. A scenario line names the child and the host's settings. The lines
//! after it are the frames of one conversation, in order, tagged with the connection (`c`) and the
//! direction (`d`: `c` is client to host, `h` is host to client). `head` is the header line without
//! its newline, and `data_hex` the raw bytes that follow it.
//!
//! The tests: the host, replayed with the client half, sends the host half; and the client
//! library, driven through the same conversation, sends the client half byte for byte. A later
//! protocol version keeps both green, because a controller speaks the current version and the one
//! before.
//!
//! The comparison of the host half ignores two things that differ on every run: the child's PID in
//! `hello`, and the human text of an `error` (its `code` is compared).

mod common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::time::Duration;

use common::{KEY, Raw, Rig, pipes, wait_until};
use pf_host::client::{self, Event};
use pf_host::protocol::{ClientMsg, SignalKind, encode_client, take_client};
use serde_json::Value;

struct Line {
    conn: usize,
    from_client: bool,
    head: String,
    data: Vec<u8>,
}

impl Line {
    /// The bytes on the wire: the header, a newline and the raw bytes.
    fn wire(&self) -> Vec<u8> {
        let mut w = self.head.clone().into_bytes();
        w.push(b'\n');
        w.extend_from_slice(&self.data);
        w
    }
}

struct Scenario {
    name: String,
    session: String,
    record: String,
    replay_bytes: usize,
    wait_exit: bool,
    key: String,
    lines: Vec<Line>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn load() -> Vec<Scenario> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/v1-transcript.jsonl"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let mut out: Vec<Scenario> = Vec::new();
    for raw in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(raw).unwrap();
        if let Some(name) = v.get("scenario") {
            out.push(Scenario {
                name: name.as_str().unwrap().to_string(),
                session: v["session"].as_str().unwrap().to_string(),
                record: v["record"].as_str().unwrap().to_string(),
                replay_bytes: v["replay_bytes"].as_u64().unwrap() as usize,
                wait_exit: v["wait_exit"].as_bool().unwrap(),
                key: v["key"].as_str().unwrap().to_string(),
                lines: Vec::new(),
            });
        } else {
            out.last_mut()
                .expect("a frame before any scenario")
                .lines
                .push(Line {
                    conn: v["c"].as_u64().unwrap() as usize,
                    from_client: v["d"] == "c",
                    head: v["head"].as_str().unwrap().to_string(),
                    data: unhex(v["data_hex"].as_str().unwrap()),
                });
        }
    }
    out
}

/// A header with the parts that differ per run set to fixed values.
fn normalize(head: &str) -> Value {
    let mut v: Value = serde_json::from_str(head).unwrap_or_else(|e| panic!("{e}: {head}"));
    match v["t"].as_str() {
        Some("hello") => v["child_pid"] = 0.into(),
        Some("error") => v["text"] = "".into(),
        _ => {}
    }
    v
}

#[test]
fn the_fixture_holds_every_message_kind_and_the_refusal_cases() {
    let scenarios = load();
    let mut seen = std::collections::BTreeSet::new();
    let mut codes = std::collections::BTreeSet::new();
    for s in &scenarios {
        for l in &s.lines {
            let v: Value = serde_json::from_str(&l.head).unwrap();
            seen.insert(v["t"].as_str().unwrap().to_string());
            if v["t"] == "error" {
                codes.insert(v["code"].as_str().unwrap().to_string());
            }
        }
    }
    for kind in [
        "version", "hello", "attach", "read", "ack", "input", "resize", "signal", "snapshot",
        "exit_ack", "out", "gap", "exit", "error",
    ] {
        assert!(seen.contains(kind), "the fixture has no {kind} message");
    }
    for code in ["key", "version", "superseded", "unsupported", "not_pty"] {
        assert!(codes.contains(code), "the fixture has no {code} error");
    }
}

#[test]
fn the_clients_encoder_reproduces_every_client_line_byte_for_byte() {
    for s in load() {
        for l in s.lines.iter().filter(|l| l.from_client) {
            let mut buf = l.wire();
            let (msg, body) = take_client(&mut buf)
                .unwrap_or_else(|e| panic!("{}: {e:?}", l.head))
                .unwrap_or_else(|| panic!("{}: not a whole frame", l.head));
            assert!(buf.is_empty());
            assert_eq!(
                encode_client(&msg, &body),
                l.wire(),
                "scenario {}: the encoder does not reproduce {}",
                s.name,
                l.head
            );
        }
    }
}

#[test]
fn the_host_sends_the_host_half_when_it_gets_the_client_half() {
    for s in load() {
        let rig = Rig::new();
        let argv_record = s.record.clone();
        let started = rig.start_with(&s.session, &argv_record, |spec| {
            spec.replay_bytes = s.replay_bytes;
            spec.attach_key = s.key.clone();
            spec.exit_hold = Duration::from_secs(600);
        });
        // The record file the rig wrote is the fixture's text, so the record hash is fixed.
        assert_eq!(
            std::fs::read_to_string(rig.tmp.path.join("rec").join(format!("{}.json", s.session)))
                .unwrap(),
            s.record
        );
        if s.wait_exit {
            let ok = wait_until(Duration::from_secs(5), || {
                client::connect(&started.socket, &s.key)
                    .map(|(_, h)| h.exit.is_some())
                    .unwrap_or(false)
            });
            assert!(ok, "scenario {}: the child did not end", s.name);
        }
        let mut conns: HashMap<usize, Raw> = HashMap::new();
        for l in &s.lines {
            // A connection opens at its first line.
            let raw = conns
                .entry(l.conn)
                .or_insert_with(|| Raw::connect(&started.socket));
            if l.from_client {
                raw.send(&l.wire());
                continue;
            }
            let got = raw.line_within(Duration::from_secs(5)).unwrap_or_else(|| {
                panic!("scenario {}: the host sent nothing for {}", s.name, l.head)
            });
            assert_eq!(
                normalize(&got),
                normalize(&l.head),
                "scenario {}: the host's frame differs from the fixture",
                s.name
            );
            if let Some(n) = serde_json::from_str::<Value>(&l.head).unwrap().get("n") {
                let n = n.as_u64().unwrap() as usize;
                let data = raw.exact(n, Duration::from_secs(5)).expect("the raw bytes");
                assert_eq!(data, l.data, "scenario {}: the bytes of {}", s.name, l.head);
            }
            if l.head.contains("\"code\":\"key\"") || l.head.contains("\"code\":\"version\"") {
                // A refusal closes the connection.
                assert!(
                    raw.closed(),
                    "scenario {}: the host kept a refused connection",
                    s.name
                );
            }
        }
    }
}

#[test]
fn the_client_library_sends_the_client_half_of_the_main_scenario_byte_for_byte() {
    let scenarios = load();
    let main = scenarios.iter().find(|s| s.name == "main").unwrap();
    let dir = common::Tmp::new();
    let socket = dir.path.join("h.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let lines: Vec<(bool, Vec<u8>)> = main
        .lines
        .iter()
        .map(|l| (l.from_client, l.wire()))
        .collect();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut recorded = Vec::new();
        for (from_client, wire) in lines {
            if from_client {
                let mut got = vec![0u8; wire.len()];
                stream.read_exact(&mut got).unwrap();
                recorded.extend_from_slice(&got);
            } else {
                stream.write_all(&wire).unwrap();
            }
        }
        recorded
    });

    let (mut conn, hello) = client::connect(&socket, KEY).unwrap();
    assert_eq!(hello.session, "g1");
    conn.read_from(0).unwrap();
    assert_eq!(
        conn.next_event().unwrap(),
        Event::Out {
            at: 0,
            data: b"ready\n".to_vec()
        }
    );
    conn.input(b"hi\n").unwrap();
    assert_eq!(
        conn.next_event().unwrap(),
        Event::Out {
            at: 6,
            data: b"echo:hi\n".to_vec()
        }
    );
    conn.ack(14).unwrap();
    conn.snapshot().unwrap();
    assert!(
        matches!(conn.next_event().unwrap(), Event::Error { code, .. } if code == "unsupported")
    );
    conn.resize(30, 100).unwrap();
    assert!(matches!(conn.next_event().unwrap(), Event::Error { code, .. } if code == "not_pty"));
    conn.signal(SignalKind::Interrupt).unwrap();
    assert!(
        matches!(conn.next_event().unwrap(), Event::Exit(e) if e.signal == Some(2) && e.at == 14)
    );
    conn.exit_ack().unwrap();

    let recorded = server.join().unwrap();
    let expected: Vec<u8> = main
        .lines
        .iter()
        .filter(|l| l.from_client)
        .flat_map(|l| l.wire())
        .collect();
    assert_eq!(
        String::from_utf8_lossy(&recorded),
        String::from_utf8_lossy(&expected)
    );
    let _ = (pipes, ClientMsg::Snapshot);
}
