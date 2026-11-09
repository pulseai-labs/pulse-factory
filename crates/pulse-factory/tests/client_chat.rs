//! AC-5 of `r1.s3.w4`: the chat client against a real controller. A live controller with a strict
//! `FakeChat` and the API on `127.0.0.1:0` (`api::spawn_api`, as `chat_api.rs` runs it), a device
//! paired through `device_pair` over the socket, and `pf_client::transport::Transport<FileStore>` as
//! the client. Nothing here reaches beyond `127.0.0.1` or starts `claude`.

mod common;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::chat::*;
use common::ctl::Live;
use pf_api::types::{
    ChatAnswerArg, SessionAnswerArgs, SessionEndArgs, SessionInterruptArgs, SessionListArgs,
    SessionSendArgs, SessionSetArgs, SessionStartArgs,
};
use pf_client::error::ErrorKind;
use pf_client::store::{DeviceStore, FileStore};
use pf_client::stream::{ChatStream, StreamEvent, StreamOptions, StreamTuning};
use pf_client::transport::{PairedServer, Transport};
use pf_ipc::Handler;
use pulse_factory::controller::api;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const SERVER_NAME: &str = "test-server";

/// The API of a live controller, on a runtime of its own.
struct Api {
    live: Live,
    addr: SocketAddr,
    spki: String,
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
        spki,
        stop: Some(tx),
        thread: Some(thread),
    }
}

impl Api {
    fn ledger(&self) -> Vec<String> {
        lines(&self.live.sb.state.join("api.ledger"))
    }

    fn count(&self, route: &str) -> usize {
        self.ledger()
            .iter()
            .filter(|l| l.contains(&format!("route={route} ")))
            .count()
    }

    /// A one-use pairing code, through the socket as the CLI does.
    fn code(&self) -> String {
        let made = pf_ipc::call(&self.live.sb.state, "device_pair", json!({})).unwrap();
        assert!(made.ok, "{:?}", made.error);
        made.result["code"].as_str().unwrap().to_string()
    }

