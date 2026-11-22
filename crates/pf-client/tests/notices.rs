//! AC-12 of `r2.s11.w3`: the client half of the notices. A `notice` frame arrives as its own event
//! (`StreamEvent::NoticeFrame`), between other frames and in order, and never moves the `seq` a
//! reconnect asks. A notice frame that does not decode is dropped and the stream goes on. A typed
//! `notice_ack` posts its verb and reads the answer. The host is the real TLS listener and router of
//! `pf-api` on `127.0.0.1:0` over a fake host, as `needs_you.rs` builds it. Nothing leaves the
//! loopback address.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_api::code::{hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::host::{StreamEnd, StreamEvent as Feed, StreamRefusal};
use pf_api::types::{
    ApiStatus, DeviceRecord, NoticeAckArgs, NoticeFrame, NoticeOutcome, PairResult, ServerRef,
};
use pf_api::{ApiHost, Refusal, RequestRecord, router_with, tls};
use pf_client::http::Timeouts;
use pf_client::store::{DeviceStore, FileStore, Paired};
use pf_client::stream::{ChatStream, StreamEvent, StreamOptions, StreamTuning};
use pf_client::transport::Transport;
use serde_json::{Value, json};
use tokio::sync::mpsc;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

fn device() -> DeviceRecord {
    DeviceRecord {
        id: "D-1".to_string(),
        name: "device D-1".to_string(),
        paired_at: "2026-11-07T10:00:00.000Z".to_string(),
        last_seen_at: None,
        last_peer: None,
        revoked_at: None,
    }
}

/// What the fake host saw, and what it answers.
struct Fake {
    spki_hex: String,
    calls: Mutex<Vec<(String, Value)>>,
    answers: Mutex<HashMap<String, pf_ipc::Response>>,
    opens: Mutex<Vec<(Option<String>, u64)>>,
    feeds: Mutex<Vec<mpsc::Sender<Feed>>>,
}

impl ApiHost for Fake {
    fn redeem(&self, _: &str, _: &str, _: IpAddr) -> Result<PairResult, Refusal> {
        Err(Refusal::Code)
    }

    fn device(&self, token_sha256_hex: &str, _peer: IpAddr) -> Option<DeviceRecord> {
        (Some(token_sha256_hex.to_string()) == token_sha256(TOKEN)).then(device)
    }

    fn call(&self, _: &DeviceRecord, verb: &str, args: Value) -> pf_ipc::Response {
        self.calls.lock().unwrap().push((verb.to_string(), args));
        self.answers
            .lock()
            .unwrap()
            .get(verb)
            .cloned()
            .unwrap_or_else(|| pf_ipc::Response::ok(json!({})))
    }

    fn status(&self) -> ApiStatus {
        ApiStatus {
            schema_version: 1,
            version: "0.1.0".to_string(),
            key_spki_sha256: self.spki_hex.clone(),
            device: None,
            server: Some(self.server()),
        }
    }

    fn server(&self) -> ServerRef {
        ServerRef {
            id: Some(self.spki_hex.clone()),
            name: "test-server".to_string(),
        }
    }

    fn audit(&self, _record: &RequestRecord) {}

    fn open_stream(
        &self,
        _device: &DeviceRecord,
        session: Option<&str>,
        after: u64,
    ) -> Result<mpsc::Receiver<Feed>, StreamRefusal> {
        self.opens
            .lock()
            .unwrap()
            .push((session.map(str::to_string), after));
        let (tx, rx) = mpsc::channel(64);
        self.feeds.lock().unwrap().push(tx);
        Ok(rx)
    }
}

impl Fake {
    async fn wait_feed(&self, n: usize) -> mpsc::Sender<Feed> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(tx) = self.feeds.lock().unwrap().get(n) {
                return tx.clone();
            }
            assert!(Instant::now() < end, "feed {n} never opened");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

struct Server {
    fake: Arc<Fake>,
    addr: SocketAddr,
    pin: [u8; 32],
    _dir: tempfile::TempDir,
}

impl Server {
    async fn start() -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let key = tls::load_or_create(&tls::api_dir(dir.path()), &addr).unwrap();
        let fake = Arc::new(Fake {
            spki_hex: key.spki_sha256_hex(),
            calls: Mutex::new(Vec::new()),
            answers: Mutex::new(HashMap::new()),
            opens: Mutex::new(Vec::new()),
            feeds: Mutex::new(Vec::new()),
        });
        let app = router_with(fake.clone(), GuardConfig::default(), system_clock());
        let config = key.server_config().unwrap();
        tokio::spawn(async move {
            pf_api::listener::serve(listener, config, app, std::future::pending()).await;
        });
        Server {
            fake,
            addr,
            pin: key.spki_sha256(),
            _dir: dir,
        }
    }
}

struct Client {
    transport: Transport<FileStore>,
    _dir: tempfile::TempDir,
}

fn client(server: &Server) -> Client {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    store
        .save(&Paired {
            address: server.addr.to_string(),
            pin: hex(&server.pin),
            token: TOKEN.to_string(),
            device: device(),
        })
        .unwrap();
    let transport = Transport::new(store)
        .with_timeouts(Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(5),
        })
        .with_stream_tuning(StreamTuning {
            schedule: vec![Duration::from_millis(50), Duration::from_millis(100)],
            reset_after: Duration::from_secs(60),
            silence: Duration::from_secs(5),
        });
    Client {
        transport,
        _dir: dir,
    }
}

