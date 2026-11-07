//! AC-3 of `r1.s2.w5`: the guards of the network API against a real controller. The bind guard, the
//! request ledger `api.ledger`, the peer block and the idle expiry of a device. The controller is the
//! built binary (`pulse-factory controller`) and the HTTP calls are `curl --pinnedpubkey`, as in
//! `api_pairing.rs`. The idle case needs a seam that a binary has not (`device_idle` at 3 seconds), so
//! it runs the controller in the test process, through `Config`. Nothing here reaches beyond
//! `127.0.0.1`, and nothing spends money.

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use pf_api::code::PairingCode;
use pulse_factory::controller::ports::LivePorts;
use pulse_factory::controller::{Config, Controller, api};
use serde_json::{Value, json};

// ------------------------------------------------------------------ helpers

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// `sha256//<base64>` for curl, from the 64 hex digits of an SPKI hash.
fn pin(spki_hex: &str) -> String {
    format!("sha256//{}", b64(&unhex(spki_hex)))
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
}

/// The answer of one `curl` call.
struct Http {
    status: u16,
    body: String,
}

impl Http {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// A controller process, killed on drop. Its stderr goes to a file.
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

/// A sandbox, an address and (for the binary cases) a controller process.
struct Rig {
    sb: Sandbox,
    addr: String,
    proc: Option<Proc>,
}

impl Rig {
    /// A rig with no controller yet, on a free loopback port.
    fn new() -> Rig {
        Rig {
            sb: Sandbox::new(),
            addr: format!("127.0.0.1:{}", free_port()),
            proc: None,
        }
    }

    /// The controller process, started with `--api-listen <listen>`.
    fn start(&mut self, listen: &str) {
        let stderr = self.sb.root.path().join("controller.stderr");
        let child = self
            .sb
            .cmd(&["controller", "--tick-ms", "3600000", "--api-listen", listen])
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
            if pf_ipc::call(&self.sb.state, "api_status", json!({})).is_ok_and(|r| r.ok) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the controller did not answer");
    }

    /// A rig with the controller listening on its own loopback port.
    fn on() -> Rig {
        let mut rig = Rig::new();
        let addr = rig.addr.clone();
        rig.start(&addr);
        rig.wait_state("listening");
        rig
    }

