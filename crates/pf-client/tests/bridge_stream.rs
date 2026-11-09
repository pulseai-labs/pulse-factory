//! AC-3 of `r1.s3.w4`: `GET /bridge/stream` (Server-Sent Events) and the typed chat verbs of the
//! bridge. `bridge::serve` runs in the test process, and the fake host behind it is the real TLS
//! listener and router of `pf-api`. Nothing here leaves the loopback address.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_api::code::{hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::host::{StreamEnd, StreamEvent as Feed, StreamRefusal};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult, ServerRef};
use pf_api::{ApiHost, Refusal, RequestRecord, router_with, tls};
use pf_client::store::{DeviceStore, FileStore, Paired};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'";

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

struct Fake {
    spki_hex: String,
    audits: Mutex<Vec<RequestRecord>>,
    opens: Mutex<Vec<(Option<String>, u64)>>,
    feeds: Mutex<Vec<mpsc::Sender<Feed>>>,
    calls: Mutex<Vec<String>>,
}

impl ApiHost for Fake {
    fn redeem(&self, _: &str, _: &str, _: IpAddr) -> Result<PairResult, Refusal> {
        Err(Refusal::Code)
    }

    fn device(&self, token_sha256_hex: &str, _peer: IpAddr) -> Option<DeviceRecord> {
        (Some(token_sha256_hex.to_string()) == token_sha256(TOKEN)).then(device)
    }

