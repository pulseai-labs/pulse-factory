//! AC-10 of `r1.s4.w4`: the settings verbs over the real API. A live controller with a `FakeChat`,
//! the API on `127.0.0.1:0`, a paired device and the verbs over pinned TLS. Nothing here reaches
//! beyond `127.0.0.1` or starts `claude`.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::chat::*;
use common::ctl::Live;
use pf_client::pin::{PinVerifier, client_config};
use pf_engine::port::store::{AdmissionOutcome, CONTROLLER_SCOPE, Store};
use pf_ipc::Handler;
use pulse_factory::controller::api;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use serde_json::{Value, json};

const CAP: &str = "chat.self_turn_cap";

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
    pin: [u8; 32],
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
        stop: Some(tx),
        thread: Some(thread),
    }
}

struct Answer {
    status: u16,
    body: Value,
}

impl Api {
    /// One HTTPS call, pinned.
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
        let made = pf_ipc::call(&self.live.sb.state, "device_pair", json!({})).unwrap();
        assert!(made.ok, "{:?}", made.error);
        let code = made.result["code"].as_str().unwrap().to_string();
        let body = json!({"code": code, "device_name": name}).to_string();
        let reply = self.http("POST", "/v1/pair", None, Some(&body));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.body["token"].as_str().unwrap().to_string()
    }

    fn api_ledger(&self) -> Vec<String> {
        lines(&self.live.sb.state.join("api.ledger"))
    }

    /// The settings lines of `controller.ledger`.
    fn setting_lines(&self) -> Vec<String> {
        controller_ledger(&self.live)
            .into_iter()
            .filter(|l| l.contains(" setting_set ") || l.contains(" setting_unset "))
            .collect()
    }

    fn admissions(&self) -> Vec<pf_engine::port::store::AdmissionRow> {
        self.live
            .controller
            .store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
    }
}

/// The `api.ledger` lines of one verb.
fn verb_lines(api: &Api, verb: &str) -> Vec<String> {
    api.api_ledger()
        .into_iter()
        .filter(|l| l.contains(&format!("verb={verb} ")))
        .collect()
}

#[test]
fn a_paired_device_reads_writes_and_unsets_a_setting() {
    let api = api();
    let token = api.pair("macbook");
    add_project(&api.live, &repo(&api.live, "alpha"));

    let got = api.verb(&token, "settings_get", json!({}));
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["schema_version"], 1);
    assert_eq!(got.body["settings"].as_array().unwrap().len(), 3);

    let set = api.verb(
        &token,
        "settings_set",
        json!({"key": CAP, "value": 7, "project_id": "P-1", "command_id": "dev-set-1"}),
    );
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(set.body["replayed"], false);
    assert_eq!(set.body["value"], 7);
    assert_eq!(set.body["resolved"]["value"], 7);

    let after = api.verb(
        &token,
        "settings_get",
        json!({"key": CAP, "project_id": "P-1"}),
    );
    assert_eq!(after.body["settings"][0]["value"], 7, "{}", after.body);
    assert_eq!(after.body["settings"][0]["from"]["kind"], "project");

    let unset = api.verb(
        &token,
        "settings_unset",
        json!({"key": CAP, "project_id": "P-1", "command_id": "dev-unset-1"}),
    );
    assert_eq!(unset.status, 200, "{}", unset.body);
    assert_eq!(unset.body["value"], Value::Null);
    let after = api.verb(
        &token,
        "settings_get",
        json!({"key": CAP, "project_id": "P-1"}),
    );
    assert_eq!(after.body["settings"][0]["from"]["kind"], "built_in");
}

#[test]
fn a_device_write_names_the_device_and_a_replay_adds_nothing() {
    let api = api();
    let token = api.pair("macbook");
    add_project(&api.live, &repo(&api.live, "alpha"));
    let lines_before = api.setting_lines().len();
    let adm_before = api.admissions().len();

    let args = json!({"key": CAP, "value": 4, "project_id": "P-1", "command_id": "dev-cmd"});
    let first = api.verb(&token, "settings_set", args.clone());
    assert_eq!(first.status, 200, "{}", first.body);

    let lines = api.setting_lines();
    assert_eq!(lines.len(), lines_before + 1, "{lines:#?}");
    let line = lines.last().unwrap();
    assert!(line.ends_with(" by=D-1 command=dev-cmd"), "{line}");
    assert!(line.contains("scope=project:P-1 value=4"), "{line}");
    let admissions = api.admissions();
    assert_eq!(admissions.len(), adm_before + 1);
    let adm = admissions.last().unwrap();
    assert_eq!(adm.command_id, "dev-cmd");
    assert_eq!(adm.kind, "setting_set");
    assert_eq!(adm.outcome, AdmissionOutcome::Admitted);
    let stored = api.live.controller.store().setting_rows().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].set_by, "D-1");

    // The same ID and content replays: the answer says so and nothing is added.
    let replay = api.verb(&token, "settings_set", args);
    assert_eq!(replay.status, 200, "{}", replay.body);
    assert_eq!(replay.body["replayed"], true);
    assert_eq!(api.setting_lines().len(), lines_before + 1, "no new line");
    assert_eq!(api.admissions().len(), adm_before + 1, "no new admission");
    let ledger = verb_lines(&api, "settings_set");
    assert_eq!(ledger.len(), 2, "{ledger:#?}");
    assert!(ledger[0].contains("outcome=ok"), "{}", ledger[0]);
    assert!(ledger[1].contains("outcome=replayed"), "{}", ledger[1]);
}

