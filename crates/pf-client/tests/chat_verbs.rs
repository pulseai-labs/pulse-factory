//! AC-2 of `r1.s3.w4`: the seven chat verbs as typed methods of `Transport`, and the server memory
//! (`pf-server-id` and `pf-server-name` on every answer). The host is the real TLS listener and router
//! of `pf-api` over a fake host whose `call` records the verb and its arguments and answers fixed
//! JSON, the controller's `ChatSessionStarted` and `ChatSessionEnded` shapes included.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pf_api::code::{PairingCode, hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::types::{
    ApiStatus, ChatAnswerArg, DeviceRecord, PairResult, ServerRef, SessionAnswerArgs,
    SessionEndArgs, SessionInterruptArgs, SessionListArgs, SessionSendArgs, SessionSetArgs,
    SessionStartArgs,
};
use pf_api::{ApiHost, Refusal, router_with, tls};
use pf_client::error::ErrorKind;
use pf_client::http::Timeouts;
use pf_client::store::{DeviceStore, FileStore, Paired};
use pf_client::transport::Transport;
use serde_json::{Value, json};

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
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

/// A session record with every field of `ChatSessionRecord`.
fn session_record() -> Value {
    json!({
        "id": "S-1", "project_id": "P-1", "project_name": "alpha",
        "worktree_id": "W-1", "worktree_dir": "/work/alpha", "branch": "main", "detached": false,
        "agent_id": "orchestrator", "runtime": "claude", "runtime_session_ref": null,
        "state": "idle", "context_lost": null, "process": null,
        "model": "opus", "effort": "high", "permission_mode": "default",
        "runtime_options": {}, "pending_options": {},
        "usage": {"input_tokens": 1, "output_tokens": 2, "cache_read_tokens": 3,
                  "cache_write_tokens": 4, "cost_micros": 5},
        "last_turn_at": null, "cache_ttl_seconds": null, "context": null,
        "created_at": "2026-11-09T10:00:00.000Z", "updated_at": "2026-11-09T10:00:00.000Z",
        "ended_at": null
    })
}

struct Fake {
    spki_hex: String,
    /// The verb and the arguments of each call.
    calls: Mutex<Vec<(String, Value)>>,
    /// How long `call` and `status` take, in milliseconds.
    delay_ms: AtomicU64,
    /// The answer of a verb is a refusal.
    refuse: AtomicBool,
    /// `server()` names an id that is not the pin. The router reads it once, when it is built.
    foreign_id: bool,
    /// The answers by verb, replaced by a test to send a malformed one.
    answers: Mutex<Vec<(String, Value)>>,
}

impl Fake {
    fn answer(&self, verb: &str) -> Value {
        if let Some((_, v)) = self.answers.lock().unwrap().iter().find(|(k, _)| k == verb) {
            return v.clone();
        }
        match verb {
            "session_list" => json!({
                "schema_version": 1,
                "server": {"id": self.spki_hex, "name": SERVER_NAME},
                "runtimes": [], "agents": [], "sessions": [session_record()]
            }),
            "session_start" => json!({
                "replayed": false,
                "session": {"id": "S-1", "project_id": "P-1", "state": "idle"},
                "worktree": {"id": "W-1", "path": "/work/alpha", "kind": "root"},
                "note": "the chat could not open: no runtime"
            }),
            "session_end" => json!({
                "replayed": true,
                "session": {"id": "S-1", "state": "ended"}
            }),
            _ => json!({
                "schema_version": 1, "replayed": false,
                "session": session_record(), "note": null
            }),
        }
    }
}

impl ApiHost for Fake {
    fn redeem(&self, _: &str, _: &str, _: IpAddr) -> Result<PairResult, Refusal> {
        Ok(PairResult {
            schema_version: 1,
            device: device(),
            token: TOKEN.to_string(),
        })
    }

    fn device(&self, token_sha256_hex: &str, _peer: IpAddr) -> Option<DeviceRecord> {
        (Some(token_sha256_hex.to_string()) == token_sha256(TOKEN)).then(device)
    }

