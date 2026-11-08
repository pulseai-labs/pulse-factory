//! AC-2 of `r1.s2.w4`: `pf-client` against a real controller. The controller is the built binary
//! (`pulse-factory controller`) with the API on `127.0.0.1`, and the client has a `FileStore` in a
//! temp directory. Nothing here reaches beyond `127.0.0.1`, and nothing spends money.

mod common;

use std::path::PathBuf;
use std::process::{Child, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use pf_api::code::PairingCode;
use pf_client::error::{ClientError, ErrorKind};
use pf_client::http::Timeouts;
use pf_client::store::{DeviceStore, FileStore};
use pf_client::transport::Transport;
use serde_json::{Value, json};

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

struct Proc {
    child: Child,
    stderr: PathBuf,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A sandbox, a controller process with the API on, and a client store of its own.
struct Rig {
    sb: Sandbox,
    addr: String,
    proc: Option<Proc>,
    store: Arc<FileStore>,
    _client_dir: tempfile::TempDir,
}

impl Rig {
    fn on() -> Rig {
        let sb = Sandbox::new();
        let client_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::new(client_dir.path().join("device.json")));
        let mut rig = Rig {
            sb,
            addr: format!("127.0.0.1:{}", free_port()),
            proc: None,
            store,
            _client_dir: client_dir,
        };
        rig.start();
        rig
    }

    fn start(&mut self) {
        let stderr = self.sb.root.path().join("controller.stderr");
        let child = self
            .sb
            .cmd(&[
                "controller",
                "--tick-ms",
                "3600000",
                "--api-listen",
                &self.addr,
            ])
            .stdout(Stdio::null())
            .stderr(
                std::fs::File::options()
                    .create(true)
                    .append(true)
                    .open(&stderr)
                    .unwrap(),
            )
            .spawn()
            .expect("start the controller");
        self.proc = Some(Proc { child, stderr });
        for _ in 0..400 {
            if self.state_json().is_some_and(|v| v["state"] == "listening") {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the API never listened: {:?}", self.state_json());
    }

    fn stop(&mut self) {
        self.proc = None;
    }

    fn state_json(&self) -> Option<Value> {
        pf_ipc::call(&self.sb.state, "api_status", json!({}))
            .ok()
            .filter(|r| r.ok)
            .map(|r| r.result)
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.sb.cmd(args).output().expect("run pulse-factory")
    }

    fn code(&self) -> String {
        let out = self.cli(&["device", "pair", "--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
        v["code"].as_str().unwrap().to_string()
    }

    fn transport(&self) -> Transport<FileStore> {
        Transport::new(self.store.clone()).with_timeouts(Timeouts {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(10),
        })
    }

    fn ledger(&self) -> Vec<String> {
        std::fs::read_to_string(self.sb.state.join("api.ledger"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn controller_stderr(&self) -> String {
        self.proc
            .as_ref()
            .map(|p| std::fs::read_to_string(&p.stderr).unwrap_or_default())
            .unwrap_or_default()
    }

    fn project_dir(&self, name: &str) -> String {
        let dir = self.sb.root.path().join("work").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn token(&self) -> String {
        self.store.load().unwrap().expect("a paired device").token
    }
}

fn add_args(root: &str, name: &str, id: &str) -> pf_api::types::ProjectAddArgs {
    serde_json::from_value(json!({ "root": root, "name": name, "command_id": id })).unwrap()
}

/// The same code with another key hash: a code for another server.
fn code_for_another_key(text: &str) -> String {
    let mut c = PairingCode::decode(text).unwrap();
    c.spki_sha256[0] ^= 0xff;
    c.encode()
}

/// The same code with another secret: the server refuses it.
fn code_with_another_secret(text: &str) -> String {
    let mut c = PairingCode::decode(text).unwrap();
    c.secret[0] ^= 0xff;
    c.encode()
}

#[tokio::test(flavor = "current_thread")]
async fn pair_status_project_add_and_project_list_work_and_the_token_stays_in_the_store() {
    let rig = Rig::on();
    let t = rig.transport();
    assert_eq!(t.paired().await.unwrap(), None);

    let code_text = rig.code();
    let paired = t.pair(&code_text, "macbook").await.unwrap();
    assert_eq!(paired.address, rig.addr);
    assert_eq!(paired.device.id, "D-1");
    assert_eq!(paired.device.name, "macbook");

    // The token is in the store, and `pair`'s return holds none of it.
    let token = rig.token();
    assert_eq!(token.len(), 64);
    let returned = serde_json::to_string(&paired).unwrap();
    assert!(!returned.contains(&token), "{returned}");
    assert!(!format!("{paired:?}").contains(&token));
    assert_eq!(t.paired().await.unwrap(), Some(paired.clone()));

    let status = t.status().await.unwrap();
    assert_eq!(status.device.as_ref().map(|d| d.id.as_str()), Some("D-1"));
    assert_eq!(
        status.key_spki_sha256,
        rig.state_json().unwrap()["key_spki_sha256"]
    );

    let root = rig.project_dir("alpha");
    let added = t
        .project_add(add_args(&root, "alpha", "client-cmd-1"))
        .await
        .unwrap();
    assert_eq!(added.project.name, "alpha");
    assert!(!added.replayed);
    let list = t.project_list().await.unwrap();
    assert_eq!(list.projects.len(), 1);
    assert_eq!(list.projects[0].name, "alpha");
    assert_eq!(list.projects[0].root, root);

    // A retry with the same command id is a replay.
    let again = t
        .project_add(add_args(&root, "alpha", "client-cmd-1"))
        .await
        .unwrap();
    assert!(again.replayed);

    // A verb off the allowlist is refused by the client; the server's ledger never sees it.
    let before = rig.ledger().len();
    let halt = t.verb("halt", json!({})).await.unwrap_err();
    assert_eq!(halt.kind, ErrorKind::BadRequest);
    assert_eq!(rig.ledger().len(), before);

    // The token is in no log of this test.
    assert!(!rig.controller_stderr().contains(&token));
    assert!(!rig.ledger().join("\n").contains(&token));
    assert!(!rig.ledger().join("\n").contains(&code_text));
}

/// A store that records the thread of its `save`, and keeps the pairing in a `FileStore`.
struct SaveThread {
    inner: FileStore,
    saved_on: std::sync::Mutex<Option<std::thread::ThreadId>>,
}

impl DeviceStore for SaveThread {
    fn load(&self) -> Result<Option<pf_client::store::Paired>, ClientError> {
        self.inner.load()
    }
    fn save(&self, paired: &pf_client::store::Paired) -> Result<(), ClientError> {
        *self.saved_on.lock().unwrap() = Some(std::thread::current().id());
        self.inner.save(paired)
    }
    fn forget(&self) -> Result<(), ClientError> {
        self.inner.forget()
    }
}

/// Correction of `r1.s2.w4`: `pair` writes the device off the async thread. The runtime is
/// `current_thread`, so the thread of this test is the thread that polls `pair`.
#[tokio::test(flavor = "current_thread")]
async fn pair_saves_the_device_off_the_async_thread() {
    let rig = Rig::on();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SaveThread {
        inner: FileStore::new(dir.path().join("device.json")),
        saved_on: std::sync::Mutex::new(None),
    });
    let t = Transport::new(store.clone()).with_timeouts(Timeouts {
        connect: Duration::from_secs(5),
        request: Duration::from_secs(10),
    });
    t.pair(&rig.code(), "macbook").await.unwrap();
    let saved_on = store
        .saved_on
        .lock()
        .unwrap()
        .expect("pair saved the device");
    assert_ne!(
        saved_on,
        std::thread::current().id(),
        "save ran on the async thread"
    );
    assert!(store.inner.load().unwrap().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn a_used_code_is_refused_and_a_code_for_another_key_is_key_changed() {
    let rig = Rig::on();
    let t = rig.transport();
    let code_text = rig.code();
    t.pair(&code_text, "macbook").await.unwrap();

    let used = t.pair(&code_text, "macbook-2").await.unwrap_err();
    assert_eq!(used.kind, ErrorKind::CodeRefused);
    assert!(!used.message.contains(&code_text), "{used}");

    let fresh = rig.code();
    let lines = rig.ledger().len();
    let other = t
        .pair(&code_for_another_key(&fresh), "macbook-3")
        .await
        .unwrap_err();
    assert_eq!(other.kind, ErrorKind::KeyChanged);
    assert_eq!(other.message, "server key changed: re-pair");
    assert_eq!(rig.ledger().len(), lines, "nothing reached HTTP");

    // A text that is no code makes no network call at all.
    let junk = t.pair("pf1-garbage", "macbook").await.unwrap_err();
    assert_eq!(junk.kind, ErrorKind::CodeRefused);
    assert_eq!(rig.ledger().len(), lines);

    // The code that the key mismatch left alone still works.
    let ok = t.pair(&fresh, "macbook-4").await.unwrap();
    assert_eq!(ok.device.id, "D-2");
}

#[tokio::test(flavor = "current_thread")]
async fn a_revoked_device_is_unauthorized_on_its_next_call() {
    let rig = Rig::on();
    let t = rig.transport();
    t.pair(&rig.code(), "macbook").await.unwrap();
    t.status().await.unwrap();

    let out = rig.cli(&["device", "revoke", "D-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let e = t.status().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unauthorized);
    let e = t.project_list().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unauthorized);
    // The client keeps its record: the UI offers "Pair again" and `forget` drops it.
    assert!(t.paired().await.unwrap().is_some());
    t.forget().await.unwrap();
    assert!(t.paired().await.unwrap().is_none());
    let e = t.status().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unauthorized);
    assert_eq!(e.message, "this device is not paired");
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_server_key_fails_closed_before_any_request_reaches_http() {
    let mut rig = Rig::on();
    let t = rig.transport();
    t.pair(&rig.code(), "macbook").await.unwrap();
    t.status().await.unwrap();
    let token = rig.token();

    // The key files go, and the controller starts again: it makes a new key.
    rig.stop();
    let api = rig.sb.state.join("api");
    std::fs::remove_file(api.join("key.pem")).unwrap();
    std::fs::remove_file(api.join("cert.pem")).unwrap();
    let lines_before = rig.ledger();
    rig.start();
    assert!(api.join("key.pem").exists());

    let e = t.status().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::KeyChanged);
    assert_eq!(e.message, "server key changed: re-pair");
    let e = t.project_list().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::KeyChanged);
    // Nothing reached HTTP, so the ledger gained no line, and the token was not sent.
    assert_eq!(rig.ledger(), lines_before);
    assert!(!rig.controller_stderr().contains(&token));
}

#[tokio::test(flavor = "current_thread")]
async fn a_blocked_peer_is_rate_limited_and_a_closed_port_is_unreachable() {
    let mut rig = Rig::on();
    let t = rig.transport();
    t.pair(&rig.code(), "macbook").await.unwrap();

    // Five refusals of a code block the peer. Each code decodes, and its secret is wrong.
    let victim = rig.code();
    let bad = code_with_another_secret(&victim);
    for n in 1..=5 {
        let e = t.pair(&bad, "macbook-x").await.unwrap_err();
        assert_eq!(e.kind, ErrorKind::CodeRefused, "failure {n}");
    }
    let e = t.status().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::RateLimited, "{e}");
    assert!(e.message.starts_with("too many failed attempts"), "{e}");
    let e = t.pair(&victim, "macbook-y").await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::RateLimited, "{e}");

    // The controller stops: the port is closed.
    rig.stop();
    let e = t.status().await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unreachable);
    assert_eq!(e.message, format!("cannot reach {}", rig.addr));
}

#[tokio::test(flavor = "current_thread")]
async fn no_error_stderr_line_or_ledger_line_holds_the_token_or_a_code() {
    let rig = Rig::on();
    let t = rig.transport();
    let first = rig.code();
    t.pair(&first, "macbook").await.unwrap();
    let token = rig.token();
    let second = rig.code();

    let mut errors: Vec<ClientError> = Vec::new();
    errors.push(t.pair(&first, "again").await.unwrap_err());
    errors.push(
        t.pair(&code_with_another_secret(&second), "x")
            .await
            .unwrap_err(),
    );
    errors.push(
        t.pair(&code_for_another_key(&second), "x")
            .await
            .unwrap_err(),
    );
    errors.push(t.verb("halt", json!({})).await.unwrap_err());
    errors.push(
        t.verb(
            "project_add",
            json!({ "root": "relative", "command_id": "c" }),
        )
        .await
        .unwrap_err(),
    );
    errors.push(t.pair(&second, "").await.unwrap_err());

    let secrets = [
        token.clone(),
        first.clone(),
        second.clone(),
        PairingCode::decode(&first).unwrap().secret_hex(),
    ];
    let log = [
        rig.controller_stderr(),
        rig.ledger().join("\n"),
        errors
            .iter()
            .map(|e| format!("{}|{e:?}", e.message))
            .collect::<Vec<_>>()
            .join("\n"),
    ]
    .join("\n");
    for s in &secrets {
        assert!(
            !log.contains(s.as_str()),
            "a secret is in a log or an error:\n{log}"
        );
    }
}
