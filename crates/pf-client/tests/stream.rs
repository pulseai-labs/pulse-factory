//! AC-1 of `r1.s3.w4`: the chat stream of `pf-client` over a fake host. The host is the real TLS
//! listener and router of `pf-api` on `127.0.0.1:0` with a generated key, as `crates/pf-api/tests/
//! stream.rs` builds it. The client is a `Transport<FileStore>` paired to that address with the
//! key's pin. Nothing here leaves the loopback address.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_api::code::{hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::host::{StreamEnd, StreamEvent as Feed, StreamRefusal};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult, ServerRef};
use pf_api::{ApiHost, Refusal, RequestRecord, router_with, tls};
use pf_client::error::ErrorKind;
use pf_client::http::Timeouts;
use pf_client::store::{DeviceStore, FileStore, Paired};
use pf_client::stream::{StreamEvent, StreamOptions, StreamTuning, Streams};
use pf_client::transport::Transport;
use serde_json::{Value, json};
use tokio::sync::mpsc;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const OTHER_TOKEN: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";
const SERVER_NAME: &str = "test-server";

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

/// What the fake host saw, and the senders of the feeds it gave.
struct Fake {
    spki_hex: String,
    audits: Mutex<Vec<RequestRecord>>,
    /// Each `open_stream` call: the session and the `after` value.
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

    fn call(&self, _: &DeviceRecord, _: &str, _: Value) -> pf_ipc::Response {
        pf_ipc::Response::ok(json!({}))
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
            name: SERVER_NAME.to_string(),
        }
    }

    fn audit(&self, record: &RequestRecord) {
        self.audits.lock().unwrap().push(record.clone());
    }

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
        match session {
            Some("S-404") => return Err(StreamRefusal::NotFound("no chat session S-404".into())),
            Some("S-400") => return Err(StreamRefusal::BadRequest("the host says no".into())),
            _ => {}
        }
        let (tx, rx) = mpsc::channel(64);
        self.feeds.lock().unwrap().push(tx);
        Ok(rx)
    }
}

impl Fake {
    fn opens(&self) -> Vec<(Option<String>, u64)> {
        self.opens.lock().unwrap().clone()
    }

    fn audits(&self) -> Vec<RequestRecord> {
        self.audits.lock().unwrap().clone()
    }

    /// The sender of the `n`th feed given.
    fn feed(&self, n: usize) -> mpsc::Sender<Feed> {
        self.feeds.lock().unwrap()[n].clone()
    }

    /// Wait for the `n`th feed (counting from 0) to exist.
    async fn wait_feed(&self, n: usize) -> mpsc::Sender<Feed> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if self.feeds.lock().unwrap().len() > n {
                return self.feed(n);
            }
            assert!(Instant::now() < end, "feed {n} never opened");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// A TLS listener with the router over the fake host.
struct Server {
    fake: Arc<Fake>,
    addr: SocketAddr,
    pin: [u8; 32],
    _dir: tempfile::TempDir,
}

/// A key for an address, made before the listener runs, so a test can pair with it first.
struct Prepared {
    dir: tempfile::TempDir,
    key: tls::ServerKey,
    addr: SocketAddr,
}

fn prepare(addr: SocketAddr) -> Prepared {
    let dir = tempfile::tempdir().unwrap();
    let key = tls::load_or_create(&tls::api_dir(dir.path()), &addr).unwrap();
    Prepared { dir, key, addr }
}

impl Server {
    async fn start() -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        Server::serve(listener, prepare(addr))
    }

    fn serve(listener: tokio::net::TcpListener, prepared: Prepared) -> Server {
        let fake = Arc::new(Fake {
            spki_hex: prepared.key.spki_sha256_hex(),
            audits: Mutex::new(Vec::new()),
            opens: Mutex::new(Vec::new()),
            feeds: Mutex::new(Vec::new()),
        });
        let app = router_with(fake.clone(), GuardConfig::default(), system_clock());
        let config = prepared.key.server_config().unwrap();
        tokio::spawn(async move {
            pf_api::listener::serve(listener, config, app, std::future::pending()).await;
        });
        Server {
            fake,
            addr: prepared.addr,
            pin: prepared.key.spki_sha256(),
            _dir: prepared.dir,
        }
    }
}

/// A client: a `FileStore` paired with `address` and `pin`, and a transport with a fast schedule.
struct Client {
    transport: Transport<FileStore>,
    _dir: tempfile::TempDir,
}

