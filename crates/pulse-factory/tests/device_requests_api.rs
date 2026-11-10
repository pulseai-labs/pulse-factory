//! AC-3 of `r1.s4.w1`: a paired device reads the requests, gets each change live on the stream, asks for
//! a confirm code and acks the spine PR, over the real API (pinned TLS on `127.0.0.1:0`). The rig is the
//! spine-close rig of `spine_close.rs`: a `FakeAdapter` and a `BareForge`, so every merge lands in a
//! local bare repo and nothing reaches GitHub.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::chat::{controller_ledger, lines};
use common::spine::*;
use common::*;
use pf_adapter_fake::{Scenario, Step};
use pf_client::pin::{PinVerifier, client_config};
use pf_engine::port::store::{RequestKind, RequestStatus, Store};
use pf_forge_bare::PrChecks;
use pulse_factory::controller::api;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, HandshakeError, Message, WebSocket};

type Socket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

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

/// The spine rig with the API on `127.0.0.1:0`, on a runtime of its own.
struct Rig {
    s: Spine,
    addr: SocketAddr,
    pin: [u8; 32],
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn rig() -> Rig {
    let s = Spine::start_with(|cfg| cfg.api_listen = Some("127.0.0.1:0".to_string()));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let controller = s.live.controller.clone();
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
        pf_ipc::call(&s.live.sb.state, "api_status", json!({}))
            .unwrap()
            .result
    };
    let end = Instant::now() + Duration::from_secs(10);
    while status()["state"] != "listening" {
        assert!(Instant::now() < end, "the API never listened: {}", status());
        std::thread::sleep(Duration::from_millis(25));
    }
    let st = status();
    let addr: SocketAddr = st["listen"].as_str().unwrap().parse().unwrap();
    let pin = unhex(st["key_spki_sha256"].as_str().unwrap());
    Rig {
        s,
        addr,
        pin,
        stop: Some(tx),
        thread: Some(thread),
    }
}

struct Answer {
    status: u16,
    body: Value,
}

impl Rig {
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
        let deadline = Instant::now() + Duration::from_secs(60);
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
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {text:?}"));
        Answer {
            status,
            body: serde_json::from_str(body).unwrap_or(Value::Null),
        }
    }

    fn verb(&self, token: &str, verb: &str, args: Value) -> Answer {
        self.http(
            "POST",
            &format!("/v1/verbs/{verb}"),
            Some(token),
            Some(&args.to_string()),
        )
    }

    /// Pair a device through the real route and answer its token.
    fn pair(&self, name: &str) -> String {
        let made = pf_ipc::call(&self.s.live.sb.state, "device_pair", json!({})).unwrap();
        assert!(made.ok, "{:?}", made.error);
        let body = json!({"code": made.result["code"], "device_name": name}).to_string();
        let reply = self.http("POST", "/v1/pair", None, Some(&body));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.body["token"].as_str().unwrap().to_string()
    }

    fn stream(&self, token: &str) -> Socket {
        let url = format!("wss://127.0.0.1:{}/v1/stream", self.addr.port());
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tungstenite::client(request, tls_stream(self.addr, self.pin)) {
            Ok((socket, _)) => socket,
            Err(HandshakeError::Failure(tungstenite::Error::Http(r))) => {
                panic!("the stream was refused: {}", r.status())
            }
            Err(other) => panic!("the upgrade failed: {other:?}"),
        }
    }

    fn api_ledger(&self) -> Vec<String> {
        lines(&self.s.live.sb.state.join("api.ledger"))
    }

    fn socket(&self, verb: &str, args: Value) -> Value {
        let r = pf_ipc::call(&self.s.live.sb.state, verb, args).unwrap();
        assert!(r.ok, "{verb}: {:?}", r.error);
        r.result
    }

    fn claim(&self, holder: &str) {
        let out = self.s.live.cli(&["claim", "--holder", holder]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }

    /// Drive the run to an open `merge_ack` on the spine PR (#9, `spine/s1` into `main`). Returns the
    /// request ID and the head SHA.
    fn to_merge_ack(&self) -> (String, String) {
        let s = &self.s;
        s.live
            .ports
            .fake
            .script("spine", Scenario::Script(vec![cost(7_000), Step::TurnEnd]));
        s.item("u1", 1, 1);
        s.finish_item("u1", 1);
        s.touch(&s.run_dir().join("rounds.done"));
        s.tick();
        assert!(s.has_unit("spine"));
        s.tick();
        s.forge
            .write_pr(9, SPINE_BRANCH, "main", PrChecks::Green)
            .unwrap();
        let head = s.head(SPINE_BRANCH);
        std::fs::create_dir_all(s.out("spine", "")).unwrap();
        std::fs::write(
            s.out("spine", "pr.url"),
            format!("file://{}#9\n", s.forge.repo()),
        )
        .unwrap();
        s.live.ports.fake.finish(&s.session_of("spine"));
        s.tick();
        let open = s.requests(RequestKind::MergeAck);
        assert_eq!(open.len(), 1, "{open:?}");
        (open[0].id.clone(), head)
    }
}

