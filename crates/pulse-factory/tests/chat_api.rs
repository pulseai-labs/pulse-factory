//! AC-9 of `r1.s3.w3`: the chat verbs and the chat stream over the real API. A live controller with
//! a `FakeChat`, the API on `127.0.0.1:0`, a paired device, the verbs over pinned TLS and the stream
//! with a WebSocket client. Nothing here reaches beyond `127.0.0.1` or starts `claude`.

mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::chat::*;
use common::ctl::Live;
use pf_client::pin::{PinVerifier, client_config};
use pf_ipc::Handler;
use pulse_factory::controller::api;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, HandshakeError, Message, WebSocket};

type Socket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

const SERVER_NAME: &str = "test-server";

fn unhex(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn tls_stream(addr: SocketAddr, pin: [u8; 32]) -> StreamOwned<ClientConnection, TcpStream> {
    let config = client_config(Arc::new(PinVerifier::new(pin))).unwrap();
    let name = ServerName::try_from("127.0.0.1").unwrap();
    let conn = ClientConnection::new(config, name).unwrap();
    let tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    StreamOwned::new(conn, tcp)
}

/// The API of a live controller, on a runtime of its own.
struct Api {
    live: Live,
    addr: SocketAddr,
    spki: String,
    pin: [u8; 32],
    /// How many stream upgrades this test asked for, answered or refused.
    connections: std::cell::Cell<usize>,
    /// The verb calls this test made.
    calls: std::cell::RefCell<Vec<String>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Api {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn api() -> Api {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec, |cfg| cfg.api_listen = Some("127.0.0.1:0".to_string()));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let controller = live.controller.clone();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let _api = api::spawn_api(&controller);
            let _ = rx.await;
        });
    });
    let status = || {
        live.controller
            .handle(pf_ipc::Request::new("api_status", json!({})))
            .result
    };
    let end = Instant::now() + Duration::from_secs(10);
    while status()["state"] != "listening" {
        assert!(Instant::now() < end, "the API never listened: {}", status());
        std::thread::sleep(Duration::from_millis(25));
    }
    let s = status();
    let addr: SocketAddr = s["listen"].as_str().unwrap().parse().unwrap();
    let spki = s["key_spki_sha256"].as_str().unwrap().to_string();
    Api {
        live,
        addr,
        pin: unhex(&spki),
        spki,
        connections: Default::default(),
        calls: Default::default(),
        stop: Some(tx),
        thread: Some(thread),
    }
}

struct Answer {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Value,
}

impl Api {
    /// One HTTPS call, pinned. Every answer must name the server.
    fn http(&self, method: &str, path: &str, token: Option<&str>, body: Option<&str>) -> Answer {
        let mut stream = tls_stream(self.addr, self.pin);
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
        if let Some(t) = token {
            head.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        let payload = body.unwrap_or("");
        if body.is_some() {
            head.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                payload.len()
            ));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(payload.as_bytes()).unwrap();
        let mut bytes = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    assert!(Instant::now() < deadline, "no answer to {method} {path}");
                }
                Err(_) => break,
            }
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {text:?}"));
        let headers: BTreeMap<String, String> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        self.assert_names_the_server(&headers, &format!("{method} {path}"));
        Answer {
            status,
            headers,
            body: serde_json::from_str(body).unwrap_or(Value::Null),
        }
    }

    fn assert_names_the_server(&self, headers: &BTreeMap<String, String>, what: &str) {
        assert_eq!(
            headers.get("pf-server-id"),
            Some(&self.spki),
            "pf-server-id on {what}: {headers:?}"
        );
        assert_eq!(
            headers.get("pf-server-name").map(String::as_str),
            Some(SERVER_NAME),
            "pf-server-name on {what}"
        );
    }

    /// A verb call with a token.
    fn verb(&self, token: &str, verb: &str, args: Value) -> Answer {
        self.calls.borrow_mut().push(verb.to_string());
        self.http(
            "POST",
            &format!("/v1/verbs/{verb}"),
            Some(token),
            Some(&args.to_string()),
        )
    }

    /// Pair a device through the real route and answer its token.
    fn pair(&self, name: &str) -> String {
        // Through the socket, as the CLI does: the controller checks who calls.
        let made = pf_ipc::call(&self.live.sb.state, "device_pair", json!({})).unwrap();
        assert!(made.ok, "{:?}", made.error);
        let code = made.result["code"].as_str().unwrap().to_string();
        let body = json!({"code": code, "device_name": name}).to_string();
        let reply = self.http("POST", "/v1/pair", None, Some(&body));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.body["token"].as_str().unwrap().to_string()
    }

    /// Open the stream. `Err` is the refusal's status.
    fn stream(&self, token: &str, query: &str) -> Result<(Socket, BTreeMap<String, String>), u16> {
        self.connections.set(self.connections.get() + 1);
        let url = format!("wss://127.0.0.1:{}/v1/stream{query}", self.addr.port());
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tungstenite::client(request, tls_stream(self.addr, self.pin)) {
            Ok((socket, response)) => {
                let headers = response
                    .headers()
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.as_str().to_ascii_lowercase(),
                            v.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                self.assert_names_the_server(&headers, "the 101");
                Ok((socket, headers))
            }
            Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => {
                Err(response.status().as_u16())
            }
            Err(other) => panic!("the upgrade failed in an unexpected way: {other:?}"),
        }
    }

    fn ledger(&self) -> Vec<String> {
        lines(&self.live.sb.state.join("api.ledger"))
    }

    /// A project with a repo, added through the controller.
    fn project(&self, name: &str) -> String {
        add_project(&self.live, &repo(&self.live, name))
    }
}