    fn call(&self, _: &DeviceRecord, verb: &str, args: Value) -> pf_ipc::Response {
        std::thread::sleep(Duration::from_millis(self.delay_ms.load(Ordering::SeqCst)));
        self.calls.lock().unwrap().push((verb.to_string(), args));
        if self.refuse.load(Ordering::SeqCst) {
            return pf_ipc::Response::err("the project has no root");
        }
        pf_ipc::Response::ok(self.answer(verb))
    }

    fn status(&self) -> ApiStatus {
        std::thread::sleep(Duration::from_millis(self.delay_ms.load(Ordering::SeqCst)));
        ApiStatus {
            schema_version: 1,
            version: "0.1.0".to_string(),
            key_spki_sha256: self.spki_hex.clone(),
            device: None,
            server: Some(self.server()),
        }
    }

    fn server(&self) -> ServerRef {
        let id = if self.foreign_id {
            "f".repeat(64)
        } else {
            self.spki_hex.clone()
        };
        ServerRef {
            id: Some(id),
            name: SERVER_NAME.to_string(),
        }
    }
}

struct Rig {
    fake: Arc<Fake>,
    addr: SocketAddr,
    pin: [u8; 32],
    store: Arc<FileStore>,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Rig {
    async fn start() -> Rig {
        Rig::start_with(false).await
    }

    async fn start_with(foreign_id: bool) -> Rig {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_dir = tempfile::tempdir().unwrap();
        let key = tls::load_or_create(&tls::api_dir(server_dir.path()), &addr).unwrap();
        let fake = Arc::new(Fake {
            spki_hex: key.spki_sha256_hex(),
            calls: Mutex::new(Vec::new()),
            delay_ms: AtomicU64::new(0),
            refuse: AtomicBool::new(false),
            foreign_id,
            answers: Mutex::new(Vec::new()),
        });
        let app = router_with(fake.clone(), GuardConfig::default(), system_clock());
        let config = key.server_config().unwrap();
        tokio::spawn(async move {
            pf_api::listener::serve(listener, config, app, std::future::pending()).await;
        });
        let client_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::new(client_dir.path().join("device.json")));
        Rig {
            fake,
            addr,
            pin: key.spki_sha256(),
            store,
            _dirs: (server_dir, client_dir),
        }
    }

    /// Pair the client store by hand, with no network call.
    fn paired(&self) {
        self.store
            .save(&Paired {
                address: self.addr.to_string(),
                pin: hex(&self.pin),
                token: TOKEN.to_string(),
                device: device(),
            })
            .unwrap();
    }