// ------------------------------------------------------------------ the stream

enum Got {
    Frame(Value),
    Close(u16),
    Idle,
}

fn read(socket: &mut Socket) -> Got {
    match socket.read() {
        Ok(Message::Text(text)) => Got::Frame(serde_json::from_str(text.as_str()).unwrap()),
        Ok(Message::Close(frame)) => Got::Close(frame.map_or(0, |f| u16::from(f.code))),
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
            Got::Close(0)
        }
        Err(e) => panic!("the stream failed: {e}"),
    }
}

fn types(frames: &[Value]) -> String {
    frames
        .iter()
        .map(|f| f["type"].as_str().unwrap_or("?").to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read frames until `done` says so; it fails with what it saw after 20 seconds.
fn collect(socket: &mut Socket, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let end = Instant::now() + Duration::from_secs(20);
    let mut frames = Vec::new();
    while !done(&frames) {
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}; frames: {}",
            types(&frames)
        );
        match read(socket) {
            Got::Frame(f) => frames.push(f),
            Got::Close(c) => panic!("closed {c} while waiting for {what}: {}", types(&frames)),
            Got::Idle => {}
        }
    }
    frames
}

/// Frames for a moment more, to show that none of a kind comes.
fn quiet(socket: &mut Socket, ms: u64) -> Vec<Value> {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut frames = Vec::new();
    while Instant::now() < end {
        if let Got::Frame(f) = read(socket) {
            frames.push(f);
        }
    }
    frames
}

fn is_request(f: &Value, id: &str, status: &str) -> bool {
    f["type"] == "request" && f["request"]["id"] == id && f["request"]["status"] == status
}

// ------------------------------------------------------------------ tests