    fn wait_state(&self, want: &str) {
        for _ in 0..400 {
            if self.state_json()["state"] == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the API never reached {want}: {}", self.state_json());
    }

    fn state_json(&self) -> Value {
        pf_ipc::call(&self.sb.state, "api_status", json!({}))
            .unwrap()
            .result
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.sb.cmd(args).output().expect("run pulse-factory")
    }

    fn spki(&self) -> String {
        self.state_json()["key_spki_sha256"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// One pairing code, as `device pair --json` prints it.
    fn code(&self) -> String {
        let out = self.cli(&["device", "pair", "--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
        v["code"].as_str().unwrap().to_string()
    }

    fn http(&self, method: &str, path: &str, token: Option<&str>, body: Option<&str>) -> Http {
        let mut cmd = Command::new("curl");
        cmd.args([
            "-sS",
            "-k",
            "--max-time",
            "20",
            "--pinnedpubkey",
            &pin(&self.spki()),
        ])
        .args(["-X", method, "-w", "\n%{http_code}"])
        .arg(format!("https://{}{path}", self.addr))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        if let Some(t) = token {
            cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
        }
        if body.is_some() {
            cmd.args([
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                "@-",
            ]);
        }
        let mut child = cmd.spawn().expect("curl is on this host");
        {
            use std::io::Write;
            let mut si = child.stdin.take().unwrap();
            if let Some(b) = body {
                si.write_all(b.as_bytes()).unwrap();
            }
        }
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", "0"));
        Http {
            status: status.trim().parse().unwrap_or(0),
            body: body.to_string(),
        }
    }

    fn pair(&self, code: &str, name: &str) -> Http {
        let body = json!({ "code": code, "device_name": name }).to_string();
        self.http("POST", "/v1/pair", None, Some(&body))
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.sb.state.join(file))
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
}

// ------------------------------------------------------------------ the bind guard

const RANGE_TEXT: &str =
    "is outside loopback and the tailnet (127.0.0.0/8, ::1, 100.64.0.0/10, fd7a:115c:a1e0::/48)";

#[test]
fn an_address_outside_loopback_and_the_tailnet_fails_the_api_and_the_socket_still_serves() {
    for (listen, ip) in [
        ("0.0.0.0:7443", "0.0.0.0"),
        ("[::]:7443", "::"),
        ("192.168.1.5:7443", "192.168.1.5"),
    ] {
        let mut rig = Rig::new();
        rig.start(listen);
        let want = format!("the API address {ip} {RANGE_TEXT}");

        let state = rig.state_json();
        assert_eq!(state["state"], "failed", "{listen}: {state}");
        assert_eq!(state["error"], want.as_str(), "{listen}");
        assert_eq!(state["key_spki_sha256"], Value::Null, "{listen}");
        // No key, and so no directory for one.
        assert!(!rig.sb.state.join("api").exists(), "{listen}");

        let out = rig.cli(&["api", "status"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        assert_eq!(stdout(&out), format!("failed: {want}\n"), "{listen}");

        // `device pair` refuses with the same text.
        let out = rig.cli(&["device", "pair"]);
        assert_eq!(code(&out), 1, "{listen}");
        assert_eq!(stdout(&out), "", "{listen}");
        assert!(stderr(&out).contains(&want), "{listen}: {}", stderr(&out));

        // The controller still serves the socket.
        let out = rig.cli(&["project", "list", "--json"]);
        assert_eq!(code(&out), 0, "{listen}: {}", stderr(&out));
        // The controller said why on stderr.
        assert!(rig.controller_stderr().contains(&want), "{listen}");
    }
}

#[test]
fn a_tailnet_address_passes_the_guard_and_waits_for_the_host() {
    for listen in ["100.100.1.1:7443", "[fd7a:115c:a1e0::1]:7443"] {
        let mut rig = Rig::new();
        rig.start(listen);
        // The address is not on this host, so the bind fails with "address not available" and the
        // listener waits. It must stay `waiting`, not turn `failed`.
        std::thread::sleep(Duration::from_millis(400));
        let state = rig.state_json();
        assert_eq!(state["state"], "waiting", "{listen}: {state}");
        assert_eq!(state["error"], Value::Null, "{listen}");
        assert!(rig.sb.state.join("api/key.pem").exists(), "{listen}");
    }
}

// ------------------------------------------------------------------ the request ledger

#[test]
fn api_ledger_holds_one_line_for_each_request_and_no_secret() {
    let rig = Rig::on();
    let key_pem = std::fs::read_to_string(rig.sb.state.join("api/key.pem")).unwrap();
    let key_lines: Vec<String> = key_pem
        .lines()
        .filter(|l| !l.starts_with("-----") && !l.is_empty())
        .map(str::to_string)
        .collect();
    assert!(key_lines.iter().map(String::len).sum::<usize>() > 40);

    let code_text = rig.code();
    let secret_hex = PairingCode::decode(&code_text).unwrap().secret_hex();
    let paired = rig.pair(&code_text, "macbook");
    assert_eq!(paired.status, 200, "{}", paired.body);
    let token = paired.json()["token"].as_str().unwrap().to_string();

    let root = rig.sb.root.path().join("work/alpha");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let add = json!({ "root": root, "name": "alpha", "command_id": "guard-cmd-1" }).to_string();
    let first = rig.http("POST", "/v1/verbs/project_add", Some(&token), Some(&add));
    assert_eq!(first.status, 200, "{}", first.body);
    let again = rig.http("POST", "/v1/verbs/project_add", Some(&token), Some(&add));
    assert_eq!(
        (again.status, again.json()["replayed"].clone()),
        (200, json!(true))
    );
    let none = rig.http("POST", "/v1/verbs/project_list", None, Some("{}"));
    assert_eq!(none.status, 401);
    let off = rig.http("POST", "/v1/verbs/halt", Some(&token), Some("{}"));
    assert_eq!(off.status, 403);
    // A verb that breaks a line if it is written as is, and a token sent as a verb name.
    let odd = rig.http("POST", "/v1/verbs/a%0Ab%22c", Some(&token), Some("{}"));
    assert_eq!(odd.status, 403);
    let as_verb = rig.http(
        "POST",
        &format!("/v1/verbs/{token}"),
        Some(&token),
        Some("{}"),
    );
    assert_eq!(as_verb.status, 403);
    let other = rig.http("GET", "/v2/status", Some(&token), None);
    assert_eq!(other.status, 404);

    let lines = rig.lines("api.ledger");
    assert_eq!(
        lines.len(),
        8,
        "one line for each request:\n{}",
        lines.join("\n")
    );
    let expect: [&[&str]; 8] = [
        &[
            "route=pair",
            "verb=-",
            "device=-",
            "status=200",
            "outcome=ok",
        ],
        &[
            "route=verb",
            "verb=project_add",
            "device=D-1",
            "status=200",
            "outcome=ok",
        ],
        &[
            "route=verb",
            "verb=project_add",
            "device=D-1",
            "status=200",
            "outcome=replayed",
        ],
        &[
            "route=verb",
            "verb=project_list",
            "device=-",
            "status=401",
            "outcome=unauthorized",
        ],
        &[
            "route=verb",
            "verb=halt",
            "device=D-1",
            "status=403",
            "outcome=forbidden",
        ],
        &[
            "route=verb",
            "verb=? verb_len=9",
            "device=D-1",
            "status=403",
            "outcome=forbidden",
        ],
        &[
            "route=verb",
            "verb=? verb_len=64",
            "device=D-1",
            "status=403",
            "outcome=forbidden",
        ],
        &[
            "route=other",
            "verb=-",
            "device=-",
            "status=404",
            "outcome=not_found",
        ],
    ];
    for (n, (line, parts)) in lines.iter().zip(expect).enumerate() {
        // `<UTC time> api route=… verb=… device=… peer=… status=… outcome=…`
        let (time, rest) = line.split_once(' ').unwrap();
        assert!(
            time.len() == 24 && time.ends_with('Z') && time.as_bytes()[10] == b'T',
            "line {n}: {line}"
        );
        assert!(rest.starts_with("api route="), "line {n}: {line}");
        assert!(rest.contains(" peer=127.0.0.1 "), "line {n}: {line}");
        for part in parts {
            assert!(rest.contains(part), "line {n} lacks {part:?}: {line}");
        }
        assert!(!line.contains("count="), "line {n}: {line}");
    }

    // `controller.ledger` still holds the admission of the first add, once, and the pairing.
    let admissions = rig.lines("controller.ledger");
    assert_eq!(
        admissions
            .iter()
            .filter(|l| l.contains("project_add") && l.contains("admitted"))
            .count(),
        1,
        "{}",
        admissions.join("\n")
    );
    assert_eq!(
        admissions
            .iter()
            .filter(|l| l.contains("device_pair"))
            .count(),
        1,
        "{}",
        admissions.join("\n")
    );

    // No secret in the request ledger, the admission ledger or stderr.
    let mut secrets = vec![
        ("token", token.clone()),
        ("code", code_text.clone()),
        ("code secret", secret_hex.clone()),
    ];
    secrets.extend(key_lines.iter().map(|l| ("key", l.clone())));
    let log = rig.controller_stderr();
    let files = [
        ("api.ledger", rig.lines("api.ledger").join("\n")),
        ("controller.ledger", admissions.join("\n")),
        ("stderr", log),
    ];
    for (name, text) in &files {
        for (what, secret) in &secrets {
            assert!(
                !contains(text.as_bytes(), secret),
                "the {what} is in {name}:\n{text}"
            );
        }
    }
}

#[test]
fn a_peer_with_five_bad_codes_is_blocked_and_the_ledger_counts_the_block() {
    let rig = Rig::on();
    for n in 1..=5 {
        let reply = rig.pair("pf1-garbage", "macbook");
        assert_eq!(reply.status, 401, "failure {n}: {}", reply.body);
    }
    // The fifth failure's line says when the block ends.
    let lines = rig.lines("api.ledger");
    assert_eq!(lines.len(), 5, "{}", lines.join("\n"));
    assert!(!lines[3].contains("blocked_until="), "{}", lines[3]);
    assert!(lines[4].contains("blocked_until=20"), "{}", lines[4]);
    // The next request, with a valid code, is refused with 429 before the code is looked at.
    let code_text = rig.code();
    let blocked = rig.pair(&code_text, "macbook");
    assert_eq!(blocked.status, 429, "{}", blocked.body);
    assert!(
        blocked.json()["error"]
            .as_str()
            .unwrap()
            .starts_with("too many failed attempts from this address: try again after "),
        "{}",
        blocked.body
    );
    // A flood adds no line inside the minute.
    for _ in 0..10 {
        assert_eq!(rig.pair(&code_text, "macbook").status, 429);
    }
    let lines = rig.lines("api.ledger");
    assert_eq!(lines.len(), 6, "{}", lines.join("\n"));
    assert!(
        lines[5].contains("outcome=blocked count=1") && lines[5].contains("status=429"),
        "{}",
        lines[5]
    );
    // The blocked requests used up no code and paired no device.
    let devices = rig.cli(&["device", "list", "--json"]);
    let v: Value = serde_json::from_str(&stdout(&devices)).unwrap();
    assert_eq!(v["devices"].as_array().unwrap().len(), 0, "{v}");
}

// ------------------------------------------------------------------ the idle expiry

/// A controller in the test process, for the case that needs a `Config` seam.
struct InProc {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl InProc {
    fn start(sb: &Sandbox, tweak: impl FnOnce(&mut Config)) -> InProc {
        pf_ipc::prepare_state_dir(&sb.state).unwrap();
        let bound = pf_ipc::bind(&sb.state).unwrap();
        let mut cfg = Config::new(sb.state.clone(), 3_600_000, BIN.into());
        tweak(&mut cfg);
        let controller = Controller::open(cfg, Arc::new(LivePorts)).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let served = controller.clone();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let _api = api::spawn_api(&served);
                pf_ipc::serve(bound, served, async {
                    let _ = rx.await;
                })
                .await;
            });
        });
        InProc {
            stop: Some(tx),
            thread: Some(thread),
        }
    }
}

impl Drop for InProc {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn a_device_with_no_call_for_the_idle_time_is_refused_and_must_pair_again() {
    let rig = Rig::new();
    let addr = rig.addr.clone();
    let live = InProc::start(&rig.sb, |cfg| {
        cfg.api_listen = Some(addr);
        cfg.device_idle = Duration::from_secs(3);
    });
    rig.wait_state("listening");

    let token = {
        let reply = rig.pair(&rig.code(), "macbook");
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["token"].as_str().unwrap().to_string()
    };
    // A call right after the pairing works.
    let reply = rig.http("POST", "/v1/verbs/project_list", Some(&token), Some("{}"));
    assert_eq!(reply.status, 200, "{}", reply.body);
    // After 5 seconds of no use the device is idle: 401, with the one text.
    std::thread::sleep(Duration::from_secs(5));
    let reply = rig.http("POST", "/v1/verbs/project_list", Some(&token), Some("{}"));
    assert_eq!(reply.status, 401, "{}", reply.body);
    assert_eq!(reply.json()["error"], "not authorized");
    // The refusal did not touch the device, so it stays idle.
    let reply = rig.http("GET", "/v1/status", Some(&token), None);
    assert_eq!(reply.status, 401, "{}", reply.body);

    // The device is still listed, and it is not revoked.
    let out = rig.cli(&["device", "list", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let list: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let devices = list["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1, "{list}");
    assert_eq!(devices[0]["id"], "D-1");
    assert_eq!(devices[0]["revoked_at"], Value::Null, "{list}");

    // A device paired now is served.
    let fresh = rig.pair(&rig.code(), "macbook-again");
    assert_eq!(fresh.status, 200, "{}", fresh.body);
    assert_eq!(fresh.json()["device"]["id"], "D-2");
    let fresh_token = fresh.json()["token"].as_str().unwrap().to_string();
    let reply = rig.http("GET", "/v1/status", Some(&fresh_token), None);
    assert_eq!(reply.status, 200, "{}", reply.body);
    // The old token stays refused.
    let reply = rig.http("GET", "/v1/status", Some(&token), None);
    assert_eq!(reply.status, 401, "{}", reply.body);
    drop(live);
}