    /// The stored items of a session, as the `session_items` verb answers them over the socket.
    fn item_seqs(&self, session: &str) -> Vec<u64> {
        let answer = pf_ipc::call(
            &self.live.sb.state,
            "session_items",
            json!({"session_id": session, "after": 0}),
        )
        .unwrap();
        assert!(answer.ok, "{:?}", answer.error);
        answer.result["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["seq"].as_u64().unwrap())
            .collect()
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A client with a store of its own, not paired yet.
struct Client {
    transport: Transport<FileStore>,
    store: Arc<FileStore>,
    _dir: tempfile::TempDir,
}

fn fast() -> StreamTuning {
    StreamTuning {
        schedule: vec![Duration::from_millis(50), Duration::from_millis(100)],
        reset_after: Duration::from_secs(60),
        silence: Duration::from_secs(5),
    }
}

fn client() -> Client {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    Client {
        transport: Transport::new(store.clone()).with_stream_tuning(fast()),
        store,
        _dir: dir,
    }
}

async fn pair(api: &Api, c: &Client, name: &str) -> PairedServer {
    c.transport.pair(&api.code(), name).await.unwrap()
}

fn token_of(c: &Client) -> String {
    c.store.load().unwrap().unwrap().token
}

fn follow(session: &str) -> StreamOptions {
    StreamOptions {
        session: Some(session.to_string()),
        after: 0,
    }
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

async fn next_event(stream: &mut ChatStream, what: &str) -> StreamEvent {
    match tokio::time::timeout(Duration::from_secs(20), stream.next()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("the stream ended while waiting for {what}"),
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// Collect frames until `done` says so. A `closed` event or a `reconnecting` one fails the test.
async fn collect(
    stream: &mut ChatStream,
    what: &str,
    done: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let mut frames = Vec::new();
    while !done(&frames) {
        match next_event(stream, what).await {
            StreamEvent::Frame { frame } => frames.push(serde_json::to_value(frame).unwrap()),
            other => panic!("{other:?} while waiting for {what}: {}", summary(&frames)),
        }
    }
    frames
}

/// A TCP relay on `127.0.0.1` in front of `to`. `cut` drops every connection through it, and the
/// relay keeps accepting.
struct Relay {
    addr: SocketAddr,
    cuts: tokio::sync::watch::Sender<u64>,
}

async fn relay(to: SocketAddr) -> Relay {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (cuts, _) = tokio::sync::watch::channel(0u64);
    let watch = cuts.clone();
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            let mut cut = watch.subscribe();
            tokio::spawn(async move {
                let Ok(mut outbound) = TcpStream::connect(to).await else {
                    return;
                };
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                    _ = cut.changed() => {}
                }
            });
        }
    });
    Relay { addr, cuts }
}

impl Relay {
    fn cut(&self) {
        self.cuts.send_modify(|n| *n += 1);
    }
}

fn session_send(session: &str, text: &str, command_id: &str) -> SessionSendArgs {
    SessionSendArgs {
        session_id: session.to_string(),
        text: text.to_string(),
        command_id: command_id.to_string(),
    }
}

fn start_args(project: &str, command_id: &str) -> SessionStartArgs {
    SessionStartArgs {
        project_id: project.to_string(),
        agent_id: "orchestrator".to_string(),
        command_id: command_id.to_string(),
    }
}

#[test]
fn a_client_pairs_starts_a_chat_follows_it_and_acts_on_it() {
    let api = api();
    let root = repo(&api.live, "alpha");
    let project = add_project(&api.live, &root);
    runtime().block_on(async {
        let c = client();
        let paired = pair(&api, &c, "macbook").await;
        let named = paired.server.expect("pair names the server");
        assert_eq!(
            (named.id.as_deref(), named.name.as_str()),
            (Some(api.spki.as_str()), SERVER_NAME)
        );
        let token = token_of(&c);

        let started = c
            .transport
            .session_start(start_args(&project, "k-start"))
            .await
            .unwrap();
        assert_eq!(started.session_id, "S-1");
        assert_eq!(started.worktree_id, "W-1");
        assert_eq!(
            started.worktree_dir,
            root.display().to_string(),
            "a git project's worktree is its root"
        );
        let list = c
            .transport
            .session_list(SessionListArgs::default())
            .await
            .unwrap();
        assert_eq!(list.server.name, SERVER_NAME);
        assert_eq!(list.server.id.as_deref(), Some(api.spki.as_str()));
        assert!(list.sessions.iter().any(|s| s.id == "S-1"));

        // A stream that follows S-1 from 0 gets `hello` and then `caught`.
        let mut stream = c.transport.open_stream(follow("S-1"));
        let first = collect(&mut stream, "hello and caught", |f| {
            f.iter().any(|x| x["type"] == "caught")
        })
        .await;
        assert_eq!(first[0]["type"], "hello", "{}", summary(&first));
        assert_eq!(first[0]["server"]["id"], api.spki.as_str());
        assert_eq!(first[0]["server"]["name"], SERVER_NAME);
        assert_eq!(first[0]["following"], "S-1");

        // A message: the deltas come before the text item, then the turn, then the new cost.
        let sent = c
            .transport
            .session_send(session_send("S-1", "hello", "k-send"))
            .await
            .unwrap();
        assert_eq!(sent.session.id, "S-1");
        let frames = collect(&mut stream, "the turn and its cost", |f| {
            has_item(f, "turn_result")
                && f.iter()
                    .any(|x| x["type"] == "session" && x["session"]["usage"]["cost_micros"] == 270)
        })
        .await;
        let at = |pred: &dyn Fn(&Value) -> bool| frames.iter().position(pred);
        let delta = at(&|f| f["type"] == "delta").expect("a delta frame");
        let text = at(&|f| item_kind(f) == Some("assistant_text")).expect("the text item");
        let result = at(&|f| item_kind(f) == Some("turn_result")).expect("the turn result");
        assert!(delta < text && text < result, "{}", summary(&frames));
        assert_eq!(frames[result]["item"]["body"]["usage"]["cost_micros"], 270);

        // A background task: its turn arrives with no call by the client (A8).
        let verbs_before = api.count("verb");
        c.transport
            .session_send(session_send("S-1", "fake:task 200", "k-task"))
            .await
            .unwrap();
        let frames = collect(&mut stream, "the task turn", |f| {
            f.iter()
                .filter(|x| item_kind(x) == Some("turn_result"))
                .count()
                >= 2
        })
        .await;
        assert!(has_item(&frames, "background_task"), "{}", summary(&frames));
        assert_eq!(
            api.count("verb"),
            verbs_before + 1,
            "the client made no more calls"
        );

        // The settings: `effort` is a live option; the same command ID again is a replay.
        let set = SessionSetArgs {
            session_id: "S-1".to_string(),
            model: None,
            effort: Some("medium".to_string()),
            permission_mode: None,
            options: None,
            command_id: "k-set".to_string(),
        };
        let changed = c.transport.session_set(set.clone()).await.unwrap();
        assert!(!changed.replayed);
        assert_eq!(changed.session.effort, "medium");
        assert!(c.transport.session_set(set).await.unwrap().replayed);

        // An approval, denied with `session_answer`.
        c.transport
            .session_send(session_send("S-1", "fake:approval", "k-approval"))
            .await
            .unwrap();
        let frames = collect(&mut stream, "the approval request", |f| {
            has_item(f, "approval_request")
        })
        .await;
        let request = frames
            .iter()
            .find(|f| item_kind(f) == Some("approval_request"))
            .unwrap();
        let seq = request["item"]["seq"].as_u64().unwrap();
        let answer = SessionAnswerArgs {
            session_id: "S-1".to_string(),
            seq,
            answer: ChatAnswerArg::Deny {
                message: "not now".to_string(),
            },
            command_id: "k-answer".to_string(),
        };
        let answered = c.transport.session_answer(answer.clone()).await.unwrap();
        assert_eq!(answered.session.id, "S-1");
        assert!(c.transport.session_answer(answer).await.unwrap().replayed);
        collect(&mut stream, "the answer and the turn's end", |f| {
            has_item(f, "approval_answer") && has_item(f, "turn_result")
        })
        .await;

        // An interrupt of a long turn.
        c.transport
            .session_send(session_send("S-1", "fake:slow 5000", "k-slow"))
            .await
            .unwrap();
        collect(&mut stream, "the slow turn's start", |f| {
            has_item(f, "operator_message")
        })
        .await;
        let interrupt = SessionInterruptArgs {
            session_id: "S-1".to_string(),
            command_id: "k-int".to_string(),
        };
        assert_eq!(
            c.transport
                .session_interrupt(interrupt.clone())
                .await
                .unwrap()
                .session
                .id,
            "S-1"
        );
        assert!(
            c.transport
                .session_interrupt(interrupt)
                .await
                .unwrap()
                .replayed
        );
        collect(&mut stream, "the interrupted turn's end", |f| {
            has_item(f, "turn_result")
        })
        .await;

        // The end.
        let end = SessionEndArgs {
            session_id: "S-1".to_string(),
            command_id: "k-end".to_string(),
        };
        let ended = c.transport.session_end(end.clone()).await.unwrap();
        assert_eq!(ended.session_id, "S-1");
        assert!(!ended.replayed);
        assert!(c.transport.session_end(end).await.unwrap().replayed);

        // The ledger: one stream line for the one connection, and none added while the stream
        // stays connected and idle. The token is nowhere in it.
        let streams = api.count("stream");
        assert_eq!(streams, 1, "{:#?}", api.ledger());
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            api.count("stream"),
            streams,
            "an idle stream adds no ledger line"
        );
        let ledger = api.ledger();
        assert!(
            ledger.iter().all(|l| !l.contains(&token)),
            "the token is in the ledger"
        );
    });
}

#[test]
fn a_dropped_connection_reconnects_and_every_item_arrives_once_in_order() {
    let api = api();
    let project = add_project(&api.live, &repo(&api.live, "alpha"));
    runtime().block_on(async {
        let c = client();
        pair(&api, &c, "macbook").await;
        let token = token_of(&c);
        // The stored address is a relay in front of the API.
        let relay = relay(api.addr).await;
        let mut paired = c.store.load().unwrap().unwrap();
        paired.address = relay.addr.to_string();
        c.store.save(&paired).unwrap();

        c.transport
            .session_start(start_args(&project, "k-start"))
            .await
            .unwrap();
        let mut stream = c.transport.open_stream(follow("S-1"));
        collect(&mut stream, "hello and caught", |f| {
            f.iter().any(|x| x["type"] == "caught")
        })
        .await;
        c.transport
            .session_send(session_send("S-1", "fake:slow 600", "k-slow"))
            .await
            .unwrap();

        // The relay is cut while the turn runs.
        let mut seqs: Vec<u64> = Vec::new();
        let mut reconnecting = 0;
        let mut cut = false;
        let mut done = false;
        while !done {
            match next_event(&mut stream, "the turn across a cut").await {
                StreamEvent::Frame { frame } => {
                    let f = serde_json::to_value(frame).unwrap();
                    if f["type"] == "item" {
                        seqs.push(f["item"]["seq"].as_u64().unwrap());
                        if item_kind(&f) == Some("turn_result") {
                            done = true;
                        }
                        if !cut && item_kind(&f) == Some("operator_message") {
                            relay.cut();
                            cut = true;
                        }
                    }
                }
                StreamEvent::Reconnecting { reason, .. } => {
                    assert_eq!(reason, "the connection dropped");
                    reconnecting += 1;
                }
                StreamEvent::Closed { error } => panic!("closed: {error:?}"),
            }
        }
        assert!(
            cut && reconnecting >= 1,
            "the cut was seen: {reconnecting} reconnects"
        );
        // Every item of the session exactly once, in `seq` order with no gap.
        let stored = api.item_seqs("S-1");
        assert_eq!(seqs, stored, "the client's items are the stored ones");
        assert_eq!(
            api.count("stream"),
            2,
            "one line for each connection: {:#?}",
            api.ledger()
        );
        assert!(api.ledger().iter().all(|l| !l.contains(&token)));
    });
}

#[test]
fn a_revoke_ends_the_stream_and_the_next_call_is_unauthorized() {
    let api = api();
    let project = add_project(&api.live, &repo(&api.live, "alpha"));
    runtime().block_on(async {
        let c = client();
        let paired = pair(&api, &c, "macbook").await;
        c.transport
            .session_start(start_args(&project, "k-start"))
            .await
            .unwrap();
        let mut stream = c.transport.open_stream(follow("S-1"));
        collect(&mut stream, "hello and caught", |f| {
            f.iter().any(|x| x["type"] == "caught")
        })
        .await;
        let streams = api.count("stream");

        let revoked = pf_ipc::call(
            &api.live.sb.state,
            "device_revoke",
            json!({"id": paired.device.id, "command_id": "revoke-1"}),
        )
        .unwrap();
        assert!(revoked.ok, "{:?}", revoked.error);
        let started = Instant::now();
        match tokio::time::timeout(Duration::from_secs(1), stream.next()).await {
            Ok(Some(StreamEvent::Closed { error })) => {
                assert_eq!(error.kind, ErrorKind::Unauthorized);
                assert_eq!(error.message, "this device was revoked; pair it again");
            }
            other => panic!("wanted closed within 1 s, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(stream.next().await.is_none(), "nothing follows closed");
        // No later upgrade reaches the ledger.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(api.count("stream"), streams);
        // The next verb call is refused by the server on its own token.
        let refused = c
            .transport
            .session_send(session_send("S-1", "no", "k-no"))
            .await
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Unauthorized);
    });
}

/// One Server-Sent Event stream of the bridge, read as raw HTTP.
struct Sse {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    _write: tokio::net::tcp::OwnedWriteHalf,
    pending: String,
    raw: String,
}

impl Sse {
    async fn open(addr: SocketAddr, target: &str) -> Sse {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (read, mut write) = tcp.into_split();
        let request = format!("GET {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        write.write_all(request.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(read);
        let mut raw = String::new();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains(" 200 "), "{line}");
        loop {
            raw.push_str(&line);
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        assert!(
            raw.to_ascii_lowercase()
                .contains("content-type: text/event-stream")
        );
        Sse {
            reader,
            _write: write,
            pending: String::new(),
            raw,
        }
    }

    async fn event(&mut self) -> Value {
        loop {
            if let Some(at) = self.pending.find("\n\n") {
                let block: String = self.pending.drain(..at + 2).collect();
                let data = block
                    .lines()
                    .find_map(|l| l.strip_prefix("data: "))
                    .unwrap();
                return serde_json::from_str(data).unwrap();
            }
            let mut size_line = String::new();
            tokio::time::timeout(
                Duration::from_secs(20),
                self.reader.read_line(&mut size_line),
            )
            .await
            .expect("an event within 20 s")
            .unwrap();
            let size = usize::from_str_radix(size_line.trim(), 16).expect("a chunk size");
            assert!(size > 0, "the response ended: {}", self.raw);
            let mut data = vec![0u8; size];
            self.reader.read_exact(&mut data).await.unwrap();
            let mut crlf = [0u8; 2];
            self.reader.read_exact(&mut crlf).await.unwrap();
            let text = String::from_utf8_lossy(&data).into_owned();
            self.raw.push_str(&text);
            self.pending.push_str(&text);
        }
    }

    async fn frames_until(&mut self, done: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut frames = Vec::new();
        loop {
            let event = self.event().await;
            assert_eq!(event["event"], "frame", "{event}");
            let frame = event["frame"].clone();
            let end = done(&frame);
            frames.push(frame);
            if end {
                return frames;
            }
        }
    }
}

#[test]
fn the_bridge_carries_the_same_frames_as_the_direct_stream() {
    let api = api();
    let project = add_project(&api.live, &repo(&api.live, "alpha"));
    runtime().block_on(async {
        // The direct client.
        let direct = client();
        pair(&api, &direct, "direct").await;
        direct.transport.session_start(start_args(&project, "k-start")).await.unwrap();
        // The bridge, over a device of its own.
        let ui = tempfile::tempdir().unwrap();
        std::fs::write(ui.path().join("index.html"), "<html></html>").unwrap();
        let state = tempfile::tempdir().unwrap();
        let bridge_store = Arc::new(FileStore::new(state.path().join("device.json")));
        Transport::new(bridge_store.clone())
            .pair(&api.code(), "bridge")
            .await
            .unwrap();
        let bridge_token = bridge_store.load().unwrap().unwrap().token;
        let bridge = pf_client::bridge::serve(ui.path().to_path_buf(), state.path().to_path_buf())
            .await
            .unwrap();

        let mut stream = direct.transport.open_stream(follow("S-1"));
        collect(&mut stream, "hello and caught", |f| f.iter().any(|x| x["type"] == "caught")).await;
        let mut sse = Sse::open(bridge.addr(), "/bridge/stream?session=S-1&after=0").await;
        let bridged_head = sse
            .frames_until(|f| f["type"] == "caught")
            .await;

        // A message through the bridge's verb.
        let mut post = TcpStream::connect(bridge.addr()).await.unwrap();
        let body = json!({"session_id": "S-1", "text": "hello", "command_id": "k-bridge"}).to_string();
        let request = format!(
            "POST /bridge/verbs/session_send HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            bridge.addr(),
            body.len()
        );
        post.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        post.read_to_string(&mut answer).await.unwrap();
        assert!(answer.contains(r#""ok":true"#), "{answer}");
        assert!(!answer.contains(&bridge_token), "the token is in a bridge answer");

        let is_result = |f: &Value| item_kind(f) == Some("turn_result");
        let direct_frames = collect(&mut stream, "the turn on the direct stream", |f| {
            f.iter().any(is_result)
        })
        .await;
        let bridged = sse.frames_until(is_result).await;
        assert!(!sse.raw.contains(&bridge_token), "the token is in the bridge's stream");

        // `hello` differs by the device alone (none here): the head and the turn are the same frames.
        let mut direct_all = vec![];
        direct_all.extend(direct_frames);
        assert_eq!(bridged_head.last().unwrap()["type"], "caught");
        assert_eq!(
            bridged.iter().filter(|f| f["type"] == "item").collect::<Vec<_>>(),
            direct_all.iter().filter(|f| f["type"] == "item").collect::<Vec<_>>(),
            "the items: {} / {}",
            summary(&bridged),
            summary(&direct_all)
        );
        assert_eq!(summary(&bridged), summary(&direct_all));
        // Frame for frame, apart from nothing: both streams were open before the send.
        let kinds = |frames: &[Value]| frames.iter().map(|f| f["type"].clone()).collect::<Vec<_>>();
        assert_eq!(kinds(&bridged), kinds(&direct_all));
        let by_seq: BTreeMap<u64, &Value> = bridged
            .iter()
            .filter(|f| f["type"] == "item")
            .map(|f| (f["item"]["seq"].as_u64().unwrap(), f))
            .collect();
        assert!(by_seq.len() >= 3, "{}", summary(&bridged));
        // `shutdown` waits for open connections, and an open stream is one: the page leaves first.
        drop(sse);
        drop(stream);
        bridge.shutdown().await;
    });
}
