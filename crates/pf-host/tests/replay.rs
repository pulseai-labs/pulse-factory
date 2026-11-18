//! AC-4: the replay buffer through a real host: a fast reader, a reconnect, an overflow, an ack.

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{KEY, Rig, pipes, record, wait_until};
use pf_host::client::{self, ClientError, Conn, Event};

fn connect(started: &pf_host::launcher::Started) -> Conn {
    client::connect(&started.socket, KEY).expect("attach").0
}

/// Read events until `total` output bytes arrived or the time is up. Returns every event.
fn collect(conn: &mut Conn, total: usize, within: Duration) -> Vec<Event> {
    let end = Instant::now() + within;
    let mut events = Vec::new();
    let mut got = 0usize;
    while got < total {
        assert!(
            Instant::now() < end,
            "only {got} of {total} bytes in {within:?}"
        );
        match conn.next_event() {
            Ok(e) => {
                if let Event::Out { data, .. } = &e {
                    got += data.len();
                }
                events.push(e);
            }
            Err(ClientError::Timeout) => {}
            Err(e) => panic!("{e}"),
        }
    }
    events
}

fn out_bytes(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Out { data, .. } => Some(data.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn four_mib_reach_a_fast_reader_in_full_and_in_order_with_no_stall() {
    // The first host of Spike A stopped at exactly 1 MiB: its write buffer filled and it stopped
    // polling for writes.
    let script = "seq 1 800000 | head -c 4194304";
    let want = Command::new("sh")
        .args(["-c", script])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(want.len(), 4 << 20);
    let rig = Rig::new();
    let s = rig.start_with("r1", &record(&["/bin/sh", "-c", script], pipes()), |spec| {
        spec.replay_bytes = 8 << 20;
    });
    let mut conn = connect(&s);
    conn.read_from(0).unwrap();
    let started = Instant::now();
    let events = collect(&mut conn, want.len(), Duration::from_secs(20));
    let took = started.elapsed();
    assert!(
        !events.iter().any(|e| matches!(e, Event::Gap { .. })),
        "a gap in a 4 MiB read"
    );
    let mut at = 0u64;
    for e in &events {
        if let Event::Out { at: a, data } = e {
            assert_eq!(*a, at, "output out of order");
            at += data.len() as u64;
        }
    }
    assert!(
        out_bytes(&events) == want,
        "the bytes differ from what the child wrote"
    );
    assert!(took < Duration::from_secs(15), "the read took {took:?}");

    // The same 4 MiB again, as a replay: the child has ended, all of it sits in the buffer, and a
    // new reader asks for it from 0. This is the case that stalled at 1 MiB in the spike: the
    // connection's write queue filled, emptied, and nothing woke it to fill it again.
    drop(conn);
    let ended = wait_until(Duration::from_secs(5), || {
        client::connect(&s.socket, KEY)
            .map(|(_, h)| h.exit.is_some())
            .unwrap_or(false)
    });
    assert!(ended, "the child did not end");
    let mut replay = connect(&s);
    replay.read_from(0).unwrap();
    let events = collect(&mut replay, want.len(), Duration::from_secs(20));
    assert!(!events.iter().any(|e| matches!(e, Event::Gap { .. })));
    assert!(
        out_bytes(&events) == want,
        "the replay differs from what the child wrote"
    );
}

#[test]
fn a_reader_that_reconnects_from_its_last_ack_gets_every_later_byte_once() {
    let rig = Rig::new();
    let script = "for i in 1 2 3 4 5 6; do echo line$i; sleep 0.05; done; sleep 30";
    let s = rig.start("r2", &record(&["/bin/sh", "-c", script], pipes()));
    let mut first = connect(&s);
    first.read_from(0).unwrap();
    let got = collect(&mut first, 18, Duration::from_secs(5));
    assert!(out_bytes(&got).starts_with(b"line1\nline2\nline3\n"));
    // It consumed two lines and says so, then leaves.
    first.ack(12).unwrap();
    drop(first);
    // Wait until the child has written all six lines.
    std::thread::sleep(Duration::from_millis(600));
    let mut second = connect(&s);
    second.read_from(12).unwrap();
    let events = collect(&mut second, 24, Duration::from_secs(5));
    assert!(!events.iter().any(|e| matches!(e, Event::Gap { .. })));
    assert_eq!(out_bytes(&events), b"line3\nline4\nline5\nline6\n");
    // Reading on finds nothing twice.
    second.set_timeout(Duration::from_millis(300));
    assert!(matches!(second.next_event(), Err(ClientError::Timeout)));
}

#[test]
fn an_overflow_before_ack_gives_one_gap_with_the_right_bounds_then_the_kept_bytes() {
    let rig = Rig::new();
    let script = "printf 0123456789012345678901234567890123456789";
    let s = rig.start_with("r3", &record(&["/bin/sh", "-c", script], pipes()), |spec| {
        spec.replay_bytes = 16;
    });
    assert!(wait_until(Duration::from_secs(5), || client::connect(
        &s.socket, KEY
    )
    .map(|(_, h)| h.exit.is_some())
    .unwrap_or(false)));
    let mut conn = connect(&s);
    conn.read_from(0).unwrap();
    assert_eq!(conn.next_event().unwrap(), Event::Gap { from: 0, to: 24 });
    assert_eq!(
        conn.next_event().unwrap(),
        Event::Out {
            at: 24,
            data: b"4567890123456789".to_vec()
        }
    );
    assert!(matches!(conn.next_event().unwrap(), Event::Exit(e) if e.at == 40));
    // A read from inside the kept range gets no gap.
    conn.read_from(30).unwrap();
    assert_eq!(
        conn.next_event().unwrap(),
        Event::Out {
            at: 30,
            data: b"0123456789".to_vec()
        }
    );
}

#[test]
fn ack_frees_space() {
    let rig = Rig::new();
    // 16 bytes, then (on input) 16 more. With a 16-byte buffer the second run overflows unless the
    // first was acknowledged.
    let script = "printf AAAAAAAAAAAAAAAA; read x; printf BBBBBBBBBBBBBBBB; sleep 30";
    let s = rig.start_with("r4", &record(&["/bin/sh", "-c", script], pipes()), |spec| {
        spec.replay_bytes = 16;
    });
    let mut conn = connect(&s);
    conn.read_from(0).unwrap();
    let first = collect(&mut conn, 16, Duration::from_secs(5));
    assert_eq!(out_bytes(&first), b"AAAAAAAAAAAAAAAA");
    conn.ack(16).unwrap();
    conn.input(b"go\n").unwrap();
    let second = collect(&mut conn, 16, Duration::from_secs(5));
    assert!(
        !second.iter().any(|e| matches!(e, Event::Gap { .. })),
        "the ack freed no space"
    );
    assert_eq!(out_bytes(&second), b"BBBBBBBBBBBBBBBB");
    // The host kept only what is unacknowledged: a read from 0 finds a gap up to the ack.
    let mut other = connect(&s);
    other.read_from(0).unwrap();
    assert_eq!(other.next_event().unwrap(), Event::Gap { from: 0, to: 16 });
    assert_eq!(
        other.next_event().unwrap(),
        Event::Out {
            at: 16,
            data: b"BBBBBBBBBBBBBBBB".to_vec()
        }
    );
}

#[test]
fn an_ack_alone_makes_room_for_output_that_fits_only_because_of_it() {
    // Mutation catch: with `ReplayBuffer::ack` a no-op, `ack_frees_space` still passed, because a
    // 16-byte overflow drops the same oldest bytes an ack would. The two differ in one place: an
    // overflow records a gap, an ack does not. So here 8 bytes fill half the 16-byte buffer, the
    // reader acks them, and 16 more bytes fit only because the ack freed those 8. With no ack the
    // push overflows by 8 and records the gap (0, 8).
    let rig = Rig::new();
    let script = "printf AAAAAAAA; read x; printf BBBBBBBBBBBBBBBB";
    let s = rig.start_with("r5", &record(&["/bin/sh", "-c", script], pipes()), |spec| {
        spec.replay_bytes = 16;
        // Nobody sends `exit_ack`: the host ends on the hold and writes the buffer's gap record.
        spec.exit_hold = Duration::from_secs(1);
    });
    let mut conn = connect(&s);
    conn.read_from(0).unwrap();
    let first = collect(&mut conn, 8, Duration::from_secs(5));
    assert_eq!(out_bytes(&first), b"AAAAAAAA");
    conn.ack(8).unwrap();
    // The ack and the input travel one connection in order; the host takes the ack before it
    // relays the input, so the child's second write meets a buffer that is empty.
    conn.input(b"go\n").unwrap();
    let second = collect(&mut conn, 16, Duration::from_secs(5));
    assert!(
        !second.iter().any(|e| matches!(e, Event::Gap { .. })),
        "the ack freed no space"
    );
    assert_eq!(out_bytes(&second), b"BBBBBBBBBBBBBBBB");
    // The buffer holds all 16 new bytes and the ack's bound is the oldest kept byte.
    let mut other = connect(&s);
    other.read_from(8).unwrap();
    assert_eq!(
        other.next_event().unwrap(),
        Event::Out {
            at: 8,
            data: b"BBBBBBBBBBBBBBBB".to_vec()
        }
    );
    drop(other);
    drop(conn);
    let file = rig.wait_exit_file(&s.unit, Duration::from_secs(10));
    assert_eq!(file["end"], 24);
    assert_eq!(file["acked_through"], 8);
    assert_eq!(file["unread_bytes"], 16);
    assert_eq!(
        file["gaps"],
        serde_json::json!([]),
        "an overflow dropped output the ack should have freed"
    );
}

#[test]
fn the_buffer_keeps_what_an_ack_freed_room_for_and_records_no_gap() {
    use pf_host::buffer::{Read, ReplayBuffer};
    let mut buf = ReplayBuffer::new(16);
    buf.push(b"AAAAAAAA");
    buf.ack(8);
    assert_eq!((buf.base(), buf.end(), buf.len()), (8, 8, 0));
    buf.push(b"BBBBBBBBBBBBBBBB");
    assert!(buf.gaps().is_empty(), "the ack freed no space");
    assert_eq!((buf.base(), buf.end(), buf.len()), (8, 24, 16));
    assert_eq!(buf.read(8, 64), Read::Bytes(b"BBBBBBBBBBBBBBBB".to_vec()));
    // Without the ack the same writes overflow and record a gap.
    let mut no_ack = ReplayBuffer::new(16);
    no_ack.push(b"AAAAAAAA");
    no_ack.push(b"BBBBBBBBBBBBBBBB");
    assert_eq!(no_ack.gaps(), &[(0, 8)]);
}
