//! AC-7 of `r2.s11.w3`: the app channel. A paired device with a stream open gets each notice as a
//! `notice` frame, and the controller sends it again, on a clock that doubles up to 15 minutes,
//! until the device acknowledges it with `notice_ack` or the request closes (ADR-0012's amendment
//! of 2026-11-21, `docs/contracts/api.md`). The spine-close rig (a `FakeAdapter` and a `BareForge`)
//! has the API on `127.0.0.1:0`, and `pf_client::transport::Transport` over a `FileStore` is paired
//! over pinned TLS. The wait of a re-send is the controller's notice clock, which the test moves:
//! no test sleeps for it. Nothing here reaches beyond `127.0.0.1` or starts `claude`.

mod common;
#[path = "common/needs_you_rig.rs"]
mod rig;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::chat::{controller_ledger, lines};
use pf_api::types::{NoticeAckArgs, NoticeFrame, NoticeOutcome, NoticeRecord, RequestFrame};
use pf_client::error::ErrorKind;
use pf_client::store::{DeviceStore, FileStore};
use pf_client::stream::{ChatStream, StreamEvent, StreamOptions, StreamTuning};
use pf_client::transport::Transport;
use pf_engine::port::store::Store;
use serde_json::json;

/// A client over a tokio runtime, as `client_requests.rs` has it.
struct Client {
    rt: tokio::runtime::Runtime,
    transport: Transport<FileStore>,
    store: Arc<FileStore>,
    _dir: tempfile::TempDir,
}

fn client(api: &rig::Api) -> Client {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    let transport = Transport::new(store.clone()).with_stream_tuning(StreamTuning {
        schedule: vec![Duration::from_millis(50), Duration::from_millis(100)],
        reset_after: Duration::from_secs(60),
        silence: Duration::from_secs(30),
    });
    rt.block_on(transport.pair(&api.pair_code(), "MacBook"))
        .unwrap();
    Client {
        rt,
        transport,
        store,
        _dir: dir,
    }
}

/// What a stream said, in order: the request frames and the notice frames.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum Said {
    Requests,
    Notice(NoticeRecord),
}

impl Client {
    fn token(&self) -> String {
        self.store.load().unwrap().unwrap().token
    }

    fn open(&self) -> ChatStream {
        let _enter = self.rt.enter();
        self.transport.open_stream(StreamOptions::default())
    }

    /// Wait until the stream said `n` more things, and answer them.
    fn said(&self, stream: &mut ChatStream, n: usize) -> Vec<Said> {
        self.rt.block_on(async {
            let end = tokio::time::Instant::now() + Duration::from_secs(20);
            let mut out = Vec::new();
            while out.len() < n {
                let left = end.saturating_duration_since(tokio::time::Instant::now());
                match tokio::time::timeout(left, stream.next()).await {
                    Ok(Some(StreamEvent::NoticeFrame {
                        frame: NoticeFrame::Notice { notice },
                    })) => out.push(Said::Notice(notice)),
                    Ok(Some(StreamEvent::RequestFrame {
                        frame: RequestFrame::Requests { .. },
                    })) => out.push(Said::Requests),
                    Ok(Some(StreamEvent::RequestFrame { .. } | StreamEvent::Frame { .. })) => {}
                    Ok(other) => panic!("{other:?} while waiting for {n} frames"),
                    Err(_) => panic!("timed out waiting for {n} frames; got {out:?}"),
                }
            }
            out
        })
    }

    /// The notice frames that come within `wait`, and no more.
    fn notices_within(&self, stream: &mut ChatStream, wait: Duration) -> Vec<NoticeRecord> {
        self.rt.block_on(async {
            let end = tokio::time::Instant::now() + wait;
            let mut out = Vec::new();
            loop {
                let left = end.saturating_duration_since(tokio::time::Instant::now());
                match tokio::time::timeout(left, stream.next()).await {
                    Ok(Some(StreamEvent::NoticeFrame {
                        frame: NoticeFrame::Notice { notice },
                    })) => out.push(notice),
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => return out,
                }
            }
        })
    }

    fn ack(&self, position: u64, outcome: NoticeOutcome) -> pf_api::types::NoticeAcked {
        self.rt
            .block_on(
                self.transport
                    .notice_ack(NoticeAckArgs { position, outcome }),
            )
            .unwrap()
    }
}

fn notice_of(said: &Said) -> &NoticeRecord {
    match said {
        Said::Notice(n) => n,
        other => panic!("wanted a notice, got {other:?}"),
    }
}