fn tuning() -> StreamTuning {
    StreamTuning {
        schedule: vec![Duration::from_millis(50), Duration::from_millis(100)],
        reset_after: Duration::from_secs(60),
        silence: Duration::from_secs(1),
    }
}

fn client_with(address: &str, pin: &[u8; 32], token: &str, tuning: StreamTuning) -> Client {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    store
        .save(&Paired {
            address: address.to_string(),
            pin: hex(pin),
            token: token.to_string(),
            device: device(),
        })
        .unwrap();
    let transport = Transport::new(store)
        .with_timeouts(Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(5),
        })
        .with_stream_tuning(tuning);
    Client {
        transport,
        _dir: dir,
    }
}

fn client(server: &Server) -> Client {
    client_with(&server.addr.to_string(), &server.pin, TOKEN, tuning())
}

fn options(session: &str, after: u64) -> StreamOptions {
    StreamOptions {
        session: Some(session.to_string()),
        after,
    }
}

/// The next event, or a failure that says what came instead.
async fn next(stream: &mut pf_client::stream::ChatStream, within: Duration) -> StreamEvent {
    match tokio::time::timeout(within, stream.next()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("the stream ended with no event"),
        Err(_) => panic!("no event within {within:?}"),
    }
}

async fn next_frame(stream: &mut pf_client::stream::ChatStream) -> Value {
    match next(stream, Duration::from_secs(5)).await {
        StreamEvent::Frame { frame } => serde_json::to_value(frame).unwrap(),
        other => panic!("wanted a frame, got {other:?}"),
    }
}

async fn next_closed(stream: &mut pf_client::stream::ChatStream) -> (ErrorKind, String) {
    match next(stream, Duration::from_secs(5)).await {
        StreamEvent::Closed { error } => (error.kind, error.message),
        other => panic!("wanted closed, got {other:?}"),
    }
}

/// After `closed` nothing follows.
async fn ends(stream: &mut pf_client::stream::ChatStream) {
    match tokio::time::timeout(Duration::from_millis(600), stream.next()).await {
        Ok(None) => {}
        other => panic!("wanted the end of the stream, got {other:?}"),
    }
}

async fn next_reconnecting(stream: &mut pf_client::stream::ChatStream) -> (u64, u64, String) {
    match next(stream, Duration::from_secs(5)).await {
        StreamEvent::Reconnecting {
            after,
            retry_in_ms,
            reason,
        } => (after, retry_in_ms, reason),
        other => panic!("wanted reconnecting, got {other:?}"),
    }
}

fn frame(value: Value) -> Feed {
    Feed::Frame(value.to_string())
}

fn caught(last_seq: u64) -> Value {
    json!({"type": "caught", "session_id": "S-1", "last_seq": last_seq})
}

fn item(seq: u64, redacted: bool) -> Value {
    json!({"type": "item", "item": {
        "session_id": "S-1", "seq": seq, "answers_seq": null,
        "created_at": "2026-11-09T10:00:00.000Z",
        "redacted_at": if redacted { json!("2026-11-09T11:00:00.000Z") } else { Value::Null },
        "body": {"kind": "assistant_text", "text": format!("item {seq}")}
    }})
}

