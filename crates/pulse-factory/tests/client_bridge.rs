//! AC-3 of `r1.s2.w4`: `pf_client::bridge::serve` in the test process against a real controller (the
//! built binary with the API on `127.0.0.1`). A test in this package cannot name another package's
//! binary, so the built `pf-client-bridge` runs in d12 (AC-4). Nothing here reaches beyond
//! `127.0.0.1`, and nothing spends money.

mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use common::*;
use pf_api::code::PairingCode;
use pf_client::bridge::serve;
use serde_json::{Value, json};

const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'";

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

struct Controller {
    child: Child,
    stderr: PathBuf,
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The bridge on a thread of its own, with its own runtime: the tests make blocking calls.
struct BridgeThread {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BridgeThread {
    fn start(ui: PathBuf, state: PathBuf) -> BridgeThread {
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let bridge = serve(ui, state).await.expect("start the bridge");
                addr_tx.send(bridge.addr()).unwrap();
                let _ = stopped.await;
                bridge.shutdown().await;
            });
        });
        BridgeThread {
            addr: addr_rx.recv().unwrap(),
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}

impl Drop for BridgeThread {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Rig {
    sb: Sandbox,
    ctl: Controller,
    ui: PathBuf,
    state: PathBuf,
    bridge: BridgeThread,
    /// Every answer body of the bridge, for the no-token check.
    seen: std::cell::RefCell<Vec<String>>,
}

struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

impl Rig {
    fn on() -> Rig {
        let sb = Sandbox::new();
        let addr = format!("127.0.0.1:{}", free_port());
        let stderr = sb.root.path().join("controller.stderr");
        let child = sb
            .cmd(&["controller", "--tick-ms", "3600000", "--api-listen", &addr])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .expect("start the controller");
        let ctl = Controller { child, stderr };
        for _ in 0..400 {
            let state = pf_ipc::call(&sb.state, "api_status", json!({}))
                .ok()
                .filter(|r| r.ok);
            if state.is_some_and(|r| r.result["state"] == "listening") {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // A bundle with an index, an asset, and a secret beside it that must stay unserved.
        let ui = sb.root.path().join("bundle/ui");
        std::fs::create_dir_all(ui.join("assets")).unwrap();
        std::fs::write(ui.join("index.html"), "<!doctype html><title>pf</title>").unwrap();
        std::fs::write(ui.join("assets/app.js"), "console.log(1)").unwrap();
        std::fs::write(sb.root.path().join("bundle/secret.txt"), "TOP-SECRET").unwrap();
        std::os::unix::fs::symlink(
            sb.root.path().join("bundle/secret.txt"),
            ui.join("link.txt"),
        )
        .unwrap();
        let state = sb.root.path().join("bridge-state");
        let bridge = BridgeThread::start(ui.clone(), state.clone());
        Rig {
            sb,
            ctl,
            ui,
            state,
            bridge,
            seen: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn addr(&self) -> SocketAddr {
        self.bridge.addr
    }

    fn code(&self) -> String {
        let out = self.sb.cmd(&["device", "pair", "--json"]).output().unwrap();
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
        v["code"].as_str().unwrap().to_string()
    }

    /// One raw HTTP/1.1 request on a fresh connection. `headers` replace the defaults by name.
    fn raw(
        &self,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Reply {
        let own = format!("127.0.0.1:{}", self.addr().port());
        let mut h: Vec<(String, String)> = vec![("Host".into(), own)];
        for (k, v) in headers {
            h.retain(|(name, _)| !name.eq_ignore_ascii_case(k));
            h.push(((*k).into(), (*v).into()));
        }
        let mut req = format!("{method} {target} HTTP/1.1\r\n");
        for (k, v) in &h {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("Connection: close\r\n\r\n");
        if let Some(b) = body {
            req.push_str(b);
        }
        let mut s = TcpStream::connect(self.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let _ = s.read_to_end(&mut raw);
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let reply = Reply {
            status,
            headers,
            body: body.to_string(),
        };
        self.seen.borrow_mut().push(reply.body.clone());
        reply
    }

    /// A POST with the content type and the origin of the bundle's own page.
    fn post(&self, path: &str, body: &Value) -> Reply {
        let origin = format!("http://127.0.0.1:{}", self.addr().port());
        self.raw(
            "POST",
            path,
            &[("Content-Type", "application/json"), ("Origin", &origin)],
            Some(&body.to_string()),
        )
    }

    fn get(&self, path: &str) -> Reply {
        self.raw("GET", path, &[], None)
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(self.sb.state.join("api.ledger")).unwrap_or_default()
    }

    fn device_json(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.state.join("device.json")).unwrap())
            .unwrap()
    }
}

fn kind(v: &Value) -> &str {
    assert_eq!(v["ok"], false, "{v}");
    v["error"]["kind"]
        .as_str()
        .unwrap_or_else(|| panic!("no kind in {v}"))
}

#[test]
fn every_route_answers_in_the_envelope_and_no_answer_holds_the_token() {
    let rig = Rig::on();
    let a = {
        // Nothing is paired yet.
        let r = rig.get("/bridge/paired");
        assert_eq!(
            (r.status, r.json()),
            (200, json!({ "ok": true, "result": null }))
        );
        assert_eq!(r.headers["content-type"], "application/json");
        let r = rig.post("/bridge/status", &json!({}));
        assert_eq!(r.status, 200);
        assert_eq!(kind(&r.json()), "unauthorized");

        // Pair.
        let code_text = rig.code();
        let r = rig.post(
            "/bridge/pair",
            &json!({ "code": code_text, "device_name": "macbook" }),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let v = r.json();
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["result"]["device"]["id"], "D-1");
        assert_eq!(
            v["result"]["address"],
            PairingCode::decode(&code_text).unwrap().listen
        );
        assert!(v["result"].get("token").is_none());

        // The token is in device.json, mode 0600, and in no answer.
        let device = rig.device_json();
        let token = device["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 64);
        let mode = std::fs::metadata(rig.state.join("device.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let r = rig.get("/bridge/paired");
        assert_eq!(r.json()["result"]["device"]["id"], "D-1");
        let r = rig.post("/bridge/status", &json!({}));
        assert_eq!(r.json()["result"]["device"]["id"], "D-1", "{}", r.body);
        let root = rig.sb.root.path().join("work/alpha");
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let r = rig.post(
            "/bridge/verbs/project_add",
            &json!({ "root": root, "name": "alpha", "command_id": "bridge-1" }),
        );
        assert_eq!(r.json()["result"]["project"]["name"], "alpha", "{}", r.body);
        let r = rig.post("/bridge/verbs/project_list", &json!({}));
        assert_eq!(
            r.json()["result"]["projects"][0]["name"],
            "alpha",
            "{}",
            r.body
        );

        // Forget removes the file.
        let r = rig.post("/bridge/forget", &json!({}));
        assert_eq!(r.json(), json!({ "ok": true, "result": null }));
        assert!(!rig.state.join("device.json").exists());
        let r = rig.get("/bridge/paired");
        assert_eq!(r.json()["result"], Value::Null);
        (token, code_text)
    };
    let (token, code_text) = a;
    for body in rig.seen.borrow().iter() {
        assert!(!body.contains(&token), "a token is in an answer: {body}");
        assert!(!body.contains(&code_text), "a code is in an answer: {body}");
    }
    let log = std::fs::read_to_string(&rig.ctl.stderr).unwrap_or_default();
    assert!(!log.contains(&token) && !log.contains(&code_text));
}

#[test]
fn a_failed_pair_names_its_kind() {
    let rig = Rig::on();
    {
        let code_text = rig.code();
        let ok = rig.post(
            "/bridge/pair",
            &json!({ "code": code_text, "device_name": "macbook" }),
        );
        assert_eq!(ok.json()["ok"], true);
        // The used code.
        let used = rig.post(
            "/bridge/pair",
            &json!({ "code": code_text, "device_name": "again" }),
        );
        assert_eq!(kind(&used.json()), "code_refused");
        // A code for another key.
        let mut c = PairingCode::decode(&rig.code()).unwrap();
        c.spki_sha256[3] ^= 1;
        let other = rig.post(
            "/bridge/pair",
            &json!({ "code": c.encode(), "device_name": "x" }),
        );
        assert_eq!(kind(&other.json()), "key_changed");
        assert_eq!(
            other.json()["error"]["message"],
            "server key changed: re-pair"
        );
        // A body that is not a pair request.
        let r = rig.post("/bridge/pair", &json!({ "code": 5 }));
        assert_eq!(kind(&r.json()), "bad_request");
    }
}

#[test]
fn a_foreign_host_a_foreign_origin_and_a_post_without_json_each_answer_403() {
    let rig = Rig::on();
    {
        for path in ["/bridge/paired", "/", "/assets/app.js"] {
            let r = rig.raw("GET", path, &[("Host", "evil.example:80")], None);
            assert_eq!(r.status, 403, "{path}");
            let r = rig.raw("GET", path, &[("Host", "localhost:1")], None);
            assert_eq!(r.status, 403, "{path}");
        }
        let r = rig.raw(
            "GET",
            "/bridge/paired",
            &[("Origin", "http://evil.example")],
            None,
        );
        assert_eq!(r.status, 403);
        let r = rig.raw("GET", "/bridge/paired", &[("Origin", "null")], None);
        assert_eq!(r.status, 403);
        // The bundle's own origin passes.
        let own = format!("http://127.0.0.1:{}", rig.addr().port());
        let r = rig.raw("GET", "/bridge/paired", &[("Origin", &own)], None);
        assert_eq!(r.status, 200);

        for ct in [
            "text/plain",
            "application/x-www-form-urlencoded",
            "multipart/form-data; boundary=x",
        ] {
            let r = rig.raw(
                "POST",
                "/bridge/status",
                &[("Content-Type", ct)],
                Some("{}"),
            );
            assert_eq!(r.status, 403, "{ct}");
        }
        let r = rig.raw("POST", "/bridge/status", &[], Some("{}"));
        assert_eq!(r.status, 403, "no content type");
        // A charset after the type is fine.
        let r = rig.raw(
            "POST",
            "/bridge/status",
            &[("Content-Type", "application/json; charset=utf-8")],
            Some("{}"),
        );
        assert_eq!(r.status, 200);
    }
}

#[test]
fn a_verb_name_must_match_the_rule_and_no_verb_off_the_allowlist_reaches_the_server() {
    let rig = Rig::on();
    {
        let code_text = rig.code();
        rig.post(
            "/bridge/pair",
            &json!({ "code": code_text, "device_name": "macbook" }),
        );
        let before = rig.ledger();

        for name in [
            "Project_Add",
            "1abc",
            "_x",
            "a-b",
            "a.b",
            "",
            "a%20b",
            "x".repeat(65).as_str(),
        ] {
            let r = rig.post(&format!("/bridge/verbs/{name}"), &json!({}));
            assert_eq!(r.status, 200, "{name}");
            assert_eq!(kind(&r.json()), "bad_request", "{name}");
        }
        for verb in ["halt", "claim", "device_revoke", "no_such_verb"] {
            let r = rig.post(&format!("/bridge/verbs/{verb}"), &json!({}));
            assert_eq!(r.status, 200, "{verb}");
            assert_eq!(kind(&r.json()), "bad_request", "{verb}");
        }
        let r = rig.post("/bridge/verbs/project_list", &json!([]));
        assert_eq!(
            kind(&r.json()),
            "bad_request",
            "arguments that are no object"
        );
        assert_eq!(rig.ledger(), before, "no refused verb reached the server");

        // Bad bodies and routes.
        let origin = format!("http://127.0.0.1:{}", rig.addr().port());
        let r = rig.raw(
            "POST",
            "/bridge/status",
            &[("Content-Type", "application/json"), ("Origin", &origin)],
            Some("{not json"),
        );
        assert_eq!(kind(&r.json()), "bad_request");
        let r = rig.get("/bridge/status");
        assert_eq!((r.status, kind(&r.json())), (200, "bad_request"));
        let r = rig.post("/bridge/nowhere", &json!({}));
        assert_eq!(kind(&r.json()), "bad_request");
    }
}

#[test]
fn the_bundle_is_served_and_nothing_outside_its_directory_is() {
    let rig = Rig::on();
    {
        let r = rig.get("/");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("<title>pf</title>"));
        assert!(r.headers["content-type"].starts_with("text/html"));
        let r = rig.get("/assets/app.js");
        assert_eq!(r.status, 200);
        assert!(r.headers["content-type"].starts_with("text/javascript"));
        assert_eq!(rig.get("/assets").status, 404, "a directory with no index");
        assert_eq!(rig.get("/missing.css").status, 404);

        for target in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/..%2fsecret.txt",
            "/assets/../../secret.txt",
            "/assets/%2e%2e/%2e%2e/secret.txt",
            "/link.txt",
            "//etc/passwd",
            "/%00",
            "/..\\secret.txt",
        ] {
            let r = rig.raw("GET", target, &[], None);
            assert_ne!(r.status, 200, "{target} was served");
            assert!(!r.body.contains("TOP-SECRET"), "{target}: {}", r.body);
        }
        // A method other than GET and HEAD on a file is refused.
        let r = rig.post("/index.html", &json!({}));
        assert_eq!(r.status, 405);
        let _ = &rig.ui;
    }
}

/// Correction of `r1.s2.w4`: a file read for the bundle does not stop the bridge. A FIFO in the
/// bundle makes `read` wait for a writer. The bridge runs on one thread, so a read on the async
/// path would hold every other request until the writer came. A read on the blocking pool does not.
#[test]
fn a_slow_bundle_file_does_not_hold_up_another_request() {
    let rig = Rig::on();
    let fifo = rig.ui.join("slow.bin");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo");

    let own = rig.addr();
    let slow = std::thread::spawn(move || {
        let mut s = TcpStream::connect(own).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let req = format!(
            "GET /slow.bin HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            own.port()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let _ = s.read_to_end(&mut raw);
        String::from_utf8_lossy(&raw).to_string()
    });
    // Let the slow request reach its read. A bridge that reads on the async path is now stuck.
    std::thread::sleep(Duration::from_millis(500));
    let quick = rig.get("/");

    // Let the slow read end (an open for writing and a close is an end of file), then judge.
    std::fs::OpenOptions::new().write(true).open(&fifo).unwrap();
    let slow_answer = slow.join().unwrap();

    assert_eq!(
        quick.status, 200,
        "another request waited for the slow read"
    );
    assert!(quick.body.contains("<title>pf</title>"));
    assert!(slow_answer.starts_with("HTTP/1.1 200"), "{slow_answer}");
}

#[test]
fn every_answer_for_the_bundle_carries_the_csp() {
    let rig = Rig::on();
    {
        for (method, path) in [
            ("GET", "/"),
            ("GET", "/assets/app.js"),
            ("GET", "/missing"),
            ("GET", "/../secret.txt"),
            ("GET", "/bridge/paired"),
        ] {
            let r = rig.raw(method, path, &[], None);
            assert_eq!(
                r.headers.get("content-security-policy").map(String::as_str),
                Some(CSP),
                "{path}"
            );
            assert_eq!(
                r.headers.get("x-content-type-options").map(String::as_str),
                Some("nosniff"),
                "{path}"
            );
            assert_eq!(
                r.headers.get("cache-control").map(String::as_str),
                Some("no-store"),
                "{path}"
            );
        }
        let r = rig.raw("GET", "/", &[("Host", "evil.example")], None);
        assert_eq!(
            r.headers.get("content-security-policy").map(String::as_str),
            Some(CSP)
        );
    }
}

/// The bridge prints nothing but its `listening` line, and that line is the binary's. The library
/// has no print macro at all, with one exception (`r1.s4.w3`): the stream's one stderr line for a
/// request frame it cannot read, one `eprintln!` in `stream.rs` that carries no frame text.
/// (The built binary's whole output is checked in d12, AC-4.)
#[test]
fn the_bridge_library_has_no_print_macro() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pf-client/src");
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for mac in ["println!", "eprintln!", "print!", "eprint!", "dbg!"] {
                    let dropped_frame_line =
                        code.contains("eprintln!") && path.ends_with("stream.rs");
                    if code.contains(mac) && !dropped_frame_line {
                        hits.push(format!("{}:{}: {mac}", path.display(), n + 1));
                    }
                }
            }
        }
    }
    assert!(hits.is_empty(), "{}", hits.join("\n"));
    // The exception is one line, not a licence: `stream.rs` holds exactly one `eprintln!`.
    let stream = std::fs::read_to_string(src.join("stream.rs")).unwrap();
    let lines = stream
        .lines()
        .filter(|l| l.split("//").next().unwrap_or("").contains("eprintln!"))
        .count();
    assert_eq!(
        lines, 1,
        "stream.rs may print only the dropped-request-frame line"
    );
}