/// One tick, and the clock moved forward first.
fn tick_after(api: &rig::Api, seconds: u64) {
    api.s
        .live
        .controller
        .advance_notice_clock(Duration::from_secs(seconds));
    api.s.tick();
}

fn targets(api: &rig::Api) -> Vec<pf_engine::port::store::NoticeTargetRow> {
    api.s.live.controller.store().notice_targets().unwrap()
}

/// Revise the open request, so that a second notice follows.
fn revise(api: &rig::Api, id: &str) {
    let row = api.s.live.controller.store().request(id).unwrap().unwrap();
    let mut body: serde_json::Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!(row.revision);
    api.s
        .live
        .handle()
        .engine
        .revise_request(id, row.revision, &body)
        .unwrap();
    api.s.tick();
}

fn ledger_of(api: &rig::Api, kind: &str) -> Vec<String> {
    controller_ledger(&api.s.live)
        .into_iter()
        .filter_map(|l| {
            let at = l.find(&format!(" {kind} "))?;
            Some(l[at + 1..].to_string())
        })
        .collect()
}

#[test]
fn a_connected_device_gets_the_notice_frame_after_the_request_frame() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    // `hello`'s companion first: the open requests, none yet.
    let first = c.said(&mut stream, 1);
    assert!(matches!(first[0], Said::Requests), "{first:?}");
    let id = api.open_stop("which branch?");
    let got = c.said(&mut stream, 1);
    let n = notice_of(&got[0]);
    assert_eq!((n.position, n.event.as_str(), n.sends), (1, "opened", 1));
    assert_eq!(
        (n.request.as_str(), n.revision, n.kind.as_str()),
        (id.as_str(), 1, "stop")
    );
    assert_eq!(n.unit.as_deref(), Some("u1"));
    assert_eq!(n.cause.as_deref(), Some("stop_file"));
    assert_eq!(n.project, None);
    assert!(n.text.contains("u1"), "{}", n.text);
    // The first stream joined the device at the newest position, with the outcome `joined`.
    let t = targets(&api);
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(
        (t[0].target.as_str(), t[0].outcome.as_str()),
        ("device:D-1", "joined")
    );
    assert_eq!(t[0].acked, 0);
}

#[test]
fn a_first_stream_joins_at_the_newest_position_and_gets_no_old_notice() {
    let api = rig::api();
    api.open_stop("which branch?");
    assert_eq!(api.s.live.controller.store().newest_notice().unwrap(), 1);
    let c = client(&api);
    let mut stream = c.open();
    // The `requests` snapshot tells it of the open request; no old notice follows.
    let got = c.said(&mut stream, 1);
    assert!(matches!(got[0], Said::Requests), "{got:?}");
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(600))
            .is_empty()
    );
    let t = targets(&api);
    assert_eq!((t[0].acked, t[0].outcome.as_str()), (1, "joined"));
    assert_eq!(
        ledger_of(&api, "notice_ack"),
        vec!["notice_ack device:D-1 position=1 outcome=joined"]
    );
}

#[test]
fn a_new_stream_gets_the_requests_frame_then_its_waiting_notices_oldest_first() {
    let api = rig::api();
    let c = client(&api);
    let mut first = c.open();
    c.said(&mut first, 1);
    // Two notices the device never acknowledges: a stop, then its revision.
    let id = api.open_stop("which branch?");
    c.said(&mut first, 1);
    revise(&api, &id);
    c.said(&mut first, 1);
    drop(first);
    std::thread::sleep(Duration::from_millis(200));
    let mut again = c.open();
    let got = c.said(&mut again, 3);
    assert!(matches!(got[0], Said::Requests), "{got:?}");
    let (a, b) = (notice_of(&got[1]), notice_of(&got[2]));
    assert_eq!((a.position, a.sends), (1, 2), "a reconnect sends at once");
    assert_eq!((b.position, b.sends), (2, 2));
}

#[test]
fn an_unacknowledged_notice_comes_again_after_the_wait_and_the_wait_doubles() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    api.open_stop("which branch?");
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 1);
    // `notify.resend_seconds` is 60: not due at 59 s.
    tick_after(&api, 59);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    tick_after(&api, 2);
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 2);
    // The wait is now 120 s.
    tick_after(&api, 100);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    tick_after(&api, 21);
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 3);
    // Each send is one `notice_send` line, with the attempt.
    let sends = ledger_of(&api, "notice_send");
    assert_eq!(
        sends,
        vec![
            "notice_send 1 target=device:D-1 attempt=1",
            "notice_send 1 target=device:D-1 attempt=2",
            "notice_send 1 target=device:D-1 attempt=3",
        ]
    );
}