#[test]
fn a_device_acks_the_spine_pr_with_a_confirm_code_and_the_stream_shows_each_change() {
    let rig = rig();
    let s = &rig.s;
    let d1 = rig.pair("MacBook");
    let _d2 = rig.pair("iPad");

    // The stream opens before the merge ack exists: `hello`, then the open requests and the claim.
    let mut socket = rig.stream(&d1);
    let first = collect(&mut socket, "hello and requests", |f| {
        f.iter().any(|x| x["type"] == "requests")
    });
    assert_eq!(first[0]["type"], "hello", "{}", types(&first));
    assert_eq!(
        first.last().unwrap()["type"],
        "requests",
        "{}",
        types(&first)
    );
    assert!(first.last().unwrap()["claim"].is_null());

    let (id, head) = rig.to_merge_ack();
    let frames = collect(&mut socket, "the merge_ack request frame", |f| {
        f.iter().any(|x| is_request(x, &id, "open"))
    });
    let frame = frames.iter().find(|x| is_request(x, &id, "open")).unwrap();
    assert_eq!(frame["request"]["kind"], "merge_ack");
    assert_eq!(frame["request"]["verbs"], json!(["ack"]));
    assert_eq!(frame["request"]["head_sha"], head.as_str());

    // `requests` and `show` over the API answer the socket's JSON.
    for args in [json!({}), json!({ "all": true })] {
        let over_api = rig.verb(&d1, "requests", args.clone());
        assert_eq!(over_api.status, 200, "{}", over_api.body);
        assert_eq!(over_api.body, rig.socket("requests", args));
    }
    let shown = rig.verb(&d1, "show", json!({ "id": id }));
    assert_eq!(shown.status, 200, "{}", shown.body);
    assert_eq!(shown.body, rig.socket("show", json!({ "id": id })));
    assert_eq!(shown.body["body"]["head_sha"], head.as_str());

    // The CLI claims (generation 1); the claim frame says so.
    rig.claim("ann@host");
    let frames = collect(&mut socket, "the claim frame", |f| {
        f.iter().any(|x| x["type"] == "claim")
    });
    let claim = frames.iter().find(|x| x["type"] == "claim").unwrap();
    assert_eq!(claim["claim"]["generation"], 1);
    assert_eq!(claim["claim"]["holder"], "ann@host");
    assert!(claim["claim"]["device"].is_null());

    // The code names the PR, the base, the head branch and the head SHA, binds generation 1, and moves
    // nothing.
    let main_before = s.head("main");
    let issued = rig.verb(
        &d1,
        "confirm_code",
        json!({ "subject": { "kind": "request", "request": id, "revision": 1, "verb": "ack" }, "command_id": "issue-1" }),
    );
    assert_eq!(issued.status, 200, "{}", issued.body);
    let subject = &issued.body["subject"];
    let repo = s.forge.repo().to_string();
    assert!(
        subject["pr"].as_str().unwrap().contains(&repo)
            && subject["pr"].as_str().unwrap().ends_with("#9"),
        "{subject}"
    );
    assert_eq!(subject["base"], "main");
    assert_eq!(subject["head_branch"], SPINE_BRANCH);
    assert_eq!(subject["head_sha"], head.as_str());
    assert_eq!(issued.body["generation"], 1);
    assert_eq!(issued.body["holder"]["holder"], "ann@host");
    let code_text = issued.body["code"].as_str().unwrap().to_string();
    let quiet_frames = quiet(&mut socket, 600);
    assert!(
        quiet_frames.iter().all(|f| f["type"] != "claim"),
        "the issue moved no claim: {}",
        types(&quiet_frames)
    );
    assert_eq!(s.live.controller.store().claims().unwrap().len(), 1);

    // The ack: the claim moves to D-1 (generation 2) and the PR merges in the same act.
    let ack_args = json!({ "request": id, "rev": 1, "code": code_text, "command_id": "ack-1" });
    let acked = rig.verb(&d1, "ack", ack_args.clone());
    assert_eq!(acked.status, 200, "{}", acked.body);
    assert_eq!(acked.body["merged"], true, "{}", acked.body);
    assert_eq!(acked.body["head_sha"], head.as_str());
    assert!(acked.body["pr"].as_str().unwrap().ends_with("#9"));
    assert_eq!(acked.body["base"], "main");
    assert_eq!(acked.body["generation"], 2);
    let main = s.head("main");
    assert_ne!(main, main_before);
    git(
        &s.live.bare,
        &["merge-base", "--is-ancestor", head.as_str(), main.as_str()],
    );
    // The CLI's own ack is refused, and names the device.
    let out = s.live.cli(&["ack", &id, "--rev", "1"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("the claim moved to device D-1"),
        "{}",
        stderr(&out)
    );

    // The stream: the claim moved to D-1, and the request is answered.
    let frames = collect(&mut socket, "the claim and the answered request", |f| {
        f.iter()
            .any(|x| x["type"] == "claim" && x["claim"]["device"] == "D-1")
            && f.iter().any(|x| is_request(x, &id, "answered"))
    });
    let moved = frames
        .iter()
        .find(|x| x["type"] == "claim" && x["claim"]["device"] == "D-1")
        .unwrap();
    assert_eq!(moved["claim"]["generation"], 2);

    // The ledgers.
    let run_ledger = s.ledger();
    assert!(
        run_ledger.contains(&format!("ack {id}@1 by claim 2: verb=ack"))
            && run_ledger.contains(&format!("sha={head}")),
        "{run_ledger}"
    );
    assert!(run_ledger.contains("outcome=landed"), "{run_ledger}");
    let controller = controller_ledger(&s.live).join("\n");
    assert!(
        controller.contains(&format!(
            "confirm_code D-1 request={id}@1 verb=ack generation=1"
        )),
        "{controller}"
    );
    assert!(
        controller.contains(&format!("pr={repo}#9")) || controller.contains("#9"),
        "{controller}"
    );
    assert!(controller.contains(&format!("head={head}")), "{controller}");
    assert!(
        controller.contains(&format!(
            "device_ack D-1 {id}@1 generation=2 from=1 admitted"
        )),
        "{controller}"
    );
    let api_ledger = rig.api_ledger().join("\n");
    assert!(
        api_ledger.contains("verb=confirm_code device=D-1")
            && api_ledger.contains("status=200 outcome=ok"),
        "{api_ledger}"
    );
    assert!(
        api_ledger
            .lines()
            .any(|l| l.contains("verb=ack device=D-1") && l.contains("status=200 outcome=ok")),
        "{api_ledger}"
    );

    // The same command ID again: a replay, and the forge merges nothing more.
    let again = rig.verb(&d1, "ack", ack_args);
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.body["replayed"], true);
    assert_eq!(s.head("main"), main, "main is unchanged");
    assert!(
        rig.api_ledger()
            .iter()
            .any(|l| l.contains("verb=ack device=D-1") && l.contains("outcome=replayed")),
        "{:?}",
        rig.api_ledger()
    );

    // No code, claim token or device token text anywhere.
    let token = s.token();
    let all = [
        rig.api_ledger().join("\n"),
        controller_ledger(&s.live).join("\n"),
        s.ledger(),
    ];
    for secret in [&code_text, &token, &d1] {
        for text in &all {
            assert!(
                !text.contains(secret.as_str()),
                "a secret leaked into a ledger"
            );
        }
    }
}