    fn transport(&self) -> Transport<FileStore> {
        Transport::new(self.store.clone()).with_timeouts(Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(5),
        })
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.fake.calls.lock().unwrap().clone()
    }

    fn code(&self) -> String {
        PairingCode {
            secret: [7; 10],
            spki_sha256: self.pin,
            listen: self.addr.to_string(),
        }
        .encode()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_method_posts_its_verb_with_its_arguments_and_parses_its_answer() {
    let rig = Rig::start().await;
    rig.paired();
    let t = rig.transport();

    let list_args = SessionListArgs {
        project_id: Some("P-1".to_string()),
        all: None,
    };
    let list = t.session_list(list_args.clone()).await.unwrap();
    assert_eq!(list.server.name, SERVER_NAME);
    assert_eq!(list.sessions[0].id, "S-1");
    assert_eq!(list.sessions[0].worktree_dir, "/work/alpha");

    let start_args = SessionStartArgs {
        project_id: "P-1".to_string(),
        agent_id: "orchestrator".to_string(),
        command_id: "c-start".to_string(),
    };
    let started = t.session_start(start_args.clone()).await.unwrap();
    assert_eq!(started.session_id, "S-1");
    assert_eq!(started.worktree_id, "W-1");
    assert_eq!(started.worktree_dir, "/work/alpha", "from worktree.path");
    assert!(!started.replayed);
    assert_eq!(
        started.note.as_deref(),
        Some("the chat could not open: no runtime")
    );
    // The transport type's JSON is what the UI reads.
    assert_eq!(
        serde_json::to_value(&started).unwrap(),
        json!({"replayed": false, "session_id": "S-1", "worktree_id": "W-1",
               "worktree_dir": "/work/alpha", "note": "the chat could not open: no runtime"})
    );

    let send_args = SessionSendArgs {
        session_id: "S-1".to_string(),
        text: "hello".to_string(),
        command_id: "c-send".to_string(),
    };
    assert_eq!(
        t.session_send(send_args.clone()).await.unwrap().session.id,
        "S-1"
    );

    let answer_args = SessionAnswerArgs {
        session_id: "S-1".to_string(),
        seq: 7,
        answer: ChatAnswerArg::Deny {
            message: "no".to_string(),
        },
        command_id: "c-answer".to_string(),
    };
    assert_eq!(
        t.session_answer(answer_args.clone())
            .await
            .unwrap()
            .session
            .id,
        "S-1"
    );

    let interrupt_args = SessionInterruptArgs {
        session_id: "S-1".to_string(),
        command_id: "c-int".to_string(),
    };
    assert!(t.session_interrupt(interrupt_args.clone()).await.is_ok());

    let set_args = SessionSetArgs {
        session_id: "S-1".to_string(),
        model: None,
        effort: Some("medium".to_string()),
        permission_mode: None,
        options: None,
        command_id: "c-set".to_string(),
    };
    assert!(t.session_set(set_args.clone()).await.is_ok());

    let end_args = SessionEndArgs {
        session_id: "S-1".to_string(),
        command_id: "c-end".to_string(),
    };
    let ended = t.session_end(end_args.clone()).await.unwrap();
    assert_eq!(ended.session_id, "S-1");
    assert!(ended.replayed);
    assert_eq!(ended.note, None);
    assert_eq!(
        serde_json::to_value(&ended).unwrap(),
        json!({"replayed": true, "session_id": "S-1", "note": null})
    );

    let own = |args: &dyn erased::Json| args.json();
    let expected: Vec<(&str, Value)> = vec![
        ("session_list", own(&list_args)),
        ("session_start", own(&start_args)),
        ("session_send", own(&send_args)),
        ("session_answer", own(&answer_args)),
        ("session_interrupt", own(&interrupt_args)),
        ("session_set", own(&set_args)),
        ("session_end", own(&end_args)),
    ];
    let seen = rig.calls();
    assert_eq!(seen.len(), 7, "{seen:?}");
    for ((verb, args), (want_verb, want_args)) in seen.iter().zip(expected) {
        assert_eq!(verb, want_verb);
        assert_eq!(args, &want_args, "the arguments of {verb}");
    }
}

/// The JSON of an argument type, so the loop above can hold seven types in one list.
mod erased {
    use serde::Serialize;
    use serde_json::Value;

    pub trait Json {
        fn json(&self) -> Value;
    }

    impl<T: Serialize> Json for T {
        fn json(&self) -> Value {
            serde_json::to_value(self).unwrap()
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_without_the_documented_fields_is_a_server_error_and_a_422_is_refused() {
    let rig = Rig::start().await;
    rig.paired();
    let t = rig.transport();
    let start = || SessionStartArgs {
        project_id: "P-1".to_string(),
        agent_id: "orchestrator".to_string(),
        command_id: "c".to_string(),
    };
    let end = || SessionEndArgs {
        session_id: "S-1".to_string(),
        command_id: "c".to_string(),
    };
    // No `worktree` in a start answer, no `session` in an end answer.
    {
        let mut answers = rig.fake.answers.lock().unwrap();
        answers.push((
            "session_start".to_string(),
            json!({"replayed": false, "session": {"id": "S-1"}}),
        ));
        answers.push(("session_end".to_string(), json!({"replayed": false})));
        answers.push(("session_send".to_string(), json!({"schema_version": 1})));
    }
    for error in [
        t.session_start(start()).await.unwrap_err(),
        t.session_end(end()).await.unwrap_err(),
        t.session_send(SessionSendArgs {
            session_id: "S-1".to_string(),
            text: "x".to_string(),
            command_id: "c".to_string(),
        })
        .await
        .unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Server);
        assert_eq!(
            error.message,
            "the server's answer is not the documented one"
        );
    }
    // A 422 keeps the server's text.
    rig.fake.refuse.store(true, Ordering::SeqCst);
    let error = t.session_start(start()).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Refused);
    assert_eq!(error.message, "the project has no root");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chat_verbs_that_open_a_process_wait_longer_than_the_others() {
    let rig = Rig::start().await;
    rig.paired();
    let t = rig
        .transport()
        .with_timeouts(Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_millis(200),
        })
        .with_chat_open_timeout(Duration::from_millis(600));
    rig.fake.delay_ms.store(400, Ordering::SeqCst);
    // The request timeout is 200 ms: `status` and `session_list` time out as `unreachable`.
    let status = t.status().await.unwrap_err();
    assert_eq!(status.kind, ErrorKind::Unreachable);
    let list = t
        .session_list(SessionListArgs::default())
        .await
        .unwrap_err();
    assert_eq!(list.kind, ErrorKind::Unreachable);
    // The two verbs that may open a chat process have 600 ms, and 400 ms is inside it.
    let started = t
        .session_start(SessionStartArgs {
            project_id: "P-1".to_string(),
            agent_id: "orchestrator".to_string(),
            command_id: "c".to_string(),
        })
        .await;
    assert!(started.is_ok(), "{started:?}");
    let sent = t
        .session_send(SessionSendArgs {
            session_id: "S-1".to_string(),
            text: "x".to_string(),
            command_id: "c".to_string(),
        })
        .await;
    assert!(sent.is_ok(), "{sent:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_an_answer_names_lands_in_paired_and_forget_clears_it() {
    let rig = Rig::start().await;
    let t = rig.transport();
    // No answer yet: no `server` key, and `paired` is `None` before a pairing.
    assert_eq!(t.paired().await.unwrap(), None);
    // `pair`'s answer names the server.
    let pairing = t.pair(&rig.code(), "macbook").await.unwrap();
    let named = pairing.server.clone().expect("pair names the server");
    assert_eq!(named.name, SERVER_NAME);
    assert_eq!(named.id, Some(hex(&rig.pin)));
    assert_eq!(
        t.paired().await.unwrap().unwrap().server,
        Some(named.clone())
    );
    // A second transport over the same store has seen no answer yet: no server, and no key in JSON.
    let fresh = rig.transport();
    let before = fresh.paired().await.unwrap().unwrap();
    assert_eq!(before.server, None);
    let json = serde_json::to_value(&before).unwrap();
    assert!(json.get("server").is_none(), "{json}");
    // A verb's answer names it, `status` too.
    fresh
        .session_list(SessionListArgs::default())
        .await
        .unwrap();
    assert_eq!(
        fresh.paired().await.unwrap().unwrap().server,
        Some(named.clone())
    );
    let other = rig.transport();
    other.status().await.unwrap();
    assert_eq!(
        other.paired().await.unwrap().unwrap().server,
        Some(named.clone())
    );
    // `forget` clears it.
    t.forget().await.unwrap();
    rig.paired();
    assert_eq!(t.paired().await.unwrap().unwrap().server, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_header_that_names_another_id_is_ignored_and_the_answer_still_parses() {
    let rig = Rig::start_with(true).await;
    rig.paired();
    let t = rig.transport();
    let list = t.session_list(SessionListArgs::default()).await.unwrap();
    assert_eq!(list.sessions.len(), 1);
    t.status().await.unwrap();
    assert_eq!(t.paired().await.unwrap().unwrap().server, None);
}