#[test]
fn the_wait_stops_growing_at_fifteen_minutes_and_resend_seconds_applies_at_the_next_resend() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    api.open_stop("which branch?");
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 1);
    // The value is read at each re-send: a change before the next one rules it.
    let out = api
        .s
        .live
        .cli(&["settings", "set", "notify.resend_seconds", "600"]);
    assert_eq!(common::code(&out), 0, "{}", common::stderr(&out));
    tick_after(&api, 599);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    tick_after(&api, 2);
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 2);
    // 600 doubled is 1200, and the cap is 900.
    tick_after(&api, 899);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    tick_after(&api, 2);
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 3);
    // 600 times four is 2400, and the cap is 900 still.
    tick_after(&api, 899);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    tick_after(&api, 2);
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 4);
}

#[test]
fn shown_ends_the_resend_and_a_second_ack_admits_nothing() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    api.open_stop("which branch?");
    c.said(&mut stream, 1);
    let acked = c.ack(1, NoticeOutcome::Shown);
    assert!(acked.admitted);
    assert_eq!((acked.device.as_str(), acked.position), ("D-1", 1));
    tick_after(&api, 1000);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    let again = c.ack(1, NoticeOutcome::Shown);
    assert!(
        !again.admitted,
        "a position that does not rise admits nothing"
    );
    assert_eq!(
        ledger_of(&api, "notice_ack").len(),
        2,
        "the join and the first ack"
    );
    let t = targets(&api);
    assert_eq!((t[0].acked, t[0].outcome.as_str()), (1, "shown"));
    // One `api.ledger` line for each `notice_ack` call.
    let api_lines: Vec<String> = lines(&api.s.live.sb.state.join("api.ledger"))
        .into_iter()
        .filter(|l| l.contains("verb=notice_ack"))
        .collect();
    assert_eq!(api_lines.len(), 2, "{api_lines:?}");
}

#[test]
fn blocked_ends_the_resend_and_status_lists_the_device() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    let id = api.open_stop("which branch?");
    c.said(&mut stream, 1);
    let v = api.s.live.sb.status_json("");
    assert_eq!(v["notices"]["waiting"][0]["target"], "device:D-1", "{v}");
    assert_eq!(v["notices"]["waiting"][0]["sends"], 1);
    c.ack(1, NoticeOutcome::Blocked);
    tick_after(&api, 1000);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    let v = api.s.live.sb.status_json("");
    assert_eq!(v["notices"]["waiting"], json!([]), "{v}");
    assert_eq!(v["notices"]["blocked"], json!(["D-1"]), "{v}");
    let text = common::stdout(&api.s.live.cli(&["status"]));
    assert!(
        text.contains("D-1") && text.contains("notification"),
        "{text}"
    );
    // The next `shown` takes it off the list.
    revise(&api, &id);
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).position, 2);
    c.ack(2, NoticeOutcome::Shown);
    assert_eq!(
        api.s.live.sb.status_json("")["notices"]["blocked"],
        json!([])
    );
}

#[test]
fn a_position_above_the_newest_notice_is_refused() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    api.open_stop("which branch?");
    c.said(&mut stream, 1);
    let err =
        c.rt.block_on(c.transport.notice_ack(NoticeAckArgs {
            position: 2,
            outcome: NoticeOutcome::Shown,
        }))
        .unwrap_err();
    assert!(err.message.contains("above the newest notice"), "{err:?}");
    assert_eq!(targets(&api)[0].acked, 0);
}

#[test]
fn a_revoked_device_gets_device_revoked_its_stream_ends_and_no_send_follows() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    let out = api.s.live.cli(&["device", "revoke", "D-1"]);
    assert_eq!(common::code(&out), 0, "{}", common::stderr(&out));
    let ended = c.rt.block_on(async {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
                Ok(Some(StreamEvent::Closed { error })) => return Some(error),
                Ok(Some(_)) => {}
                _ => return None,
            }
        }
    });
    assert!(ended.is_some(), "the stream ended");
    let sends_before = ledger_of(&api, "notice_send").len();
    api.open_stop("which branch?");
    tick_after(&api, 1000);
    assert_eq!(
        ledger_of(&api, "notice_send").len(),
        sends_before,
        "no send follows"
    );
    let err =
        c.rt.block_on(c.transport.notice_ack(NoticeAckArgs {
            position: 1,
            outcome: NoticeOutcome::Shown,
        }))
        .unwrap_err();
    assert!(
        matches!(
            err.kind,
            ErrorKind::Unauthorized | ErrorKind::Forbidden | ErrorKind::Refused
        ),
        "{err:?}"
    );
}

