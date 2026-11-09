//! AC-1 of `r1.s3.w3`: the stream route over a fake host. The listener is the real TLS listener on
//! `127.0.0.1:0` with a test key, and the client is a WebSocket client over a TLS connection whose
//! key is pinned, as a paired device's is. Nothing here leaves the loopback address.
//!
//! The client is the synchronous `tungstenite` client that `tokio-tungstenite` re-exports. It runs
//! on a blocking thread, so a test needs no stream combinator crate.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_api::code::{hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::host::{StreamEnd, StreamEvent, StreamRefusal};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult, ServerRef};
use pf_api::{ApiHost, Outcome, Refusal, RequestRecord, Route, router_with, tls};
use pf_client::pin::{PinVerifier, client_config};
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Response as HandshakeResponse;
use tokio_tungstenite::tungstenite::{self, HandshakeError, Message, WebSocket};

const TOKEN_A: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const TOKEN_B: &str = "ef56ef56ef56ef56ef56ef56ef56ef56ef56ef56ef56ef56ef56ef56ef56ef56";
const BAD_TOKEN: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";
/// A token whose device was revoked: the host no longer knows its hash.
const REVOKED_TOKEN: &str = "11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa";
/// A token whose device is idle for too long: the host answers `None` for it too.
const IDLE_TOKEN: &str = "22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb";
const SERVER_NAME: &str = "test-server";

