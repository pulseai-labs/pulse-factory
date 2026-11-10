//! AC-1 of `r1.s4.w3`: the client half of w1's request frames and verbs, W3's refusal word and W6's
//! long wait for `ack`. The host is the real TLS listener and router of `pf-api` on `127.0.0.1:0`
//! over a fake host, as `tests/stream.rs` builds it; this file's host also records each verb call,
//! answers each verb with a canned answer, and can hold a verb for a while. Nothing leaves the
//! loopback address.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_api::code::{hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::host::{StreamEnd, StreamEvent as Feed, StreamRefusal};
use pf_api::types::{
    ApiStatus, ConfirmCodeArgs, ConfirmSubjectArg, ConfirmVerb, DeviceAckArgs, DeviceAnswerArgs,
    DeviceClearArgs, DeviceRecord, PairResult, RequestFrame, RequestsArgs, RunListArgs, ServerRef,
    SettingValue, SettingsGetArgs, SettingsSetArgs, SettingsUnsetArgs, ShowArgs,
};
use pf_api::{ApiHost, Refusal, RequestRecord, router_with, tls};
use pf_client::error::{ClientError, ErrorKind, Route, error_from_status};
use pf_client::http::Timeouts;
use pf_client::store::{DeviceStore, FileStore, Paired};
use pf_client::stream::{ChatStream, StreamEvent, StreamOptions, StreamTuning};
use pf_client::transport::Transport;
use serde_json::{Value, json};
use tokio::sync::mpsc;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const CODE: &str = "pfc1-0123456789abcdef0123456789abcdef";

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
    delays: Mutex<HashMap<String, Duration>>,
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
        let delay = self.delays.lock().unwrap().get(verb).copied();
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
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
    fn opens(&self) -> Vec<(Option<String>, u64)> {
        self.opens.lock().unwrap().clone()
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }

    fn answer(&self, verb: &str, response: pf_ipc::Response) {
        self.answers
            .lock()
            .unwrap()
            .insert(verb.to_string(), response);
    }

    fn delay(&self, verb: &str, by: Duration) {
        self.delays.lock().unwrap().insert(verb.to_string(), by);
    }

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
            delays: Mutex::new(HashMap::new()),
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

fn client_with(address: &str, pin: &[u8; 32], timeouts: Timeouts) -> Client {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    store
        .save(&Paired {
            address: address.to_string(),
            pin: hex(pin),
            token: TOKEN.to_string(),
            device: device(),
        })
        .unwrap();
    let transport = Transport::new(store)
        .with_timeouts(timeouts)
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

fn client(server: &Server) -> Client {
    client_with(
        &server.addr.to_string(),
        &server.pin,
        Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(5),
        },
    )
}

// ---- the stream ---------------------------------------------------------------------------------

async fn next(stream: &mut ChatStream) -> StreamEvent {
    match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("the stream ended with no event"),
        Err(_) => panic!("no event within 5 s"),
    }
}

/// The next event as JSON.
async fn next_json(stream: &mut ChatStream) -> Value {
    serde_json::to_value(next(stream).await).unwrap()
}

fn frame(value: Value) -> Feed {
    Feed::Frame(value.to_string())
}

fn summary(id: &str, revision: u32, status: &str) -> Value {
    json!({
        "id": id, "revision": revision, "kind": "stop", "status": status, "run": "R-1",
        "unit": null, "stage": "implement", "blocks": [], "opened_at": "2026-11-10T10:00:00.000Z",
        "answered_at": null, "reason": "the implementer asks a question", "verbs": ["answer"]
    })
}