async fn send(tx: &mpsc::Sender<Feed>, event: Feed) {
    tx.send(event).await.expect("the host's feed is open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frames_arrive_decoded_in_order_and_a_bad_known_frame_ends_the_stream() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let tx = server.fake.wait_feed(0).await;
    send(&tx, frame(item(1, false))).await;
    // A type this client does not know is skipped: a later server may add frames.
    send(&tx, frame(json!({"type": "future_thing", "n": 1}))).await;
    send(&tx, frame(json!({"no_type": true}))).await;
    send(
        &tx,
        frame(json!({"type": "delta", "session_id": "S-1", "text": "hi"})),
    )
    .await;
    send(&tx, frame(caught(1))).await;
    assert_eq!(next_frame(&mut stream).await["item"]["seq"], 1);
    let delta = next_frame(&mut stream).await;
    assert_eq!(delta["type"], "delta");
    assert_eq!(delta["text"], "hi");
    assert_eq!(next_frame(&mut stream).await["type"], "caught");
    assert_eq!(
        server.fake.opens(),
        vec![(Some("S-1".to_string()), 0)],
        "the open names the session and after 0"
    );
    // A known type that does not decode ends the stream: an item is never skipped silently.
    send(&tx, frame(json!({"type": "item", "item": "oops"}))).await;
    let (kind, message) = next_closed(&mut stream).await;
    assert_eq!(kind, ErrorKind::Server);
    assert_eq!(message, "the server sent a frame this client cannot read");
    ends(&mut stream).await;
    assert_eq!(server.fake.opens().len(), 1, "no retry");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoke_closes_the_stream_once_with_no_retry() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let tx = server.fake.wait_feed(0).await;
    send(&tx, Feed::End(StreamEnd::Revoked)).await;
    let (kind, message) = next_closed(&mut stream).await;
    assert_eq!(kind, ErrorKind::Unauthorized);
    assert_eq!(message, "this device was revoked; pair it again");
    ends(&mut stream).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(server.fake.opens().len(), 1, "the host saw one open only");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lagged_close_reconnects_at_once_with_the_last_seq_and_drops_what_it_holds() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let tx = server.fake.wait_feed(0).await;
    for seq in 1..=5 {
        send(&tx, frame(item(seq, false))).await;
    }
    send(&tx, Feed::End(StreamEnd::Lagged { after_seq: 3 })).await;
    for seq in 1..=5 {
        assert_eq!(next_frame(&mut stream).await["item"]["seq"], seq);
    }
    // The reason is `lagged` and the retry is at once. `after` is the client's own last seq (5),
    // not the 3 of the close reason.
    let (after, retry_in_ms, reason) = next_reconnecting(&mut stream).await;
    assert_eq!((after, retry_in_ms, reason.as_str()), (5, 0, "lagged"));
    let tx = server.fake.wait_feed(1).await;
    assert_eq!(
        server.fake.opens(),
        vec![(Some("S-1".to_string()), 0), (Some("S-1".to_string()), 5)]
    );
    // The host re-sends items 4 and 5, then a redaction's re-send of 5, then item 6.
    send(&tx, frame(item(4, false))).await;
    send(&tx, frame(item(5, false))).await;
    send(&tx, frame(item(5, true))).await;
    send(&tx, frame(item(6, false))).await;
    let redacted = next_frame(&mut stream).await;
    assert_eq!(redacted["item"]["seq"], 5);
    assert!(redacted["item"]["redacted_at"].is_string());
    assert_eq!(next_frame(&mut stream).await["item"]["seq"], 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_going_away_close_and_a_dropped_feed_reconnect_on_the_schedule() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options("S-1", 2));
    let tx = server.fake.wait_feed(0).await;
    send(&tx, frame(item(3, false))).await;
    send(&tx, Feed::End(StreamEnd::Shutdown)).await;
    assert_eq!(next_frame(&mut stream).await["item"]["seq"], 3);
    let (after, wait, reason) = next_reconnecting(&mut stream).await;
    assert_eq!((after, wait), (3, 50));
    assert_eq!(reason, "the server is stopping");
    // The second drop is a dropped feed: the host ends the socket without a code of its own.
    let tx = server.fake.wait_feed(1).await;
    drop(tx);
    server.fake.feeds.lock().unwrap().clear();
    let (after, wait, _) = next_reconnecting(&mut stream).await;
    assert_eq!((after, wait), (3, 100), "the second step of the schedule");
    // The last step repeats.
    let _tx = server.fake.wait_feed(0).await;
    assert_eq!(
        server.fake.opens(),
        vec![
            (Some("S-1".to_string()), 2),
            (Some("S-1".to_string()), 3),
            (Some("S-1".to_string()), 3)
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_held_past_the_reset_time_resets_the_schedule() {
    let server = Server::start().await;
    let mut t = tuning();
    t.reset_after = Duration::from_millis(1200);
    let c = client_with(&server.addr.to_string(), &server.pin, TOKEN, t);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    // Two drops in a row walk the schedule: 50 ms, then 100 ms.
    let tx = server.fake.wait_feed(0).await;
    send(&tx, Feed::End(StreamEnd::Shutdown)).await;
    assert_eq!(next_reconnecting(&mut stream).await.1, 50);
    let tx = server.fake.wait_feed(1).await;
    send(&tx, Feed::End(StreamEnd::Shutdown)).await;
    assert_eq!(next_reconnecting(&mut stream).await.1, 100);
    // The third connection stays open past the reset time on a feed that sends a frame every 500 ms,
    // inside the 1 s silence limit.
    let tx = server.fake.wait_feed(2).await;
    for n in 0..3 {
        send(
            &tx,
            frame(json!({"type": "delta", "session_id": "S-1", "text": n.to_string()})),
        )
        .await;
        next_frame(&mut stream).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    send(&tx, Feed::End(StreamEnd::Shutdown)).await;
    // The schedule starts again at its first step.
    let (_, wait, _) = loop {
        match next(&mut stream, Duration::from_secs(5)).await {
            StreamEvent::Reconnecting {
                after,
                retry_in_ms,
                reason,
            } => break (after, retry_in_ms, reason),
            StreamEvent::Frame { .. } => {}
            other => panic!("wanted reconnecting, got {other:?}"),
        }
    };
    assert_eq!(wait, 50, "a long connection resets the schedule");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_that_goes_silent_past_the_limit_counts_as_dropped() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let _tx = server.fake.wait_feed(0).await;
    // The host sends nothing and no ping (the router's ping is every 30 s): the 1 s limit passes.
    let started = Instant::now();
    let (_, _, reason) = next_reconnecting(&mut stream).await;
    assert_eq!(reason, "the connection dropped");
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "it waited the silence limit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_listening_retries_and_a_listener_that_starts_later_gets_the_next_attempt() {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let prepared = prepare(addr);
    let pin = prepared.key.spki_sha256();
    let c = client_with(&addr.to_string(), &pin, TOKEN, tuning());
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let (after, wait, reason) = next_reconnecting(&mut stream).await;
    assert_eq!((after, wait), (0, 50));
    assert_eq!(reason, format!("cannot reach {addr}"));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let server = Server::serve(listener, prepared);
    let tx = server.fake.wait_feed(0).await;
    send(&tx, frame(caught(0))).await;
    loop {
        match next(&mut stream, Duration::from_secs(5)).await {
            StreamEvent::Frame { frame } => {
                assert_eq!(serde_json::to_value(frame).unwrap()["type"], "caught");
                break;
            }
            StreamEvent::Reconnecting { .. } => {}
            other => panic!("wanted a frame, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_key_is_key_changed_with_no_request_and_no_retry() {
    let real = Server::start().await;
    let other = Server::start().await;
    // Paired with `real`'s key, but the address is `other`'s.
    let c = client_with(&other.addr.to_string(), &real.pin, TOKEN, tuning());
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let (kind, message) = next_closed(&mut stream).await;
    assert_eq!(kind, ErrorKind::KeyChanged);
    assert_eq!(message, "server key changed: re-pair");
    ends(&mut stream).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(other.fake.audits().is_empty(), "the host saw no request");
    assert!(other.fake.opens().is_empty(), "the host saw no open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_http_refusal_is_one_closed_event_and_one_host_record() {
    let server = Server::start().await;
    // 401: a token the host does not know.
    let bad = client_with(&server.addr.to_string(), &server.pin, OTHER_TOKEN, tuning());
    let mut stream = bad.transport.open_stream(options("S-1", 0));
    assert_eq!(next_closed(&mut stream).await.0, ErrorKind::Unauthorized);
    ends(&mut stream).await;
    assert_eq!(server.fake.audits().len(), 1);

    let c = client(&server);
    // 404: an unknown session. 400: the host's refusal.
    for (session, kind, text) in [
        ("S-404", ErrorKind::Server, "no chat session S-404"),
        ("S-400", ErrorKind::BadRequest, "the host says no"),
    ] {
        let before = server.fake.audits().len();
        let mut stream = c.transport.open_stream(options(session, 0));
        let (got, message) = next_closed(&mut stream).await;
        assert_eq!((got, message.as_str()), (kind, text), "{session}");
        ends(&mut stream).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            server.fake.audits().len(),
            before + 1,
            "{session}: no retry"
        );
    }

    // A session ID that is no `S-<n>` never connects.
    let before = server.fake.audits().len();
    let mut stream = c.transport.open_stream(options("X-1", 0));
    assert_eq!(next_closed(&mut stream).await.0, ErrorKind::BadRequest);
    ends(&mut stream).await;
    assert_eq!(server.fake.audits().len(), before, "no connection");

    // 429: the ninth stream of a device.
    let mut held = Vec::new();
    for n in 0..8 {
        held.push(c.transport.open_stream(options("S-1", 0)));
        server.fake.wait_feed(n).await;
    }
    let before = server.fake.audits().len();
    let mut ninth = c.transport.open_stream(options("S-1", 0));
    let (kind, message) = next_closed(&mut ninth).await;
    assert_eq!(kind, ErrorKind::RateLimited);
    assert_eq!(message, "too many open streams");
    ends(&mut ninth).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.fake.audits().len(),
        before + 1,
        "no retry after a 429"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_stream_makes_one_upgrade_and_no_request_on_a_timer() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let tx = server.fake.wait_feed(0).await;
    let end = Instant::now() + Duration::from_secs(3);
    let mut n = 0;
    while Instant::now() < end {
        send(
            &tx,
            frame(json!({"type": "delta", "session_id": "S-1", "text": n.to_string()})),
        )
        .await;
        next_frame(&mut stream).await;
        n += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(server.fake.opens().len(), 1);
    assert_eq!(
        server.fake.audits().len(),
        1,
        "one upgrade, no other request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_token_is_in_the_upgrade_header_and_nowhere_else() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let tx = server.fake.wait_feed(0).await;
    // The host knows the device by the token's hash, so the upgrade carried it. The query holds
    // only the session and `after`: the router refuses any other key with a 400.
    assert_eq!(server.fake.opens(), vec![(Some("S-1".to_string()), 0)]);
    send(&tx, frame(caught(0))).await;
    send(&tx, Feed::End(StreamEnd::Revoked)).await;
    let mut seen = format!("{stream:?}");
    for _ in 0..2 {
        let event = next(&mut stream, Duration::from_secs(5)).await;
        seen.push_str(&format!(
            "{event:?} {}",
            serde_json::to_string(&event).unwrap()
        ));
    }
    let bad = client_with(&server.addr.to_string(), &server.pin, OTHER_TOKEN, tuning());
    let mut refused = bad.transport.open_stream(options("S-1", 0));
    let event = next(&mut refused, Duration::from_secs(5)).await;
    seen.push_str(&format!(
        "{event:?} {}",
        serde_json::to_string(&event).unwrap()
    ));
    for token in [TOKEN, OTHER_TOKEN] {
        assert!(!seen.contains(token), "a token is in {seen}");
    }
    assert!(!seen.contains("Bearer"), "{seen}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_caller_closing_closes_the_socket_and_forget_ends_the_stream() {
    let server = Server::start().await;
    let c = client(&server);
    let stream = c.transport.open_stream(options("S-1", 0));
    let tx = server.fake.wait_feed(0).await;
    drop(stream);
    tokio::time::timeout(Duration::from_secs(1), tx.closed())
        .await
        .expect("the host's feed saw its receiver closed within 1 s");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(server.fake.opens().len(), 1, "no open follows a close");

    let mut open = c.transport.open_stream(options("S-1", 0));
    server.fake.wait_feed(1).await;
    c.transport.forget().await.unwrap();
    let (kind, message) = next_closed(&mut open).await;
    assert_eq!(kind, ErrorKind::Unauthorized);
    assert_eq!(message, "this device is not paired");
    ends(&mut open).await;
    // With no pairing, a new stream says the same at once.
    let mut again = c.transport.open_stream(options("S-1", 0));
    assert_eq!(next_closed(&mut again).await.1, "this device is not paired");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_registry_gives_distinct_ids_and_closes_one_or_all() {
    let server = Server::start().await;
    let c = client(&server);
    let streams = Streams::new();
    let seen: Arc<Mutex<Vec<StreamEvent>>> = Arc::default();
    let sink = |seen: &Arc<Mutex<Vec<StreamEvent>>>| {
        let seen = seen.clone();
        move |event: StreamEvent| {
            seen.lock().unwrap().push(event);
            true
        }
    };
    let a = streams.open(&c.transport, options("S-1", 0), sink(&seen));
    let b = streams.open(&c.transport, options("S-1", 0), sink(&seen));
    let third = streams.open(&c.transport, options("S-1", 0), sink(&seen));
    assert!(a != b && b != third && a != third, "{a} {b} {third}");
    let feeds = [
        server.fake.wait_feed(0).await,
        server.fake.wait_feed(1).await,
        server.fake.wait_feed(2).await,
    ];
    // `close(id)` ends one stream: its feed sees the socket go, the others stay.
    assert!(streams.close(b));
    assert!(!streams.close(b), "a closed ID is gone");
    let end = Instant::now() + Duration::from_secs(2);
    while !feeds.iter().any(|f| f.is_closed()) {
        assert!(Instant::now() < end, "no feed closed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(feeds.iter().filter(|f| f.is_closed()).count(), 1);
    // `close_all()` ends every one that is left.
    streams.close_all();
    let end = Instant::now() + Duration::from_secs(2);
    while !feeds.iter().all(|f| f.is_closed()) {
        assert!(Instant::now() < end, "not every feed closed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // A sink that answers `false` ends its stream.
    let refusing = streams.open(&c.transport, options("S-1", 0), |_| false);
    let _ = refusing;
    let tx = server.fake.wait_feed(3).await;
    send(&tx, frame(caught(0))).await;
    tokio::time::timeout(Duration::from_secs(2), tx.closed())
        .await
        .expect("a sink that answers false ends its stream");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hello_with_the_pins_id_sets_the_server_of_the_paired_answer() {
    let server = Server::start().await;
    let c = client(&server);
    assert_eq!(c.transport.paired().await.unwrap().unwrap().server, None);
    let mut stream = c.transport.open_stream(options("S-1", 0));
    let tx = server.fake.wait_feed(0).await;
    let hello = |id: Option<String>| {
        frame(
            json!({"type": "hello", "server": {"id": id, "name": "named-by-hello"},
                     "sessions": [], "following": "S-1", "after_seq": 0}),
        )
    };
    // The `101` already named the server, in its headers.
    let from_upgrade = c.transport.paired().await.unwrap().unwrap().server.unwrap();
    assert_eq!(from_upgrade.name, SERVER_NAME);
    // A hello that names another id is ignored.
    send(&tx, hello(Some("0".repeat(64)))).await;
    next_frame(&mut stream).await;
    assert_eq!(
        c.transport
            .paired()
            .await
            .unwrap()
            .unwrap()
            .server
            .unwrap()
            .name,
        SERVER_NAME
    );
    send(&tx, hello(Some(hex(&server.pin)))).await;
    next_frame(&mut stream).await;
    let named = c.transport.paired().await.unwrap().unwrap().server.unwrap();
    assert_eq!(named.name, "named-by-hello");
    assert_eq!(named.id, Some(hex(&server.pin)));
}

#[test]
fn each_event_serializes_to_the_documented_json() {
    let caught: pf_api::types::ChatFrame =
        serde_json::from_value(caught(6)).expect("a caught frame");
    assert_eq!(
        serde_json::to_value(StreamEvent::Frame { frame: caught }).unwrap(),
        json!({"event": "frame", "frame": {"type": "caught", "session_id": "S-1", "last_seq": 6}})
    );
    assert_eq!(
        serde_json::to_value(StreamEvent::Reconnecting {
            after: 6,
            retry_in_ms: 4000,
            reason: "the server is stopping".to_string(),
        })
        .unwrap(),
        json!({"event": "reconnecting", "after": 6, "retry_in_ms": 4000, "reason": "the server is stopping"})
    );
    assert_eq!(
        serde_json::to_value(StreamEvent::Closed {
            error: pf_client::error::ClientError::new(
                ErrorKind::Unauthorized,
                "this device was revoked; pair it again"
            )
        })
        .unwrap(),
        json!({"event": "closed", "error": {"kind": "unauthorized", "message": "this device was revoked; pair it again"}})
    );
    // An omitted `after` is 0.
    let parsed: StreamOptions = serde_json::from_value(json!({"session": "S-1"})).unwrap();
    assert_eq!(parsed.after, 0);
    let none: StreamOptions = serde_json::from_value(json!({})).unwrap();
    assert_eq!((none.session, none.after), (None, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_stream_sends_a_close_frame_before_the_tcp_close() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    // A bare WebSocket server over the same pinned TLS key: it says what the client sent last.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let prepared = prepare(addr);
    let pin = prepared.key.spki_sha256();
    let acceptor = tokio_rustls::TlsAcceptor::from(prepared.key.server_config().unwrap());
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel::<String>();
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(tcp).await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tls).await.unwrap();
        let last = loop {
            match socket.next().await {
                Some(Ok(Message::Close(_))) => break "close frame".to_string(),
                Some(Ok(_)) => {}
                Some(Err(e)) => break format!("error: {e}"),
                None => break "end with no close frame".to_string(),
            }
        };
        let _ = seen_tx.send(last);
        let _keep = prepared.dir;
    });
    let c = client_with(&addr.to_string(), &pin, TOKEN, tuning());
    let stream = c.transport.open_stream(options("S-1", 0));
    // The upgrade is done when the server has sent nothing and the client waits: give it a moment.
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(stream);
    let seen = tokio::time::timeout(Duration::from_secs(2), seen_rx)
        .await
        .expect("the server saw the end")
        .unwrap();
    assert_eq!(seen, "close frame");
}