async fn next(stream: &mut ChatStream) -> StreamEvent {
    match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("the stream ended with no event"),
        Err(_) => panic!("no event within 5 s"),
    }
}

async fn next_json(stream: &mut ChatStream) -> Value {
    serde_json::to_value(next(stream).await).unwrap()
}

fn frame(value: Value) -> Feed {
    Feed::Frame(value.to_string())
}

fn notice(position: u64, event: &str, sends: u64) -> Value {
    json!({"type": "notice", "notice": {
        "position": position, "event": event, "request": "R-1", "revision": 1, "run": "s1-1",
        "project": "P-1", "unit": "u1", "kind": "stop", "cause": "stop_file",
        "text": "Unit u1 stopped at stage implement and wrote a stop file.",
        "created_at": "2026-11-21T10:00:00.000Z", "sends": sends
    }})
}

fn item(seq: u64) -> Value {
    json!({"type": "item", "item": {
        "session_id": "S-1", "seq": seq, "answers_seq": null,
        "created_at": "2026-11-10T10:00:00.000Z", "redacted_at": null,
        "body": {"kind": "assistant_text", "text": format!("item {seq}")}
    }})
}

fn options(after: u64) -> StreamOptions {
    StreamOptions {
        session: Some("S-1".to_string()),
        after,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notice_frame_arrives_as_its_own_event_between_other_frames_and_in_order() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options(0));
    let tx = server.fake.wait_feed(0).await;
    let first = notice(1, "opened", 1);
    let again = notice(1, "opened", 2);
    tx.send(frame(item(1))).await.unwrap();
    tx.send(frame(first.clone())).await.unwrap();
    tx.send(frame(json!({"type": "future_thing", "n": 1})))
        .await
        .unwrap();
    tx.send(frame(item(2))).await.unwrap();
    tx.send(frame(again.clone())).await.unwrap();
    tx.send(frame(notice(2, "closed", 1))).await.unwrap();

    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 1);
    // The JSON of the event is `{"event":"notice_frame","frame":{…}}`, the frame as the server sent it.
    assert_eq!(
        next_json(&mut stream).await,
        json!({"event": "notice_frame", "frame": first})
    );
    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 2);
    // The typed side holds the same frame, and a send that repeats keeps its position.
    match next(&mut stream).await {
        StreamEvent::NoticeFrame {
            frame: NoticeFrame::Notice { notice },
        } => assert_eq!((notice.position, notice.sends), (1, 2)),
        other => panic!("wanted the second send, got {other:?}"),
    }
    match next(&mut stream).await {
        StreamEvent::NoticeFrame {
            frame: NoticeFrame::Notice { notice },
        } => assert_eq!((notice.position, notice.event.as_str()), (2, "closed")),
        other => panic!("wanted the closed notice, got {other:?}"),
    }

    // The reconnect's `after` is the last item `seq`: a notice frame does not move it.
    tx.send(Feed::End(StreamEnd::Shutdown)).await.unwrap();
    match next(&mut stream).await {
        StreamEvent::Reconnecting { after, .. } => assert_eq!(after, 2),
        other => panic!("wanted reconnecting, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_notice_frame_that_does_not_decode_is_dropped_and_the_stream_goes_on() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options(0));
    let tx = server.fake.wait_feed(0).await;
    tx.send(frame(item(1))).await.unwrap();
    tx.send(frame(json!({"type": "notice", "notice": "oops"})))
        .await
        .unwrap();
    tx.send(frame(
        json!({"type": "notice", "notice": {"position": "one"}}),
    ))
    .await
    .unwrap();
    tx.send(frame(item(2))).await.unwrap();
    tx.send(frame(notice(3, "opened", 1))).await.unwrap();
    tx.send(frame(item(3))).await.unwrap();

    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 1);
    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 2);
    let good = next_json(&mut stream).await;
    assert_eq!(good["event"], "notice_frame");
    assert_eq!(good["frame"]["notice"]["position"], 3);
    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 3);
    assert_eq!(server.fake.opens.lock().unwrap().len(), 1, "no new open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notice_ack_posts_its_verb_and_reads_the_answer() {
    let server = Server::start().await;
    let c = client(&server);
    server.fake.answers.lock().unwrap().insert(
        "notice_ack".to_string(),
        pf_ipc::Response::ok(json!({
            "schema_version": 1, "device": "D-1", "position": 4, "outcome": "blocked",
            "admitted": true
        })),
    );
    let acked = c
        .transport
        .notice_ack(NoticeAckArgs {
            position: 4,
            outcome: NoticeOutcome::Blocked,
        })
        .await
        .unwrap();
    assert_eq!(acked.device, "D-1");
    assert_eq!(
        (acked.position, acked.outcome, acked.admitted),
        (4, NoticeOutcome::Blocked, true)
    );
    let calls = server.fake.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![(
            "notice_ack".to_string(),
            json!({"position": 4, "outcome": "blocked"})
        )]
    );
}