/// An unset that carries a `value` in its arguments is no set: a later set with that value and the
/// same command ID must write its row, not replay the unset (review B1).
#[test]
fn a_set_after_an_unset_that_carried_a_value_is_not_a_replay() {
    let api = api();
    let token = api.pair("macbook");
    add_project(&api.live, &repo(&api.live, "alpha"));
    api.verb(
        &token,
        "settings_set",
        json!({"key": CAP, "value": 3, "project_id": "P-1", "command_id": "seed"}),
    );
    let unset = api.verb(
        &token,
        "settings_unset",
        json!({"key": CAP, "value": 7, "project_id": "P-1", "command_id": "same"}),
    );
    assert_eq!(unset.status, 200, "{}", unset.body);
    assert!(
        api.live
            .controller
            .store()
            .setting_rows()
            .unwrap()
            .is_empty()
    );

    let set = api.verb(
        &token,
        "settings_set",
        json!({"key": CAP, "value": 7, "project_id": "P-1", "command_id": "same"}),
    );
    assert_ne!(
        set.status, 200,
        "the ID is spent by the unset: {}",
        set.body
    );
    assert_ne!(set.body["replayed"], true, "{}", set.body);
    assert!(
        api.live
            .controller
            .store()
            .setting_rows()
            .unwrap()
            .is_empty(),
        "a conflict writes no row"
    );
}

#[test]
fn each_call_writes_one_request_line_with_the_device_and_the_outcome() {
    let api = api();
    let token = api.pair("macbook");
    add_project(&api.live, &repo(&api.live, "alpha"));
    let before = api.api_ledger().len();

    api.verb(&token, "settings_get", json!({}));
    api.verb(
        &token,
        "settings_set",
        json!({"key": CAP, "value": 2, "command_id": "k-1"}),
    );
    // An unknown key is refused.
    let refused = api.verb(
        &token,
        "settings_set",
        json!({"key": "claim.required", "value": 1, "command_id": "k-2"}),
    );
    assert_eq!(refused.status, 422, "{}", refused.body);
    api.verb(
        &token,
        "settings_unset",
        json!({"key": CAP, "command_id": "k-3"}),
    );

    let ledger = api.api_ledger();
    let new: Vec<&String> = ledger[before..]
        .iter()
        .filter(|l| l.contains("route=verb"))
        .collect();
    assert_eq!(new.len(), 4, "{ledger:#?}");
    for (line, (verb, outcome)) in new.iter().zip([
        ("settings_get", "ok"),
        ("settings_set", "ok"),
        ("settings_set", "refused"),
        ("settings_unset", "ok"),
    ]) {
        assert!(line.contains(&format!("verb={verb} ")), "{line}");
        assert!(line.contains("device=D-1"), "{line}");
        assert!(line.contains(&format!("outcome={outcome}")), "{line}");
    }
    for line in &ledger {
        assert!(!line.contains(&token), "the token is not in the ledger");
    }
    // The refused call is one refused admission with one line.
    let lines = api.setting_lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("setting_set refused reason=\"unknown setting claim.required")),
        "{lines:#?}"
    );
}

#[test]
fn a_revoked_device_is_refused_and_admits_nothing() {
    let api = api();
    let token = api.pair("macbook");
    let adm_before = api.admissions().len();
    let revoked = pf_ipc::call(
        &api.live.sb.state,
        "device_revoke",
        json!({"id": "D-1", "command_id": "revoke-1"}),
    )
    .unwrap();
    assert!(revoked.ok, "{:?}", revoked.error);
    let adm_after_revoke = api.admissions().len();
    assert_eq!(adm_after_revoke, adm_before + 1);
    let lines_before = api.setting_lines().len();

    let reply = api.verb(
        &token,
        "settings_set",
        json!({"key": CAP, "value": 9, "command_id": "late"}),
    );
    assert_eq!(reply.status, 401, "{}", reply.body);
    assert_eq!(api.admissions().len(), adm_after_revoke, "no admission");
    assert_eq!(api.setting_lines().len(), lines_before, "no line");
    assert!(
        api.live
            .controller
            .store()
            .setting_rows()
            .unwrap()
            .is_empty()
    );
    let line = verb_lines(&api, "settings_set");
    assert!(line[0].contains("status=401"), "{line:#?}");
}

#[test]
fn the_verbs_of_the_earlier_rounds_still_reach_the_controller() {
    let api = api();
    let token = api.pair("macbook");
    let list = api.verb(&token, "session_list", json!({}));
    assert_eq!(list.status, 200, "{}", list.body);
    let projects = api.verb(&token, "project_list", json!({}));
    assert_eq!(projects.status, 200, "{}", projects.body);
    // A verb off the list is still 403.
    let halt = api.verb(&token, "halt", json!({}));
    assert_eq!(halt.status, 403, "{}", halt.body);
}