#[test]
fn the_refusals_are_one_api_ledger_line_each_and_leave_main_alone() {
    let rig = rig();
    let s = &rig.s;
    let d1 = rig.pair("MacBook");
    let d2 = rig.pair("iPad");
    let (id, head) = rig.to_merge_ack();
    rig.claim("ann@host");
    let main_before = s.head("main");
    let ack_subject = json!({ "kind": "request", "request": id, "revision": 1, "verb": "ack" });
    let issue = |token: &str, n: u32| {
        let r = rig.verb(
            token,
            "confirm_code",
            json!({ "subject": ack_subject, "command_id": format!("issue-{n}") }),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["code"].as_str().unwrap().to_string()
    };
    let ack = |token: &str, code: &str, n: u32| {
        rig.verb(
            token,
            "ack",
            json!({ "request": id, "rev": 1, "code": code, "command_id": format!("ack-{n}") }),
        )
    };
    let refused = |a: &Answer, word: &str| {
        assert_eq!(a.status, 422, "{}", a.body);
        assert_eq!(a.body["refusal"], word, "{}", a.body);
    };

    // Another device's code.
    let c1 = issue(&d1, 1);
    refused(&ack(&d2, &c1, 1), "code_foreign");

    // A CLI claim between the issue and the ack.
    let c2 = issue(&d1, 2);
    rig.claim("bob@host");
    refused(&ack(&d1, &c2, 2), "claim_moved");
    assert_eq!(s.head("main"), main_before);

    // A new commit on the spine branch after the code was issued.
    let c3 = issue(&d1, 3);
    git(&s.work, &["fetch", "--quiet", "origin"]);
    git(
        &s.work,
        &[
            "checkout",
            "--quiet",
            "-B",
            SPINE_BRANCH,
            &format!("origin/{SPINE_BRANCH}"),
        ],
    );
    std::fs::write(s.work.join("late.txt"), "late\n").unwrap();
    git(&s.work, &["add", "-A"]);
    git(&s.work, &["commit", "--quiet", "-m", "late"]);
    git(&s.work, &["push", "--quiet", "origin", SPINE_BRANCH]);
    refused(&ack(&d1, &c3, 3), "subject_moved");
    assert_eq!(s.head("main"), main_before, "main is unchanged");
    let request = s.live.controller.store().request(&id).unwrap().unwrap();
    assert_eq!(request.revision, 2, "the request is at revision 2");
    assert_eq!(request.status, RequestStatus::Open);

    // A replay of the issue answers the FIRST record: the head the code bound, not the head now.
    let replay = rig.verb(
        &d1,
        "confirm_code",
        json!({ "subject": ack_subject, "command_id": "issue-3" }),
    );
    assert_eq!(replay.status, 200, "{}", replay.body);
    assert_eq!(replay.body["replayed"], true);
    assert!(replay.body["code"].is_null(), "{}", replay.body);
    assert!(replay.body["expires_at"].is_string(), "{}", replay.body);
    assert!(replay.body["generation"].is_number(), "{}", replay.body);
    assert_eq!(replay.body["subject"]["revision"], 1);
    assert_eq!(replay.body["subject"]["head_sha"], head, "{}", replay.body);

    // Each refusal is exactly one api.ledger line.
    let ledger = rig.api_ledger();
    for word in ["code_foreign", "claim_moved", "subject_moved"] {
        let hits: Vec<&String> = ledger
            .iter()
            .filter(|l| {
                l.contains("verb=ack") && l.ends_with(&format!("outcome=refused refusal={word}"))
            })
            .collect();
        assert_eq!(hits.len(), 1, "{word}: {ledger:?}");
        assert!(hits[0].contains("device=D-"), "{}", hits[0]);
    }
    for secret in [&c1, &c2, &c3] {
        assert!(!ledger.join("\n").contains(secret.as_str()));
    }
}

#[test]
fn a_stop_rule_request_is_answered_from_a_device_and_the_stream_shows_it() {
    let rig = rig();
    let s = &rig.s;
    let d1 = rig.pair("MacBook");
    let mut socket = rig.stream(&d1);
    collect(&mut socket, "the first frames", |f| {
        f.iter().any(|x| x["type"] == "requests")
    });
    // The implement seat writes its stop file and ends its turn: a stop request opens.
    s.item("u1", 1, 1);
    s.tick();
    std::fs::create_dir_all(s.out("u1", "")).unwrap();
    std::fs::write(s.out("u1", "stop"), "Which base branch?\n").unwrap();
    s.live.ports.fake.finish(&s.session_of("u1"));
    s.tick();
    let open: Vec<_> = s
        .requests(RequestKind::Stop)
        .into_iter()
        .filter(|r| r.status == RequestStatus::Open)
        .collect();
    assert_eq!(open.len(), 1, "{open:?}");
    let id = open[0].id.clone();
    collect(&mut socket, "the stop request frame", |f| {
        f.iter().any(|x| is_request(x, &id, "open"))
    });

    let text = "Use main.";
    let sha = pf_engine::claim::sha256_hex(text.as_bytes());
    let issued = rig.verb(
        &d1,
        "confirm_code",
        json!({ "subject": { "kind": "request", "request": id, "revision": 1, "verb": "answer", "text_sha256": sha }, "command_id": "i1" }),
    );
    assert_eq!(issued.status, 200, "{}", issued.body);
    let answered = rig.verb(
        &d1,
        "answer",
        json!({ "request": id, "rev": 1, "text": text, "code": issued.body["code"], "command_id": "a1" }),
    );
    assert_eq!(answered.status, 200, "{}", answered.body);
    assert_eq!(answered.body["generation"], 1);
    collect(&mut socket, "the answered request frame", |f| {
        f.iter().any(|x| is_request(x, &id, "answered"))
    });
}

#[test]
fn a_revoked_device_is_refused_and_five_bad_tokens_block_the_peer() {
    let rig = rig();
    let d1 = rig.pair("MacBook");
    let d2 = rig.pair("iPad");
    let (id, _) = rig.to_merge_ack();
    let subject = json!({ "kind": "request", "request": id, "revision": 1, "verb": "ack" });
    let held = rig.verb(
        &d2,
        "confirm_code",
        json!({ "subject": subject, "command_id": "i-2" }),
    );
    assert_eq!(held.status, 200, "{}", held.body);
    let out = rig.s.live.cli(&["device", "revoke", "D-2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let next = rig.verb(
        &d2,
        "confirm_code",
        json!({ "subject": subject, "command_id": "i-3" }),
    );
    assert_eq!(next.status, 401, "{}", next.body);
    let spend = rig.verb(
        &d2,
        "ack",
        json!({ "request": id, "rev": 1, "code": held.body["code"], "command_id": "a-2" }),
    );
    assert_eq!(
        spend.status, 401,
        "a code the device held is dead: {}",
        spend.body
    );
    assert_eq!(
        rig.s.requests(RequestKind::MergeAck)[0].status,
        RequestStatus::Open
    );
    let _ = d1;
}

#[test]
fn five_bad_tokens_on_confirm_code_block_the_peer() {
    let rig = rig();
    let d1 = rig.pair("MacBook");
    let (id, _) = rig.to_merge_ack();
    let subject = json!({ "kind": "request", "request": id, "revision": 1, "verb": "ack" });
    // Five bad tokens on one route from one peer, and the sixth call, even with a good token, is 429.
    let bad = "ab".repeat(32);
    for n in 0..5 {
        let r = rig.verb(
            &bad,
            "confirm_code",
            json!({ "subject": subject, "command_id": format!("b-{n}") }),
        );
        assert_eq!(r.status, 401, "{}", r.body);
    }
    let blocked = rig.verb(
        &d1,
        "confirm_code",
        json!({ "subject": subject, "command_id": "i-9" }),
    );
    assert_eq!(blocked.status, 429, "{}", blocked.body);
}