#[test]
fn notice_ack_on_the_socket_is_refused() {
    let api = rig::api();
    let out = pf_ipc::call(
        &api.s.live.sb.state,
        "notice_ack",
        json!({"position": 0, "outcome": "shown"}),
    )
    .unwrap();
    assert!(!out.ok);
    assert!(
        out.error
            .unwrap()
            .contains("notice_ack is for paired devices"),
        "the refusal says so"
    );
}

#[test]
fn a_closed_notice_is_sent_once_and_never_again() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    let id = api.open_stop("which branch?");
    c.said(&mut stream, 1);
    api.s
        .live
        .handle()
        .engine
        .withdraw_request(&id, "nothing will answer")
        .unwrap();
    api.s.tick();
    let got = c.said(&mut stream, 1);
    let closed = notice_of(&got[0]);
    assert_eq!(
        (closed.position, closed.event.as_str(), closed.sends),
        (2, "closed", 1)
    );
    // Nothing is owed: an answered request ends the re-send of its `opened` notice, and the closed
    // one is not sent again.
    tick_after(&api, 5000);
    tick_after(&api, 5000);
    assert!(
        c.notices_within(&mut stream, Duration::from_millis(300))
            .is_empty()
    );
    let sends = ledger_of(&api, "notice_send");
    assert_eq!(sends.len(), 2, "{sends:?}");
}

#[test]
fn an_answered_request_ends_the_resend_of_its_opened_notice() {
    let api = rig::api();
    let c = client(&api);
    let mut stream = c.open();
    c.said(&mut stream, 1);
    let id = api.open_stop("which branch?");
    assert_eq!(notice_of(&c.said(&mut stream, 1)[0]).sends, 1);
    let engine = &api.s.live.handle().engine;
    let granted = engine.claim("tester", &"a".repeat(64)).unwrap();
    engine
        .answer_request(&id, 1, granted.generation, "the left one")
        .unwrap();
    api.s.tick();
    let got = c.said(&mut stream, 1);
    assert_eq!(notice_of(&got[0]).event, "closed");
    tick_after(&api, 5000);
    // The unit resumed and stopped again on the same stop file: that is a new request, with its own
    // notice. The answered request's `opened` notice is not sent again.
    let extra = c.notices_within(&mut stream, Duration::from_millis(300));
    assert!(extra.iter().all(|n| n.request != id), "{extra:?}");
    let waiting = api
        .s
        .live
        .controller
        .store()
        .waiting_deliveries("app")
        .unwrap();
    assert!(waiting.iter().all(|d| d.position != 1), "{waiting:?}");
    assert_eq!(
        ledger_of(&api, "notice_send")
            .iter()
            .filter(|l| l.starts_with("notice_send 1 "))
            .count(),
        1,
        "the opened notice was sent once"
    );
}

#[test]
fn the_stream_reader_acknowledges_each_notice_it_prints_except_the_first_it_withholds() {
    let api = rig::api();
    let c = client(&api);
    let token_file = c._dir.path().join("token");
    std::fs::write(&token_file, c.token()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pf-stream-reader"))
        .args([
            "--url",
            &format!("wss://{}/v1/stream", api.addr),
            "--pin",
            &api.spki,
            "--token-file",
            token_file.to_str().unwrap(),
            "--max-seconds",
            "60",
            "--ack-notices",
            "--withhold-acks",
            "1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let out = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let next = |what: &str| -> serde_json::Value {
        let line = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("the reader printed nothing for {what}"));
        serde_json::from_str(&line).unwrap()
    };
    // `hello`, then the `requests` frame.
    assert_eq!(next("hello")["type"], "hello");
    assert_eq!(next("requests")["type"], "requests");
    api.open_stop("which branch?");
    let first = loop {
        let v = next("the first send");
        if v["type"] == "notice" {
            break v;
        }
    };
    assert_eq!(first["notice"]["position"], 1);
    assert_eq!(first["notice"]["sends"], 1);
    // The first frame was withheld: nothing is acknowledged yet.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(targets(&api)[0].acked, 0);
    tick_after(&api, 61);
    let second = loop {
        let v = next("the second send");
        if v["type"] == "notice" {
            break v;
        }
    };
    assert_eq!(second["notice"]["position"], 1);
    assert_eq!(second["notice"]["sends"], 2);
    // It acknowledged the second send.
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while targets(&api)[0].acked != 1 {
        assert!(
            std::time::Instant::now() < end,
            "the reader never acknowledged"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(targets(&api)[0].outcome, "shown");
    let _ = child.kill();
    let _ = child.wait();
}