    fn call(&self, _: &DeviceRecord, verb: &str, _args: Value) -> pf_ipc::Response {
        self.calls.lock().unwrap().push(verb.to_string());
        pf_ipc::Response::ok(json!({
            "replayed": false,
            "session": {"id": "S-1", "state": "idle"},
            "worktree": {"id": "W-1", "path": "/work/alpha"}
        }))
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

struct Rig {
    fake: Arc<Fake>,
    bridge: Option<pf_client::bridge::Bridge>,
    host: String,
    state: tempfile::TempDir,
    _ui: tempfile::TempDir,
    _server: tempfile::TempDir,
}

impl Rig {
    /// A bridge over a device store that is paired with the fake host, or not.
    async fn start(paired: bool) -> Rig {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_dir = tempfile::tempdir().unwrap();
        let key = tls::load_or_create(&tls::api_dir(server_dir.path()), &addr).unwrap();
        let fake = Arc::new(Fake {
            spki_hex: key.spki_sha256_hex(),
            audits: Mutex::new(Vec::new()),
            opens: Mutex::new(Vec::new()),
            feeds: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        });
        let app = router_with(fake.clone(), GuardConfig::default(), system_clock());
        let config = key.server_config().unwrap();
        tokio::spawn(async move {
            pf_api::listener::serve(listener, config, app, std::future::pending()).await;
        });
        let ui = tempfile::tempdir().unwrap();
        std::fs::write(ui.path().join("index.html"), "<html></html>").unwrap();
        let state = tempfile::tempdir().unwrap();
        if paired {
            FileStore::new(state.path().join("device.json"))
                .save(&Paired {
                    address: addr.to_string(),
                    pin: hex(&key.spki_sha256()),
                    token: TOKEN.to_string(),
                    device: device(),
                })
                .unwrap();
        }
        let bridge = pf_client::bridge::serve(ui.path().to_path_buf(), state.path().to_path_buf())
            .await
            .unwrap();
        Rig {
            fake,
            host: format!("127.0.0.1:{}", bridge.addr().port()),
            bridge: Some(bridge),
            state,
            _ui: ui,
            _server: server_dir,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.bridge.as_ref().unwrap().addr()
    }

    async fn stop(&mut self) {
        if let Some(bridge) = self.bridge.take() {
            bridge.shutdown().await;
        }
    }

    /// A request with the bridge's own Host, and `extra` header lines.
    async fn get(&self, target: &str, extra: &str) -> Reply {
        let head = format!(
            "GET {target} HTTP/1.1\r\nHost: {}\r\n{extra}Connection: close\r\n\r\n",
            self.host
        );
        Reply::send(self.addr(), head.as_bytes()).await
    }

    async fn post(&self, target: &str, host: &str, content_type: &str, body: &str) -> Reply {
        let head = format!(
            "POST {target} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        Reply::send(self.addr(), head.as_bytes()).await
    }
}

/// An answer whose body is read as it comes.
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    reader: BufReader<OwnedReadHalf>,
    chunked: bool,
    length: Option<usize>,
    pending: String,
    ended: bool,
    /// Every byte of the head and body read so far.
    raw: String,
    /// The write half stays open until the reply is dropped, so the server sees a live client, and
    /// dropping the reply is the page's disconnect.
    _write: OwnedWriteHalf,
}

impl Reply {
    async fn send(addr: SocketAddr, request: &[u8]) -> Reply {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (read, mut write) = tcp.into_split();
        write.write_all(request).await.unwrap();
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        let mut raw = String::new();
        reader.read_line(&mut line).await.unwrap();
        raw.push_str(&line);
        let status = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {line:?}"));
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            raw.push_str(&line);
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let chunked = headers
            .get("transfer-encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
        let length = headers.get("content-length").and_then(|v| v.parse().ok());
        Reply {
            status,
            headers,
            reader,
            chunked,
            length,
            pending: String::new(),
            ended: false,
            raw,
            _write: write,
        }
    }

    /// The next piece of the body, or `None` at its end.
    async fn piece(&mut self) -> Option<String> {
        if self.ended {
            return None;
        }
        let mut data = Vec::new();
        if self.chunked {
            let mut size_line = String::new();
            self.reader.read_line(&mut size_line).await.ok()?;
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                self.ended = true;
                return None;
            }
            data.resize(size, 0);
            self.reader.read_exact(&mut data).await.ok()?;
            let mut crlf = [0u8; 2];
            self.reader.read_exact(&mut crlf).await.ok()?;
        } else if let Some(length) = self.length.take() {
            data.resize(length, 0);
            self.reader.read_exact(&mut data).await.ok()?;
        } else {
            self.reader.read_to_end(&mut data).await.ok()?;
            self.ended = true;
            if data.is_empty() {
                return None;
            }
        }
        let text = String::from_utf8_lossy(&data).into_owned();
        self.raw.push_str(&text);
        Some(text)
    }

    async fn body(mut self) -> (String, String) {
        let mut all = String::new();
        while let Some(piece) = self.piece().await {
            all.push_str(&piece);
        }
        (all, self.raw)
    }

    /// The next Server-Sent Event: its `data:` line as JSON. `None` when the response ends.
    async fn event(&mut self, within: Duration) -> Option<Value> {
        let end = Instant::now() + within;
        loop {
            if let Some(at) = self.pending.find("\n\n") {
                let block: String = self.pending.drain(..at + 2).collect();
                let data = block
                    .lines()
                    .filter_map(|l| l.strip_prefix("data: "))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(!data.is_empty(), "an event with no data line: {block:?}");
                return Some(
                    serde_json::from_str(&data)
                        .unwrap_or_else(|e| panic!("an event that is not JSON ({e}): {data:?}")),
                );
            }
            let left = end.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.piece()).await {
                Ok(Some(piece)) => self.pending.push_str(&piece),
                Ok(None) => return None,
                Err(_) => panic!("no event within {within:?}; pending {:?}", self.pending),
            }
        }
    }
}

fn caught(last_seq: u64) -> Feed {
    Feed::Frame(json!({"type": "caught", "session_id": "S-1", "last_seq": last_seq}).to_string())
}

fn item(seq: u64) -> Feed {
    Feed::Frame(
        json!({"type": "item", "item": {
            "session_id": "S-1", "seq": seq, "answers_seq": null,
            "created_at": "2026-11-09T10:00:00.000Z", "redacted_at": null,
            "body": {"kind": "assistant_text", "text": "hi"}
        }})
        .to_string(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stream_relays_each_event_as_a_data_line_and_ends_after_closed() {
    let rig = Rig::start(true).await;
    let mut reply = rig.get("/bridge/stream?session=S-1&after=0", "").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.headers["content-type"], "text/event-stream");
    assert_eq!(reply.headers["cache-control"], "no-store");
    assert_eq!(reply.headers["content-security-policy"], CSP);
    let tx = rig.fake.wait_feed(0).await;
    assert_eq!(
        rig.fake.opens.lock().unwrap().clone(),
        vec![(Some("S-1".to_string()), 0)]
    );
    tx.send(item(1)).await.unwrap();
    tx.send(caught(1)).await.unwrap();
    tx.send(Feed::End(StreamEnd::Revoked)).await.unwrap();
    let first = reply.event(Duration::from_secs(5)).await.unwrap();
    assert_eq!(first["event"], "frame");
    assert_eq!(first["frame"]["type"], "item");
    assert_eq!(first["frame"]["item"]["seq"], 1);
    let second = reply.event(Duration::from_secs(5)).await.unwrap();
    assert_eq!(
        second,
        json!({"event": "frame", "frame": {"type": "caught", "session_id": "S-1", "last_seq": 1}})
    );
    let third = reply.event(Duration::from_secs(5)).await.unwrap();
    assert_eq!(
        third,
        json!({"event": "closed", "error": {"kind": "unauthorized", "message": "this device was revoked; pair it again"}})
    );
    // The response ends after `closed`.
    assert_eq!(reply.event(Duration::from_secs(5)).await, None);
    let (_, raw) = reply.body().await;
    assert!(!raw.contains(TOKEN), "the token is in the answer");
    assert_eq!(
        rig.fake.opens.lock().unwrap().len(),
        1,
        "no retry after a revoke"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_host_or_origin_is_forbidden_before_any_upstream_connection() {
    let rig = Rig::start(true).await;
    let foreign_host = Reply::send(
        rig.addr(),
        b"GET /bridge/stream?session=S-1 HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(foreign_host.status, 403);
    let foreign_origin = rig
        .get(
            "/bridge/stream?session=S-1",
            "Origin: http://evil.example\r\n",
        )
        .await;
    assert_eq!(foreign_origin.status, 403);
    // A method other than GET is 405.
    let post = rig
        .post("/bridge/stream", &rig.host, "application/json", "{}")
        .await;
    assert_eq!(post.status, 405);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        rig.fake.audits.lock().unwrap().is_empty(),
        "the host saw a request"
    );
    assert!(
        rig.fake.opens.lock().unwrap().is_empty(),
        "the host saw an upgrade"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pages_disconnect_closes_the_upstream_within_a_second() {
    let rig = Rig::start(true).await;
    let mut reply = rig.get("/bridge/stream?session=S-1&after=0", "").await;
    let tx = rig.fake.wait_feed(0).await;
    tx.send(caught(0)).await.unwrap();
    assert_eq!(
        reply.event(Duration::from_secs(5)).await.unwrap()["event"],
        "frame"
    );
    drop(reply);
    tokio::time::timeout(Duration::from_secs(1), tx.closed())
        .await
        .expect("the upstream socket closed within 1 s of the page's disconnect");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unpaired_bridge_sends_one_closed_event_and_ends() {
    let rig = Rig::start(false).await;
    let mut reply = rig.get("/bridge/stream?session=S-1", "").await;
    assert_eq!(reply.status, 200);
    let event = reply.event(Duration::from_secs(5)).await.unwrap();
    assert_eq!(
        event,
        json!({"event": "closed", "error": {"kind": "unauthorized", "message": "this device is not paired"}})
    );
    assert_eq!(reply.event(Duration::from_secs(5)).await, None);
    assert!(rig.fake.audits.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_query_the_bridge_cannot_read_is_one_closed_event_too() {
    let rig = Rig::start(true).await;
    for target in [
        "/bridge/stream?session=X-1",
        "/bridge/stream?after=abc",
        "/bridge/stream?token=x",
    ] {
        let mut reply = rig.get(target, "").await;
        assert_eq!(reply.status, 200, "{target}");
        let event = reply.event(Duration::from_secs(5)).await.unwrap();
        assert_eq!(event["event"], "closed", "{target}");
        assert_eq!(event["error"]["kind"], "bad_request", "{target}");
        assert_eq!(reply.event(Duration::from_secs(5)).await, None);
    }
    assert!(
        rig.fake.audits.lock().unwrap().is_empty(),
        "no connection was made"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_verb_answers_the_transport_shape_and_an_off_list_verb_never_connects() {
    let rig = Rig::start(true).await;
    let started = rig
        .post(
            "/bridge/verbs/session_start",
            &rig.host,
            "application/json",
            r#"{"project_id":"P-1","command_id":"c-1"}"#,
        )
        .await;
    assert_eq!(started.status, 200);
    let (body, raw) = started.body().await;
    let answer: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        answer,
        json!({"ok": true, "result": {
            "replayed": false, "session_id": "S-1", "worktree_id": "W-1",
            "worktree_dir": "/work/alpha", "note": null
        }})
    );
    assert!(!raw.contains(TOKEN));
    assert_eq!(
        rig.fake.calls.lock().unwrap().clone(),
        vec!["session_start"]
    );
    // A bad argument fails at the bridge.
    let bad = rig
        .post(
            "/bridge/verbs/session_send",
            &rig.host,
            "application/json",
            r#"{"text":1}"#,
        )
        .await;
    let (body, _) = bad.body().await;
    let answer: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(answer["ok"], false);
    assert_eq!(answer["error"]["kind"], "bad_request");
    // `session_redact` and `session_items` are not reachable from a device.
    let audits = rig.fake.audits.lock().unwrap().len();
    for verb in ["session_redact", "session_items"] {
        let reply = rig
            .post(
                &format!("/bridge/verbs/{verb}"),
                &rig.host,
                "application/json",
                r#"{"session_id":"S-1","seq":1,"command_id":"c"}"#,
            )
            .await;
        let (body, _) = reply.body().await;
        let answer: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(answer["error"]["kind"], "bad_request", "{verb}");
    }
    assert_eq!(
        rig.fake.audits.lock().unwrap().len(),
        audits,
        "no connection for them"
    );
    assert_eq!(rig.fake.calls.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_answer_holds_the_token_and_the_csp_is_unchanged() {
    let mut rig = Rig::start(true).await;
    let file = rig.get("/", "").await;
    assert_eq!(file.headers["content-security-policy"], CSP);
    let paired = rig.get("/bridge/paired", "").await;
    assert_eq!(paired.headers["content-security-policy"], CSP);
    let (body, raw) = paired.body().await;
    assert!(!body.contains(TOKEN) && !raw.contains(TOKEN));
    let mut stream = rig.get("/bridge/stream?session=S-1", "").await;
    assert_eq!(stream.headers["content-security-policy"], CSP);
    let tx = rig.fake.wait_feed(0).await;
    tx.send(caught(0)).await.unwrap();
    stream.event(Duration::from_secs(5)).await.unwrap();
    tx.send(Feed::End(StreamEnd::Revoked)).await.unwrap();
    while stream.event(Duration::from_secs(5)).await.is_some() {}
    assert!(!stream.raw.contains(TOKEN));
    // The token is in the device file only.
    let device_file = std::fs::read_to_string(rig.state.path().join("device.json")).unwrap();
    assert!(device_file.contains(TOKEN));
    rig.stop().await;
}