fn claim(generation: u64) -> Value {
    json!({"generation": generation, "holder": "device D-1", "device": "D-1", "revoked": false,
           "claimed_at": "2026-11-10T10:05:00.000Z"})
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
async fn request_frames_arrive_as_their_own_event_in_order_between_chat_frames() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options(0));
    let tx = server.fake.wait_feed(0).await;
    let requests =
        json!({"type": "requests", "requests": [summary("R-1", 1, "open")], "claim": null});
    let one = json!({"type": "request", "request": summary("R-1", 2, "answered")});
    let moved = json!({"type": "claim", "claim": claim(3)});
    tx.send(frame(item(1))).await.unwrap();
    tx.send(frame(requests.clone())).await.unwrap();
    // A type this client does not know is still skipped.
    tx.send(frame(json!({"type": "future_thing", "n": 1})))
        .await
        .unwrap();
    tx.send(frame(one.clone())).await.unwrap();
    tx.send(frame(item(2))).await.unwrap();
    tx.send(frame(moved.clone())).await.unwrap();
    tx.send(frame(json!({"type": "claim", "claim": null})))
        .await
        .unwrap();

    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 1);
    // The JSON of the event is `{"event":"request_frame","frame":{…}}`, the frame as the server sent it.
    assert_eq!(
        next_json(&mut stream).await,
        json!({"event": "request_frame", "frame": requests})
    );
    assert_eq!(
        next_json(&mut stream).await,
        json!({"event": "request_frame", "frame": one})
    );
    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 2);
    // The typed side holds the same frame.
    match next(&mut stream).await {
        StreamEvent::RequestFrame {
            frame: RequestFrame::Claim { claim },
        } => assert_eq!(claim.map(|c| c.generation), Some(3)),
        other => panic!("wanted the claim frame, got {other:?}"),
    }
    assert_eq!(
        next_json(&mut stream).await,
        json!({"event": "request_frame", "frame": {"type": "claim", "claim": null}})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_poisoned_request_frame_is_dropped_and_the_stream_and_its_chat_go_on() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options(0));
    let tx = server.fake.wait_feed(0).await;
    tx.send(frame(item(1))).await.unwrap();
    tx.send(frame(json!({"type": "request", "request": "oops"})))
        .await
        .unwrap();
    tx.send(frame(json!({"type": "claim", "claim": "bad"})))
        .await
        .unwrap();
    tx.send(frame(
        json!({"type": "requests", "requests": 3, "claim": null}),
    ))
    .await
    .unwrap();
    tx.send(frame(item(2))).await.unwrap();
    tx.send(frame(
        json!({"type": "request", "request": summary("R-2", 1, "open")}),
    ))
    .await
    .unwrap();
    tx.send(frame(item(3))).await.unwrap();

    // The chat frames before and after the dropped ones arrive in order, with no `closed` and no
    // `reconnecting` between them.
    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 1);
    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 2);
    let good = next_json(&mut stream).await;
    assert_eq!(good["event"], "request_frame");
    assert_eq!(good["frame"]["request"]["id"], "R-2");
    assert_eq!(next_json(&mut stream).await["frame"]["item"]["seq"], 3);
    assert_eq!(server.fake.opens().len(), 1, "the host saw no new open");

    // The reconnect's `after` is the last item `seq`: request frames do not move it.
    tx.send(Feed::End(StreamEnd::Shutdown)).await.unwrap();
    match next(&mut stream).await {
        StreamEvent::Reconnecting { after, .. } => assert_eq!(after, 3),
        other => panic!("wanted reconnecting, got {other:?}"),
    }
    let _again = server.fake.wait_feed(1).await;
    assert_eq!(
        server.fake.opens(),
        vec![(Some("S-1".to_string()), 0), (Some("S-1".to_string()), 3)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_known_chat_frame_still_closes_the_stream_as_server() {
    let server = Server::start().await;
    let c = client(&server);
    let mut stream = c.transport.open_stream(options(0));
    let tx = server.fake.wait_feed(0).await;
    tx.send(frame(json!({"type": "request", "request": "oops"})))
        .await
        .unwrap();
    tx.send(frame(json!({"type": "item", "item": "oops"})))
        .await
        .unwrap();
    match next(&mut stream).await {
        StreamEvent::Closed { error } => {
            assert_eq!(error.kind, ErrorKind::Server);
            assert_eq!(
                error.message,
                "the server sent a frame this client cannot read"
            );
        }
        other => panic!("wanted closed, got {other:?}"),
    }
}

// ---- the typed methods --------------------------------------------------------------------------

fn ok(value: Value) -> pf_ipc::Response {
    pf_ipc::Response::ok(value)
}

fn setting(value: Value, from: &str) -> Value {
    json!({"key": "chat.self_turn_cap", "value": value,
           "from": {"kind": from, "project_id": null, "agent_id": null},
           "built_in": 3, "merge": "override", "applies": "next_turn", "layers": [], "masked": []})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_typed_method_posts_its_verb_with_its_arguments_and_parses_the_answer() {
    let server = Server::start().await;
    let c = client(&server);
    let t = &c.transport;
    let acted = |verb: &str| {
        json!({"schema_version": 1, "replayed": false, "verb": verb, "device": "D-1",
               "generation": 2, "request": summary("R-1", 2, "answered"), "merged": null,
               "head_sha": null, "pr": null, "base": null, "outcome": null, "resumed": true,
               "hook": "none", "to_stage": null})
    };
    let canned: Vec<(&str, Value)> = vec![
        (
            "requests",
            json!({"schema_version": 1, "open": 1, "requests": [summary("R-1", 1, "open")], "claim": null}),
        ),
        ("show", {
            let mut detail = summary("R-1", 1, "open");
            detail["schema_version"] = json!(1);
            detail["body"] = json!({"stop_text": "which branch?"});
            detail["answer"] = json!(null);
            detail["answer_generation"] = json!(null);
            detail
        }),
        (
            "confirm_code",
            json!({"schema_version": 1, "replayed": false, "code": CODE,
                   "expires_at": "2026-11-10T10:07:00.000Z", "device": "D-1", "generation": null,
                   "holder": null,
                   "subject": {"request": "R-1", "revision": 1, "kind": "stop", "run": "R-1",
                               "unit": null, "reason": "why"}}),
        ),
        ("answer", acted("answer")),
        ("clear", acted("clear")),
        ("ack", acted("ack")),
        (
            "settings_get",
            json!({"schema_version": 1, "project_id": null, "agent_id": null,
                   "settings": [setting(json!(3), "built_in")]}),
        ),
        (
            "settings_set",
            json!({"schema_version": 1, "replayed": false, "key": "chat.self_turn_cap",
                   "scope": {"kind": "project", "project_id": "P-1", "agent_id": null},
                   "value": 5, "resolved": setting(json!(5), "project")}),
        ),
        (
            "settings_unset",
            json!({"schema_version": 1, "replayed": false, "key": "chat.self_turn_cap",
                   "scope": {"kind": "project", "project_id": "P-1", "agent_id": null},
                   "value": null, "resolved": setting(json!(3), "built_in")}),
        ),
        (
            "run_list",
            json!({"schema_version": 1, "runs": [{
                "id": "R-1", "spine": "s", "workflow": "w", "state": "active",
                "halted_reason": null, "end_reason": null, "open_requests": 1,
                "waiting": "the spine PR is merged: close it"}]}),
        ),
    ];
    for (verb, answer) in &canned {
        server.fake.answer(verb, ok(answer.clone()));
    }
    let args = |verb: &str| -> Value {
        match verb {
            "requests" => json!({"run": "R-1", "all": true}),
            "show" => json!({"id": "R-1"}),
            "confirm_code" => {
                json!({"subject": {"kind": "request", "request": "R-1", "revision": 1,
                "verb": "answer", "text_sha256": "a".repeat(64)}, "command_id": "c-1"})
            }
            "answer" => {
                json!({"request": "R-1", "rev": 1, "text": "main", "code": CODE, "command_id": "c-2"})
            }
            "clear" => {
                json!({"request": "R-1", "rev": 1, "to": "implement", "code": CODE, "command_id": "c-3"})
            }
            "ack" => json!({"request": "R-9", "rev": 1, "code": CODE, "command_id": "c-4"}),
            "settings_get" => json!({"key": "chat.self_turn_cap", "project_id": "P-1"}),
            "settings_set" => {
                json!({"key": "chat.self_turn_cap", "value": 5, "project_id": "P-1", "command_id": "c-5"})
            }
            "settings_unset" => {
                json!({"key": "chat.self_turn_cap", "project_id": "P-1", "command_id": "c-6"})
            }
            _ => json!({}),
        }
    };

    let got: Vec<(&str, Value)> = vec![
        (
            "requests",
            serde_json::to_value(
                t.requests(RequestsArgs {
                    run: Some("R-1".into()),
                    all: Some(true),
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "show",
            serde_json::to_value(t.show(ShowArgs { id: "R-1".into() }).await.unwrap()).unwrap(),
        ),
        (
            "confirm_code",
            serde_json::to_value(
                t.confirm_code(ConfirmCodeArgs {
                    subject: ConfirmSubjectArg::Request {
                        request: "R-1".into(),
                        revision: 1,
                        verb: ConfirmVerb::Answer,
                        text_sha256: Some("a".repeat(64)),
                        to: None,
                    },
                    command_id: "c-1".into(),
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "answer",
            serde_json::to_value(
                t.answer(DeviceAnswerArgs {
                    request: "R-1".into(),
                    rev: 1,
                    text: "main".into(),
                    code: CODE.into(),
                    command_id: "c-2".into(),
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "clear",
            serde_json::to_value(
                t.clear(DeviceClearArgs {
                    request: "R-1".into(),
                    rev: 1,
                    to: Some("implement".into()),
                    code: CODE.into(),
                    command_id: "c-3".into(),
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "ack",
            serde_json::to_value(
                t.ack(DeviceAckArgs {
                    request: "R-9".into(),
                    rev: 1,
                    code: CODE.into(),
                    command_id: "c-4".into(),
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "settings_get",
            serde_json::to_value(
                t.settings_get(SettingsGetArgs {
                    key: Some("chat.self_turn_cap".into()),
                    project_id: Some("P-1".into()),
                    agent_id: None,
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "settings_set",
            serde_json::to_value(
                t.settings_set(SettingsSetArgs {
                    key: "chat.self_turn_cap".into(),
                    value: SettingValue::Number(5),
                    project_id: Some("P-1".into()),
                    agent_id: None,
                    command_id: "c-5".into(),
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "settings_unset",
            serde_json::to_value(
                t.settings_unset(SettingsUnsetArgs {
                    key: "chat.self_turn_cap".into(),
                    project_id: Some("P-1".into()),
                    agent_id: None,
                    command_id: "c-6".into(),
                })
                .await
                .unwrap(),
            )
            .unwrap(),
        ),
        (
            "run_list",
            serde_json::to_value(t.run_list(RunListArgs {}).await.unwrap()).unwrap(),
        ),
    ];

    let calls = server.fake.calls();
    assert_eq!(
        calls.len(),
        canned.len(),
        "one call for each method: {calls:?}"
    );
    for (i, (verb, answer)) in canned.iter().enumerate() {
        assert_eq!(calls[i].0, *verb, "the verb of call {i}");
        let want = args(verb);
        // A `None` argument is a null on the wire or no field: both mean none.
        let mut sent = calls[i].1.clone();
        if let Some(object) = sent.as_object_mut() {
            object.retain(|_, v| !v.is_null());
        }
        assert_eq!(sent, want, "the arguments of {verb}");
        assert_eq!(got[i].0, *verb);
        assert_eq!(&got[i].1, answer, "the parsed answer of {verb}");
    }
}

// ---- the refusal word ---------------------------------------------------------------------------

fn refused_with(word: Option<&str>, text: &str) -> pf_ipc::Response {
    let mut r = pf_ipc::Response::err(text);
    if let Some(word) = word {
        r.result = json!({ "refusal": word });
    }
    r
}

fn ack_args() -> DeviceAckArgs {
    DeviceAckArgs {
        request: "R-9".into(),
        rev: 1,
        code: CODE.into(),
        command_id: "c-4".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_word_reaches_the_error_and_only_a_word_of_the_closed_list() {
    let server = Server::start().await;
    let c = client(&server);
    server
        .fake
        .answer("ack", refused_with(Some("code_used"), "that code was used"));
    let e = c.transport.ack(ack_args()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Refused);
    assert_eq!(e.refusal.as_deref(), Some("code_used"));
    assert_eq!(e.message, "that code was used");

    // A 422 with no word (an engine refusal after the admission) has none.
    server
        .fake
        .answer("ack", refused_with(None, "checks are red"));
    let e = c.transport.ack(ack_args()).await.unwrap_err();
    assert_eq!(
        (e.kind, e.refusal, e.message.as_str()),
        (ErrorKind::Refused, None, "checks are red")
    );

    // The client keeps a word only when it is on `pf_api::host::REFUSALS`, whatever the body says.
    for word in pf_api::host::REFUSALS {
        let body = json!({"error": "no", "refusal": word}).to_string();
        let e = error_from_status(422, Route::Token, body.as_bytes(), &[]);
        assert_eq!(e.refusal.as_deref(), Some(*word));
    }
    let body = json!({"error": "no", "refusal": "made_up_word"}).to_string();
    let e = error_from_status(422, Route::Token, body.as_bytes(), &[]);
    assert_eq!(e.refusal, None);
    let body = json!({"error": "no", "refusal": 7}).to_string();
    assert_eq!(
        error_from_status(422, Route::Token, body.as_bytes(), &[]).refusal,
        None
    );
    // Only a 422 carries one.
    let body = json!({"error": "no", "refusal": "code_used"}).to_string();
    assert_eq!(
        error_from_status(403, Route::Token, body.as_bytes(), &[]).refusal,
        None
    );
}

#[test]
fn the_json_of_an_error_with_no_refusal_is_exactly_kind_and_message() {
    let e = ClientError::new(ErrorKind::Refused, "no");
    assert_eq!(e.refusal, None);
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        r#"{"kind":"refused","message":"no"}"#
    );
    let mut with = e.clone();
    with.refusal = Some("code_used".to_string());
    assert_eq!(
        serde_json::to_value(&with).unwrap(),
        json!({"kind": "refused", "message": "no", "refusal": "code_used"})
    );
    // An error written without the field reads back as one with none.
    let back: ClientError = serde_json::from_str(r#"{"kind":"refused","message":"no"}"#).unwrap();
    assert_eq!(back, e);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_text_that_holds_the_code_of_the_call_is_replaced() {
    let server = Server::start().await;
    let c = client(&server);
    server.fake.answer(
        "ack",
        refused_with(Some("code_unknown"), &format!("no such code {CODE}")),
    );
    let e = c.transport.ack(ack_args()).await.unwrap_err();
    assert_eq!(e.message, "the server answered 422");
    assert_eq!(e.refusal.as_deref(), Some("code_unknown"));
    assert!(!format!("{e:?}").contains(CODE));
    // The generic `verb` carries the same rule.
    server.fake.answer(
        "answer",
        refused_with(None, &format!("the code {CODE} is no good")),
    );
    let e = c
        .transport
        .verb(
            "answer",
            json!({"request": "R-1", "rev": 1, "text": "x", "code": CODE, "command_id": "c"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e.message, "the server answered 422");
}

// ---- the wait of `ack` ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ack_waits_for_the_long_timeout_and_confirm_code_does_not() {
    let server = Server::start().await;
    let slow = Duration::from_millis(900);
    server.fake.delay("ack", slow);
    server.fake.delay("confirm_code", slow);
    server.fake.answer(
        "ack",
        ok(
            json!({"schema_version": 1, "replayed": false, "verb": "ack", "device": "D-1",
                  "generation": 1, "request": null, "merged": true, "head_sha": "abc",
                  "pr": "o/r#1", "base": "main", "outcome": "landed", "resumed": null,
                  "hook": null, "to_stage": null}),
        ),
    );
    let c = client_with(
        &server.addr.to_string(),
        &server.pin,
        Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_millis(400),
        },
    );
    let transport = c
        .transport
        .clone()
        .with_chat_open_timeout(Duration::from_secs(5));
    let acted = transport
        .ack(ack_args())
        .await
        .expect("ack waits the long timeout");
    assert_eq!(acted.merged, Some(true));

    let e = transport
        .confirm_code(ConfirmCodeArgs {
            subject: ConfirmSubjectArg::Request {
                request: "R-9".into(),
                revision: 1,
                verb: ConfirmVerb::Ack,
                text_sha256: None,
                to: None,
            },
            command_id: "c-7".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unreachable);
}

// ---- the pin ------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_key_on_confirm_code_is_key_changed_and_the_host_saw_no_call() {
    let server = Server::start().await;
    let c = client_with(
        &server.addr.to_string(),
        &[7u8; 32],
        Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(5),
        },
    );
    let e = c
        .transport
        .confirm_code(ConfirmCodeArgs {
            subject: ConfirmSubjectArg::Request {
                request: "R-9".into(),
                revision: 1,
                verb: ConfirmVerb::Ack,
                text_sha256: None,
                to: None,
            },
            command_id: "c-8".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::KeyChanged);
    assert!(server.fake.calls().is_empty(), "no call reached the host");
}