fn device(id: &str) -> DeviceRecord {
    DeviceRecord {
        id: id.to_string(),
        name: format!("device {id}"),
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
    /// Each `open_stream` call: the device, the session and the `after` value.
    opens: Mutex<Vec<(String, Option<String>, u64)>>,
    /// The sender of each feed given, with the device it belongs to.
    feeds: Mutex<Vec<(String, mpsc::Sender<StreamEvent>)>>,
}

impl ApiHost for Fake {
    fn redeem(&self, _: &str, _: &str, _: IpAddr) -> Result<PairResult, Refusal> {
        Err(Refusal::Code)
    }

    fn device(&self, token_sha256_hex: &str, _peer: IpAddr) -> Option<DeviceRecord> {
        if Some(token_sha256_hex.to_string()) == token_sha256(TOKEN_A) {
            Some(device("D-1"))
        } else if Some(token_sha256_hex.to_string()) == token_sha256(TOKEN_B) {
            Some(device("D-2"))
        } else {
            None
        }
    }

    fn call(&self, _: &DeviceRecord, _: &str, _: Value) -> pf_ipc::Response {
        pf_ipc::Response::ok(serde_json::json!({}))
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
        device: &DeviceRecord,
        session: Option<&str>,
        after: u64,
    ) -> Result<mpsc::Receiver<StreamEvent>, StreamRefusal> {
        self.opens
            .lock()
            .unwrap()
            .push((device.id.clone(), session.map(str::to_string), after));
        if session == Some("S-404") {
            return Err(StreamRefusal::NotFound("no chat session S-404".to_string()));
        }
        let (tx, rx) = mpsc::channel(64);
        tx.try_send(StreamEvent::Frame(r#"{"type":"hello","n":0}"#.to_string()))
            .unwrap();
        self.feeds.lock().unwrap().push((device.id.clone(), tx));
        Ok(rx)
    }
}

impl Fake {
    /// The sender of the `n`th feed given.
    fn feed(&self, n: usize) -> mpsc::Sender<StreamEvent> {
        self.feeds.lock().unwrap()[n].1.clone()
    }

    fn opens(&self) -> Vec<(String, Option<String>, u64)> {
        self.opens.lock().unwrap().clone()
    }

    fn audits(&self) -> Vec<RequestRecord> {
        self.audits.lock().unwrap().clone()
    }
}

/// A TLS listener with the router over the fake host.
struct Server {
    fake: Arc<Fake>,
    addr: SocketAddr,
    pin: [u8; 32],
    _dir: tempfile::TempDir,
}

impl Server {
    async fn start() -> Server {
        Server::start_with(GuardConfig::default()).await
    }

    async fn start_with(guard: GuardConfig) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key = tls::load_or_create(&tls::api_dir(dir.path()), &addr).unwrap();
        let fake = Arc::new(Fake {
            spki_hex: key.spki_sha256_hex(),
            audits: Mutex::new(Vec::new()),
            opens: Mutex::new(Vec::new()),
            feeds: Mutex::new(Vec::new()),
        });
        let app = router_with(fake.clone(), guard, system_clock());
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

    fn url(&self, target: &str) -> String {
        format!("ws://{}{target}", self.addr)
    }

    /// The upgrade, on a blocking thread.
    async fn open(
        &self,
        token: Option<&str>,
        target: &str,
    ) -> Result<(Socket, HandshakeResponse), Failed> {
        let (addr, pin, url) = (self.addr, self.pin, self.url(target));
        let token = token.map(str::to_string);
        tokio::task::spawn_blocking(move || connect(addr, pin, &url, token.as_deref()))
            .await
            .unwrap()
    }

    /// One plain HTTP request, as bytes, with no upgrade: the status, the lower-cased headers and
    /// the body.
    async fn raw(&self, request: String) -> Raw {
        let (addr, pin) = (self.addr, self.pin);
        tokio::task::spawn_blocking(move || raw_request(addr, pin, &request))
            .await
            .unwrap()
    }
}

type Socket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

/// A refused upgrade: the status, the headers and the JSON body.
struct Failed {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn tls_stream(addr: SocketAddr, pin: [u8; 32]) -> StreamOwned<ClientConnection, TcpStream> {
    let config = client_config(Arc::new(PinVerifier::new(pin))).unwrap();
    let name = ServerName::try_from("127.0.0.1").unwrap();
    let conn = ClientConnection::new(config, name).unwrap();
    let tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    StreamOwned::new(conn, tcp)
}

fn headers_of(response: &tungstenite::http::Response<Option<Vec<u8>>>) -> BTreeMap<String, String> {
    response
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn connect(
    addr: SocketAddr,
    pin: [u8; 32],
    url: &str,
    token: Option<&str>,
) -> Result<(Socket, HandshakeResponse), Failed> {
    let mut request = url.into_client_request().unwrap();
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    }
    match tungstenite::client(request, tls_stream(addr, pin)) {
        Ok((socket, response)) => Ok((socket, response)),
        Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => {
            let headers = headers_of(&response);
            let body = response
                .body()
                .as_deref()
                .and_then(|b| serde_json::from_slice(b).ok())
                .unwrap_or(Value::Null);
            Err(Failed {
                status: response.status().as_u16(),
                headers,
                body,
            })
        }
        Err(other) => panic!("the upgrade failed in an unexpected way: {other:?}"),
    }
}

struct Raw {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn raw_request(addr: SocketAddr, pin: [u8; 32], request: &str) -> Raw {
    let mut stream = tls_stream(addr, pin);
    stream.write_all(request.as_bytes()).unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status line in {text:?}"));
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Raw {
        status,
        headers,
        body: serde_json::from_str(body).unwrap_or(Value::Null),
    }
}

/// `GET <path>` with a token and no upgrade headers.
fn plain_get(path: &str, token: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    )
}

fn assert_server_headers(server: &Server, headers: &BTreeMap<String, String>, what: &str) {
    assert_eq!(
        headers.get("pf-server-id"),
        Some(&server.fake.spki_hex),
        "pf-server-id on {what}: {headers:?}"
    );
    assert_eq!(
        headers.get("pf-server-name").map(String::as_str),
        Some(SERVER_NAME),
        "pf-server-name on {what}: {headers:?}"
    );
}

/// Read the next text frame, as JSON. A close or a failure fails the test with what it was.
fn next_json(socket: &mut Socket) -> Value {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).unwrap(),
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            other => panic!("wanted a text frame, got {other:?}"),
        }
    }
}

/// Read until the close frame. Its code and reason, and how long it took.
fn next_close(socket: &mut Socket) -> (u16, String) {
    loop {
        match socket.read() {
            Ok(Message::Close(Some(frame))) => {
                return (u16::from(frame.code), frame.reason.to_string());
            }
            Ok(Message::Close(None)) => panic!("a close frame with no code"),
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Text(_)) => continue,
            other => panic!("wanted a close frame, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_token_never_upgrades_and_counts_for_the_guard() {
    let server = Server::start().await;
    // No token, a token the host does not know, a revoked one, an idle-expired one: all answer
    // 401, and the host's `open_stream` is never called.
    let tokens: [Option<&str>; 4] = [None, Some(BAD_TOKEN), Some(REVOKED_TOKEN), Some(IDLE_TOKEN)];
    for token in tokens {
        let failed = match server.open(token, "/v1/stream").await {
            Err(f) => f,
            Ok(_) => panic!("the token {token:?} upgraded"),
        };
        assert_eq!(failed.status, 401, "{token:?}");
        assert_eq!(failed.body["error"], "not authorized");
        assert_server_headers(&server, &failed.headers, "the 401");
    }
    assert!(server.fake.opens().is_empty(), "open_stream was called");
    // The fifth token failure from this peer fills the guard's counter; the sixth request, even
    // with a good token, is a 429 and reaches no host method.
    let fifth = server
        .open(Some(BAD_TOKEN), "/v1/stream")
        .await
        .err()
        .unwrap();
    assert_eq!(fifth.status, 401);
    let sixth = server
        .open(Some(TOKEN_A), "/v1/stream")
        .await
        .err()
        .unwrap();
    assert_eq!(sixth.status, 429, "{:?}", sixth.body);
    assert_server_headers(&server, &sixth.headers, "the 429");
    assert!(server.fake.opens().is_empty(), "open_stream was called");
    let audits = server.fake.audits();
    assert_eq!(audits.len(), 6, "{audits:?}");
    assert!(
        audits.iter().all(|a| a.route == Route::Stream),
        "{audits:?}"
    );
    assert_eq!(audits[0].outcome, Outcome::Unauthorized);
    assert_eq!(audits[5].outcome, Outcome::Blocked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_good_token_upgrades_and_receives_the_hosts_frames_in_order() {
    let server = Server::start().await;
    let (mut socket, response) = server
        .open(Some(TOKEN_A), "/v1/stream?session=S-1&after=5")
        .await
        .ok()
        .expect("the upgrade");
    assert_eq!(response.status().as_u16(), 101);
    assert_server_headers(&server, &headers_of(&response), "the 101");
    assert_eq!(
        server.fake.opens(),
        vec![("D-1".to_string(), Some("S-1".to_string()), 5)]
    );
    let tx = server.fake.feed(0);
    for n in 1..=3 {
        tx.send(StreamEvent::Frame(format!(r#"{{"type":"item","n":{n}}}"#)))
            .await
            .unwrap();
    }
    let seen = tokio::task::spawn_blocking(move || {
        (0..4)
            .map(|_| next_json(&mut socket)["n"].clone())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();
    assert_eq!(seen, vec![0, 1, 2, 3], "the frames arrive in order");
    // With no query the host gets no session and `after` 0.
    let _second = server.open(Some(TOKEN_B), "/v1/stream").await.ok().unwrap();
    assert_eq!(server.fake.opens()[1], ("D-2".to_string(), None, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_that_is_no_upgrade_is_refused_before_the_host() {
    let server = Server::start().await;
    // A method other than GET is 405.
    let post = server
        .raw(format!(
            "POST /v1/stream HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN_A}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ))
        .await;
    assert_eq!(post.status, 405);
    assert_server_headers(&server, &post.headers, "the 405");
    // A GET with a good token and no upgrade headers is 400.
    let plain = server.raw(plain_get("/v1/stream", TOKEN_A)).await;
    assert_eq!(plain.status, 400, "{:?}", plain.body);
    assert_server_headers(&server, &plain.headers, "the 400");
    // A bad session id and a bad `after` are 400, and an unknown session is 404.
    for target in [
        "/v1/stream?session=x",
        "/v1/stream?session=S-",
        "/v1/stream?after=zz",
        "/v1/stream?after=-1",
    ] {
        let failed = server.open(Some(TOKEN_A), target).await.err().unwrap();
        assert_eq!(failed.status, 400, "{target}: {:?}", failed.body);
    }
    assert!(
        server.fake.opens().is_empty(),
        "the query was checked after the host was called"
    );
    let unknown = server
        .open(Some(TOKEN_A), "/v1/stream?session=S-404")
        .await
        .err()
        .unwrap();
    assert_eq!(unknown.status, 404, "{:?}", unknown.body);
    assert_server_headers(&server, &unknown.headers, "the 404");
    assert_eq!(server.fake.opens().len(), 1);
    // A refusal that is no token failure leaves the peer unblocked: the status route still answers.
    let status = server.raw(plain_get("/v1/status", TOKEN_A)).await;
    assert_eq!(status.status, 200);
    assert_server_headers(&server, &status.headers, "the status answer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_closes_a_stream_with_the_codes_of_the_contract() {
    let server = Server::start().await;
    // `revoked` closes with 4001 within a second.
    let (mut socket, _) = server.open(Some(TOKEN_A), "/v1/stream").await.ok().unwrap();
    let tx = server.fake.feed(0);
    let sent = Instant::now();
    tx.send(StreamEvent::End(StreamEnd::Revoked)).await.unwrap();
    let (code, reason) = tokio::task::spawn_blocking(move || {
        let _hello = next_json(&mut socket);
        next_close(&mut socket)
    })
    .await
    .unwrap();
    assert_eq!((code, reason.as_str()), (4001, "revoked"));
    assert!(
        sent.elapsed() < Duration::from_secs(1),
        "{:?}",
        sent.elapsed()
    );

    // `lagged` closes with 4002 and the sequence number to reconnect after.
    let (mut socket, _) = server.open(Some(TOKEN_A), "/v1/stream").await.ok().unwrap();
    server
        .fake
        .feed(1)
        .send(StreamEvent::End(StreamEnd::Lagged { after_seq: 7 }))
        .await
        .unwrap();
    let (code, reason) = tokio::task::spawn_blocking(move || {
        let _hello = next_json(&mut socket);
        next_close(&mut socket)
    })
    .await
    .unwrap();
    assert_eq!(
        (code, reason.as_str()),
        (4002, "lagged: reconnect after seq 7")
    );

    // A controller stop closes with 1001, whether the host says so or drops the feed.
    let (mut socket, _) = server.open(Some(TOKEN_A), "/v1/stream").await.ok().unwrap();
    server
        .fake
        .feed(2)
        .send(StreamEvent::End(StreamEnd::Shutdown))
        .await
        .unwrap();
    let (code, _) = tokio::task::spawn_blocking(move || {
        let _hello = next_json(&mut socket);
        next_close(&mut socket)
    })
    .await
    .unwrap();
    assert_eq!(code, 1001);

    // The stream is read-only: a data frame from the client closes it with 1008.
    let (mut socket, _) = server.open(Some(TOKEN_A), "/v1/stream").await.ok().unwrap();
    let (code, _) = tokio::task::spawn_blocking(move || {
        let _hello = next_json(&mut socket);
        socket.send(Message::Text("hello".into())).unwrap();
        next_close(&mut socket)
    })
    .await
    .unwrap();
    assert_eq!(code, 1008);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_holds_at_most_eight_streams() {
    let server = Server::start().await;
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(
            server
                .open(Some(TOKEN_A), "/v1/stream")
                .await
                .ok()
                .unwrap()
                .0,
        );
    }
    // The ninth is 429 with its own text, and it is no failure for the peer guard: six refusals
    // leave the peer able to call.
    for _ in 0..6 {
        let ninth = server
            .open(Some(TOKEN_A), "/v1/stream")
            .await
            .err()
            .unwrap();
        assert_eq!(ninth.status, 429);
        assert_eq!(ninth.body["error"], "too many open streams");
    }
    assert_eq!(
        server.raw(plain_get("/v1/status", TOKEN_A)).await.status,
        200
    );
    // Another device has its own eight.
    let _other = server.open(Some(TOKEN_B), "/v1/stream").await.ok().unwrap();
    // A closed stream frees its place.
    let mut first = held.remove(0);
    tokio::task::spawn_blocking(move || {
        first.close(None).unwrap();
        // Read until the server's reply closes the connection.
        while first.read().is_ok() {}
    })
    .await
    .unwrap();
    let mut freed = false;
    for _ in 0..40 {
        if server.open(Some(TOKEN_A), "/v1/stream").await.is_ok() {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(freed, "the place of the closed stream was not freed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_connection_is_one_audit_record_and_the_close_adds_none() {
    let server = Server::start().await;
    let (mut socket, _) = server.open(Some(TOKEN_A), "/v1/stream").await.ok().unwrap();
    let failed = server
        .open(Some(BAD_TOKEN), "/v1/stream")
        .await
        .err()
        .unwrap();
    assert_eq!(failed.status, 401);
    server
        .fake
        .feed(0)
        .send(StreamEvent::End(StreamEnd::Shutdown))
        .await
        .unwrap();
    tokio::task::spawn_blocking(move || {
        let _hello = next_json(&mut socket);
        next_close(&mut socket)
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let audits = server.fake.audits();
    assert_eq!(audits.len(), 2, "{audits:?}");
    let upgraded = &audits[0];
    assert_eq!(upgraded.route, Route::Stream);
    assert_eq!(upgraded.outcome, Outcome::Upgraded);
    assert_eq!(upgraded.status, 101);
    assert_eq!(upgraded.device.as_deref(), Some("D-1"));
    assert_eq!(upgraded.verb, None);
    assert_eq!(audits[1].outcome, Outcome::Unauthorized);
    assert_eq!(audits[1].route, Route::Stream);
    assert_eq!(Outcome::Upgraded.as_str(), "upgraded");
    assert_eq!(Route::Stream.as_str(), "stream");
    // `hex` is the code module's: the header holds the hash the host named.
    assert_eq!(hex(&server.pin), server.fake.spki_hex);
}