/// What the next read of a stream gave.
enum Got {
    Frame(Value),
    Close(u16, String),
    Idle,
}

fn read(socket: &mut Socket) -> Got {
    match socket.read() {
        Ok(Message::Text(text)) => Got::Frame(serde_json::from_str(text.as_str()).unwrap()),
        Ok(Message::Close(Some(frame))) => {
            Got::Close(u16::from(frame.code), frame.reason.to_string())
        }
        Ok(Message::Close(None)) => Got::Close(0, String::new()),
        Ok(_) => Got::Idle,
        Err(tungstenite::Error::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Got::Idle
        }
        Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
            Got::Close(0, String::new())
        }
        Err(e) => panic!("the stream failed: {e}"),
    }
}

/// Collect frames until `done` says so. It fails with what it saw after 20 seconds, or at a close.
fn collect(socket: &mut Socket, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let end = Instant::now() + Duration::from_secs(20);
    let mut frames = Vec::new();
    while !done(&frames) {
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}; frames: {}",
            summary(&frames)
        );
        match read(socket) {
            Got::Frame(f) => frames.push(f),
            Got::Close(code, why) => {
                panic!(
                    "closed {code} {why} while waiting for {what}: {}",
                    summary(&frames)
                )
            }
            Got::Idle => {}
        }
    }
    frames
}

fn summary(frames: &[Value]) -> String {
    frames
        .iter()
        .map(|f| match f["type"].as_str() {
            Some("item") => format!("item:{}:{}", f["item"]["seq"], f["item"]["body"]["kind"]),
            Some(t) => t.to_string(),
            None => "?".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn item_kind(f: &Value) -> Option<&str> {
    (f["type"] == "item").then(|| f["item"]["body"]["kind"].as_str().unwrap_or("?"))
}

fn has_item(frames: &[Value], kind: &str) -> bool {
    frames.iter().any(|f| item_kind(f) == Some(kind))
}

/// Wait for a close frame, and how long it took.
fn close_of(socket: &mut Socket, within: Duration) -> (u16, String, Duration) {
    let start = Instant::now();
    loop {
        match read(socket) {
            Got::Close(code, why) => return (code, why, start.elapsed()),
            Got::Frame(_) | Got::Idle => {}
        }
        assert!(
            start.elapsed() < within + Duration::from_secs(5),
            "no close frame"
        );
    }
}

fn started_over_the_api(api: &Api, token: &str, project: &str) -> String {
    let reply = api.verb(
        token,
        "session_start",
        json!({"project_id": project, "command_id": format!("start-{project}")}),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.body["session"]["id"].as_str().unwrap().to_string()
}

#[test]
fn the_verbs_and_the_stream_carry_a_chat_with_no_polling() {
    let api = api();
    let token = api.pair("macbook");
    let project = api.project("alpha");

    // `session_start` over the API; its answer names the server.
    let reply = api.verb(
        &token,
        "session_start",
        json!({"project_id": project, "command_id": "k-start"}),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.body["session"]["id"], "S-1");
    assert_eq!(reply.headers["pf-server-id"], api.spki);

    // The stream that follows S-1 gets `hello` with the server, then `caught`.
    let (mut follow, headers) = api.stream(&token, "?session=S-1&after=0").unwrap();
    api.assert_names_the_server(&headers, "the stream");
    let first = collect(&mut follow, "hello and caught", |f| {
        f.iter().any(|x| x["type"] == "caught")
    });
    assert_eq!(first[0]["type"], "hello", "{}", summary(&first));
    assert_eq!(first[0]["server"]["id"], api.spki.as_str());
    assert_eq!(first[0]["server"]["name"], SERVER_NAME);
    assert_eq!(first[0]["following"], "S-1");
    assert_eq!(first[0]["after_seq"], 0);
    assert!(
        first[0]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "S-1" && s["process"]["qualified"] == true),
        "{}",
        first[0]
    );
    let caught = first.iter().find(|f| f["type"] == "caught").unwrap();
    assert_eq!(caught["session_id"], "S-1");
    assert_eq!(caught["last_seq"], 0);

    // A stream with no session: `hello`, and the `session` frames only.
    let (mut overview, _) = api.stream(&token, "").unwrap();
    let hello = collect(&mut overview, "hello", |f| !f.is_empty());
    assert_eq!(hello[0]["type"], "hello");
    assert!(hello[0]["following"].is_null());

    // A message: the deltas come before the text item, then the turn, then the new cost.
    let sent = api.verb(
        &token,
        "session_send",
        json!({"session_id": "S-1", "text": "hello", "command_id": "k-send"}),
    );
    assert_eq!(sent.status, 200, "{}", sent.body);
    assert_eq!(sent.body["session"]["id"], "S-1");
    let frames = collect(&mut follow, "the turn and the new cost", |f| {
        has_item(f, "turn_result")
            && f.iter()
                .any(|x| x["type"] == "session" && x["session"]["usage"]["cost_micros"] == 270)
    });
    let at = |pred: &dyn Fn(&Value) -> bool| frames.iter().position(pred);
    let delta = at(&|f| f["type"] == "delta").expect("a delta frame");
    let text = at(&|f| item_kind(f) == Some("assistant_text")).expect("the text item");
    let result = at(&|f| item_kind(f) == Some("turn_result")).expect("the turn result");
    assert!(delta < text && text < result, "{}", summary(&frames));
    assert_eq!(frames[delta]["session_id"], "S-1");
    let kinds: Vec<_> = frames.iter().filter_map(item_kind).collect();
    assert_eq!(kinds[0], "operator_message", "{}", summary(&frames));
    let turn = &frames[result]["item"]["body"];
    assert_eq!(turn["usage"]["cost_micros"], 270);

    // The overview stream saw `session` frames and nothing of the chat's own.
    let seen = collect(&mut overview, "a session frame", |f| {
        f.iter().any(|x| x["type"] == "session")
    });
    std::thread::sleep(Duration::from_millis(200));
    let mut more = Vec::new();
    while let Got::Frame(f) = read(&mut overview) {
        more.push(f);
    }
    for f in seen.iter().chain(more.iter()) {
        assert!(
            f["type"] == "session",
            "an overview stream carries session frames only: {f}"
        );
    }

    // A background task: its turn arrives on the stream with no call by the client (A8).
    let calls_before = api.calls.borrow().len();
    let sent = api.verb(
        &token,
        "session_send",
        json!({"session_id": "S-1", "text": "fake:task 200", "command_id": "k-task"}),
    );
    assert_eq!(sent.status, 200, "{}", sent.body);
    let frames = collect(&mut follow, "the task turn", |f| {
        f.iter()
            .filter(|x| item_kind(x) == Some("turn_result"))
            .count()
            >= 2
    });
    assert!(has_item(&frames, "background_task"), "{}", summary(&frames));
    let causes: Vec<_> = frames
        .iter()
        .filter(|f| item_kind(f) == Some("turn_result"))
        .map(|f| f["item"]["body"]["cause"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(causes, ["operator", "task"], "{}", summary(&frames));
    assert_eq!(
        api.calls.borrow().len(),
        calls_before + 1,
        "the client made no more calls"
    );

    // Redact and halt are off the allowlist, over the API.
    for verb in ["session_redact", "halt", "session_items"] {
        let reply = api.verb(&token, verb, json!({}));
        assert_eq!(reply.status, 403, "{verb}: {}", reply.body);
    }

    // The request ledger: one stream line per connection and one line per verb call.
    let ledger = api.ledger();
    let streams: Vec<_> = ledger
        .iter()
        .filter(|l| l.contains("route=stream"))
        .collect();
    assert_eq!(streams.len(), api.connections.get(), "{ledger:#?}");
    assert!(
        streams.iter().all(|l| l.contains("outcome=upgraded")),
        "{streams:?}"
    );
    let verbs: Vec<_> = ledger.iter().filter(|l| l.contains("route=verb")).collect();
    assert_eq!(verbs.len(), api.calls.borrow().len(), "{ledger:#?}");
    for name in ["session_start", "session_send", "session_redact", "halt"] {
        assert!(
            verbs.iter().any(|l| l.contains(&format!("verb={name} "))),
            "{name} in the ledger"
        );
    }
    for line in &ledger {
        assert!(!line.contains(&token), "the token is not in the ledger");
        assert!(!line.contains("hello"), "no message text in the ledger");
    }
}

#[test]
fn a_reconnect_gets_the_items_after_its_seq_and_then_the_live_ones() {
    let api = api();
    let token = api.pair("macbook");
    let project = api.project("alpha");
    let s = started_over_the_api(&api, &token, &project);
    api.verb(
        &token,
        "session_send",
        json!({"session_id": s, "text": "one", "command_id": "k-1"}),
    );
    wait_turns(&api.live, &s, 1);
    let stored = items(&api.live, &s);
    let last = stored.last().unwrap().seq;
    assert!(last >= 3, "{last}");

    // `after=2` gets exactly the items after 2, then `caught`.
    let (mut socket, _) = api
        .stream(&token, &format!("?session={s}&after=2"))
        .unwrap();
    let frames = collect(&mut socket, "caught", |f| {
        f.iter().any(|x| x["type"] == "caught")
    });
    assert_eq!(frames[0]["type"], "hello");
    assert_eq!(frames[0]["after_seq"], 2);
    let got: Vec<u64> = frames
        .iter()
        .filter(|f| f["type"] == "item")
        .map(|f| f["item"]["seq"].as_u64().unwrap())
        .collect();
    let want: Vec<u64> = stored.iter().map(|i| i.seq).filter(|n| *n > 2).collect();
    assert_eq!(got, want);
    drop(socket);

    // A turn runs while the client reconnects: no gap and no double.
    api.verb(
        &token,
        "session_send",
        json!({"session_id": s, "text": "fake:slow 600", "command_id": "k-2"}),
    );
    wait_for(&api.live, &s, "the second message", |i| {
        i.iter().filter(|x| x.kind == "operator_message").count() >= 2
    });
    let (mut socket, _) = api
        .stream(&token, &format!("?session={s}&after={last}"))
        .unwrap();
    let frames = collect(&mut socket, "the second turn", |f| {
        f.iter()
            .filter(|x| item_kind(x) == Some("turn_result"))
            .count()
            >= 1
    });
    let got: Vec<u64> = frames
        .iter()
        .filter(|f| f["type"] == "item")
        .map(|f| f["item"]["seq"].as_u64().unwrap())
        .collect();
    let now = items(&api.live, &s);
    let want: Vec<u64> = now.iter().map(|i| i.seq).filter(|n| *n > last).collect();
    assert_eq!(got, want, "{}", summary(&frames));
    let caught = frames
        .iter()
        .position(|f| f["type"] == "caught")
        .expect("caught");
    let live_part = &frames[caught + 1..];
    assert!(
        live_part
            .iter()
            .any(|f| item_kind(f) == Some("turn_result")),
        "the turn's end arrives live: {}",
        summary(&frames)
    );
}

#[test]
fn a_redaction_reaches_a_reader_that_already_holds_the_item() {
    let api = api();
    let token = api.pair("macbook");
    let project = api.project("alpha");
    let s = started_over_the_api(&api, &token, &project);
    api.verb(
        &token,
        "session_send",
        json!({"session_id": s, "text": "a secret", "command_id": "k-1"}),
    );
    wait_turns(&api.live, &s, 1);

    // The reader follows from 0 and holds item 1 as it was written.
    let (mut socket, _) = api
        .stream(&token, &format!("?session={s}&after=0"))
        .unwrap();
    let frames = collect(&mut socket, "caught", |f| {
        f.iter().any(|x| x["type"] == "caught")
    });
    let held = frames
        .iter()
        .find(|f| f["type"] == "item" && f["item"]["seq"] == 1)
        .expect("the reader holds item 1");
    assert_eq!(held["item"]["body"]["kind"], "operator_message");
    assert!(held["item"]["redacted_at"].is_null());

    // The operator redacts item 1 on the CLI. The open stream gets the item again with the marks.
    let out = api.live.cli(&["session", "redact", &s, "1"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let frames = collect(&mut socket, "the redacted item 1", |f| {
        f.iter()
            .any(|x| x["type"] == "item" && x["item"]["seq"] == 1)
    });
    let again = frames
        .iter()
        .find(|f| f["type"] == "item" && f["item"]["seq"] == 1)
        .unwrap();
    assert!(
        again["item"]["redacted_at"].is_string(),
        "{}",
        summary(&frames)
    );
    assert_eq!(again["item"]["body"]["kind"], "redacted");
    assert!(!again.to_string().contains("a secret"));

    // The re-send does not move the reader back: the next item is still delivered once.
    api.verb(
        &token,
        "session_send",
        json!({"session_id": s, "text": "two", "command_id": "k-2"}),
    );
    let frames = collect(&mut socket, "the second turn", |f| {
        f.iter().any(|x| item_kind(x) == Some("turn_result"))
    });
    let seqs: Vec<u64> = frames
        .iter()
        .filter(|f| f["type"] == "item")
        .map(|f| f["item"]["seq"].as_u64().unwrap())
        .collect();
    let mut once = seqs.clone();
    once.dedup();
    assert_eq!(seqs, once, "{}", summary(&frames));
}

#[test]
fn a_revoke_closes_that_devices_streams_and_leaves_the_others() {
    let api = api();
    let first = api.pair("one");
    let second = api.pair("two");
    let project = api.project("alpha");
    let s = started_over_the_api(&api, &first, &project);

    let (mut a1, _) = api.stream(&first, &format!("?session={s}")).unwrap();
    let (mut a2, _) = api.stream(&first, "").unwrap();
    let (mut b, _) = api.stream(&second, &format!("?session={s}")).unwrap();
    for socket in [&mut a1, &mut a2, &mut b] {
        collect(socket, "hello", |f| !f.is_empty());
    }

    let revoked = pf_ipc::call(
        &api.live.sb.state,
        "device_revoke",
        json!({"id": "D-1", "command_id": "revoke-1"}),
    )
    .unwrap();
    assert!(revoked.ok, "{:?}", revoked.error);
    for socket in [&mut a1, &mut a2] {
        let (code, why, took) = close_of(socket, Duration::from_secs(1));
        assert_eq!((code, why.as_str()), (4001, "revoked"));
        assert!(took <= Duration::from_secs(1), "closed after {took:?}");
    }

    // The other device's stream is still open and still gets frames.
    let sent = api.verb(
        &second,
        "session_send",
        json!({"session_id": s, "text": "still here", "command_id": "k-b"}),
    );
    assert_eq!(sent.status, 200, "{}", sent.body);
    let frames = collect(&mut b, "the turn on the other stream", |f| {
        has_item(f, "turn_result")
    });
    assert!(has_item(&frames, "assistant_text"));

    // The revoked token is refused at once, on the verbs and on the stream.
    let before = items(&api.live, &s).len();
    let refused = api.verb(
        &first,
        "session_send",
        json!({"session_id": s, "text": "no", "command_id": "k-a"}),
    );
    assert_eq!(refused.status, 401, "{}", refused.body);
    assert_eq!(items(&api.live, &s).len(), before, "nothing was admitted");
    assert_eq!(api.stream(&first, "").err(), Some(401));
}
