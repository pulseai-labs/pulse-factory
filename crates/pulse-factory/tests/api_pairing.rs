//! AC-5 of `r1.s2.w2`: pairing, the token, the verbs over TLS and the revoke, against a real
//! controller with the API on `127.0.0.1`. The controller is the built binary (`pulse-factory
//! controller`), and the HTTP calls are `curl --pinnedpubkey`. Two cases need a seam that a binary
//! has not: a one-second code lifetime and a seat in the process list. Those run the controller in
//! the test process, through `Config`. Nothing here spends money or reaches beyond `127.0.0.1`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;

use common::*;
use pf_api::code::PairingCode;
use pulse_factory::controller::ports::LivePorts;
use pulse_factory::controller::{Config, Controller, api};
use serde_json::{Value, json};

// ------------------------------------------------------------------ helpers

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

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

fn ok_json(out: &Output) -> Value {
    assert_eq!(code(out), 0, "stderr: {}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", stdout(out)))
}

/// The answer of one `curl` call.
struct Http {
    /// curl's own exit code: 0, or 90 for a pin that does not match.
    curl: i32,
    status: u16,
    body: String,
}

impl Http {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// A controller process on a state directory, killed on drop. Its stderr goes to a file.
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

struct Rig {
    sb: Sandbox,
    port: u16,
    proc: Option<Proc>,
}

impl Rig {
    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// A rig with the controller started with the API on a free port.
    fn on() -> Rig {
        let mut rig = Rig {
            sb: Sandbox::new(),
            port: free_port(),
            proc: None,
        };
        let addr = rig.addr();
        rig.start(&["--api-listen", &addr]);
        rig.wait_state("listening");
        rig
    }

    /// Start the controller with these extra arguments. It does nothing on a tick.
    fn start(&mut self, args: &[&str]) {
        self.start_env(args, &[]);
    }

    fn start_env(&mut self, args: &[&str], env: &[(&str, &str)]) {
        let stderr = self.sb.root.path().join("controller.stderr");
        let mut all = vec!["controller", "--tick-ms", "3600000"];
        all.extend_from_slice(args);
        let mut cmd = self.sb.cmd(&all);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd
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
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("the controller did not answer");
    }

    fn stop(&mut self) {
        self.proc = None;
        // The next controller must not find the old one's lock or socket in the way.
        for _ in 0..200 {
            if pf_ipc::call(&self.sb.state, "api_status", json!({})).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    fn wait_state(&self, want: &str) {
        for _ in 0..400 {
            let state = self.state_json();
            if state["state"] == want {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
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

    fn cli_seat(&self, args: &[&str]) -> Output {
        self.sb
            .cmd(args)
            .env("PF_SEAT_TOKEN", "seat-token-value-1234")
            .output()
            .expect("run pulse-factory")
    }

    /// The SPKI hash the server says it has.
    fn spki(&self) -> String {
        self.state_json()["key_spki_sha256"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// One pairing code, as `device pair --json` prints it.
    fn code(&self) -> String {
        let v = ok_json(&self.cli(&["device", "pair", "--json"]));
        v["code"].as_str().unwrap().to_string()
    }

    /// One call over TLS, pinned to `pin_hex` (default: the server's own hash).
    fn http(&self, method: &str, path: &str, token: Option<&str>, body: Option<&str>) -> Http {
        self.http_pinned(&self.spki(), method, path, token, body)
    }

    fn http_pinned(
        &self,
        pin_hex: &str,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> Http {
        let mut cmd = Command::new("curl");
        cmd.args([
            "-sS",
            "-k",
            "--max-time",
            "20",
            "--pinnedpubkey",
            &pin(pin_hex),
        ])
        .args(["-X", method, "-w", "\n%{http_code}"])
        .arg(format!("https://{}{path}", self.addr()))
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
            curl: out.status.code().unwrap_or(-1),
            status: status.trim().parse().unwrap_or(0),
            body: body.to_string(),
        }
    }

    fn pair(&self, code: &str, name: &str) -> Http {
        let body = json!({ "code": code, "device_name": name }).to_string();
        self.http("POST", "/v1/pair", None, Some(&body))
    }

    /// Pair a device with a fresh code and return its token.
    fn paired(&self, name: &str) -> String {
        let reply = self.pair(&self.code(), name);
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()["token"].as_str().unwrap().to_string()
    }

    fn ledger(&self) -> Vec<String> {
        std::fs::read_to_string(self.sb.state.join("controller.ledger"))
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

// ------------------------------------------------------------------ off, on and waiting

#[test]
fn with_no_address_device_pair_names_the_variable_and_api_status_says_off() {
    let mut rig = Rig {
        sb: Sandbox::new(),
        port: free_port(),
        proc: None,
    };
    rig.start(&[]);

    let out = rig.cli(&["device", "pair"]);
    assert_eq!(code(&out), 1);
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("PF_API_LISTEN"), "{}", stderr(&out));

    let out = rig.cli(&["api", "status"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "off\n");
    let v = ok_json(&rig.cli(&["api", "status", "--json"]));
    assert_eq!(v["state"], "off");
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["listen"], Value::Null);
    assert_eq!(v["key_spki_sha256"], Value::Null);
    // No key was made, and nothing listens on a port.
    assert!(!rig.sb.state.join("api").exists());
    // An empty setting counts as none.
    rig.stop();
    rig.start_env(&[], &[("PF_API_LISTEN", "")]);
    assert_eq!(stdout(&rig.cli(&["api", "status"])), "off\n");
}

#[test]
fn with_the_api_on_status_names_the_address_and_the_hash_the_code_carries() {
    let rig = Rig::on();
    let out = rig.cli(&["api", "status"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some(format!("listening on {}", rig.addr()).as_str())
    );
    let hash_line = lines.next().unwrap();
    let hash = hash_line
        .strip_prefix("key spki sha256: ")
        .expect(hash_line);
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(lines.next(), None);

    let v = ok_json(&rig.cli(&["api", "status", "--json"]));
    assert_eq!(v["state"], "listening");
    assert_eq!(v["listen"], rig.addr());
    assert_eq!(v["key_spki_sha256"], hash);

    // The code carries the same hash, and the address.
    let parsed = PairingCode::decode(&rig.code()).unwrap();
    assert_eq!(pf_api::code::hex(&parsed.spki_sha256), hash);
    assert_eq!(parsed.listen, rig.addr());

    // The key file is private, the directory too, and the certificate is there.
    let dir = rig.sb.state.join("api");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir.join("key.pem")), 0o600);
    assert_eq!(mode(&dir), 0o700);
    assert!(dir.join("cert.pem").is_file());
}

#[test]
fn the_variable_sets_the_address_and_the_flag_wins_over_it() {
    let mut rig = Rig {
        sb: Sandbox::new(),
        port: free_port(),
        proc: None,
    };
    let addr = rig.addr();
    rig.start_env(&[], &[("PF_API_LISTEN", &addr)]);
    rig.wait_state("listening");
    assert_eq!(rig.state_json()["listen"], addr);
    rig.stop();

    let other = free_port();
    let other_addr = format!("127.0.0.1:{other}");
    rig.start_env(&["--api-listen", &other_addr], &[("PF_API_LISTEN", &addr)]);
    rig.wait_state("listening");
    assert_eq!(rig.state_json()["listen"], other_addr);
}

#[test]
fn an_address_not_on_this_host_waits_and_the_controller_still_serves_the_socket() {
    let mut rig = Rig {
        sb: Sandbox::new(),
        port: free_port(),
        proc: None,
    };
    // A tailnet address that is not on this host. The bind guard (`r1.s2.w5`) refuses TEST-NET-1 and
    // every other address outside loopback and the tailnet, and a 127.x address would bind: Linux
    // holds all of 127/8.
    let addr = format!("100.100.1.1:{}", rig.port);
    rig.start(&["--api-listen", &addr]);
    rig.wait_state("waiting");
    let out = rig.cli(&["api", "status"]);
    let text = stdout(&out);
    assert_eq!(
        text.lines().next(),
        Some(format!("waiting for {addr}").as_str()),
        "{text}"
    );
    assert!(
        text.contains("key spki sha256: "),
        "the key exists while it waits: {text}"
    );
    // The socket works, and so does a verb that needs the store.
    let out = rig.cli(&["project", "list"]);
    assert_eq!(stdout(&out), "no projects\n");
}

#[test]
fn a_bind_that_fails_for_another_reason_is_printed_and_the_socket_still_serves() {
    let mut rig = Rig {
        sb: Sandbox::new(),
        port: 0,
        proc: None,
    };
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    rig.port = taken.local_addr().unwrap().port();
    let addr = rig.addr();
    rig.start(&["--api-listen", &addr]);
    rig.wait_state("failed");
    let v = ok_json(&rig.cli(&["api", "status", "--json"]));
    assert_eq!(v["state"], "failed");
    assert!(v["error"].as_str().unwrap().contains(&addr), "{v}");
    let text = stdout(&rig.cli(&["api", "status"]));
    assert!(text.starts_with("failed: "), "{text}");
    assert!(
        rig.controller_stderr().contains("cannot listen"),
        "{}",
        rig.controller_stderr()
    );
    assert_eq!(stdout(&rig.cli(&["project", "list"])), "no projects\n");
}

#[test]
fn a_setting_that_is_no_address_is_printed_and_the_socket_still_serves() {
    let mut rig = Rig {
        sb: Sandbox::new(),
        port: 0,
        proc: None,
    };
    rig.start(&["--api-listen", "not-an-address"]);
    let v = ok_json(&rig.cli(&["api", "status", "--json"]));
    assert_eq!(v["state"], "failed");
    assert!(
        v["error"].as_str().unwrap().contains("not-an-address"),
        "{v}"
    );
    let out = rig.cli(&["device", "pair"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("not-an-address"), "{}", stderr(&out));
    assert!(!rig.sb.state.join("api").exists());
}

// ------------------------------------------------------------------ pairing

#[test]
fn device_pair_prints_a_pf1_code_and_its_expiry_in_text_and_in_json() {
    let rig = Rig::on();
    let out = rig.cli(&["device", "pair"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("pf1-"), "{text}");
    assert!(
        !lines[0].contains(char::is_whitespace),
        "the code is one copyable string"
    );
    assert!(lines[1].starts_with("expires "), "{text}");
    PairingCode::decode(lines[0]).expect("the printed code reads");

    let v = ok_json(&rig.cli(&["device", "pair", "--json"]));
    assert_eq!(v["schema_version"], 1);
    assert!(v["code"].as_str().unwrap().starts_with("pf1-"));
    let expires = v["expires_at"].as_str().unwrap();
    assert!(expires.ends_with('Z') && expires.contains('T'), "{expires}");
}

#[test]
fn a_pinned_pair_returns_a_token_and_d1_and_the_code_works_once() {
    let rig = Rig::on();
    let code = rig.code();
    let reply = rig.pair(&code, "macbook");
    assert_eq!((reply.curl, reply.status), (0, 200), "{}", reply.body);
    let v = reply.json();
    assert_eq!(v["schema_version"], 1);
    let token = v["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(v["device"]["id"], "D-1");
    assert_eq!(v["device"]["name"], "macbook");
    assert!(v["device"]["paired_at"].as_str().is_some());
    assert_eq!(v["device"]["revoked_at"], Value::Null);
    assert!(
        v.get("device").unwrap().get("token").is_none(),
        "the device record holds no token"
    );

    // The same code again: used up.
    let again = rig.pair(&code, "macbook");
    assert_eq!(again.status, 401, "{}", again.body);
    assert!(!again.body.contains(token));
    assert_eq!(
        ok_json(&rig.cli(&["device", "list", "--json"]))["devices"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_code_made_before_a_newer_one_is_replaced_and_answers_401() {
    let rig = Rig::on();
    let old = rig.code();
    let new = rig.code();
    assert_ne!(old, new);
    let reply = rig.pair(&old, "macbook");
    assert_eq!(reply.status, 401, "{}", reply.body);
    // The refusal of the old code did not use up the new one.
    let reply = rig.pair(&new, "macbook");
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["device"]["id"], "D-1");
}

#[test]
fn a_wrong_secret_or_a_bad_name_does_not_burn_the_live_code() {
    let rig = Rig::on();
    let code = rig.code();
    let parsed = PairingCode::decode(&code).unwrap();

    // The right key and address, a wrong secret.
    let mut wrong = parsed.clone();
    wrong.secret[0] ^= 0xff;
    assert_eq!(rig.pair(&wrong.encode(), "macbook").status, 401);
    // Another server's key.
    let mut other = parsed.clone();
    other.spki_sha256[0] ^= 0xff;
    assert_eq!(rig.pair(&other.encode(), "macbook").status, 401);
    // Garbage.
    assert_eq!(rig.pair("pf1-nonsense", "macbook").status, 401);
    // A name that breaks the rules is a 422, and the code stays live.
    assert_eq!(rig.pair(&code, "").status, 422);
    assert_eq!(rig.pair(&code, &"n".repeat(65)).status, 422);

    let reply = rig.pair(&code, "macbook");
    assert_eq!(reply.status, 200, "{}", reply.body);
}

#[test]
fn a_code_past_its_lifetime_answers_401() {
    let rig = Rig {
        sb: Sandbox::new(),
        port: free_port(),
        proc: None,
    };
    let addr = rig.addr();
    let live = InProc::start(&rig.sb, |cfg| {
        cfg.api_listen = Some(addr);
        cfg.pair_ttl = std::time::Duration::from_secs(1);
    });
    live.wait_listening(&rig.sb);
    let fresh = rig.code();
    std::thread::sleep(std::time::Duration::from_millis(1300));
    let reply = rig.pair(&fresh, "late");
    assert_eq!(reply.status, 401, "{}", reply.body);
    // A code made now, and used at once, still works.
    let reply = rig.pair(&rig.code(), "on-time");
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["device"]["id"], "D-1");
    drop(live);
}

// ------------------------------------------------------------------ the verbs over the API

#[test]
fn project_add_then_list_over_the_api_match_the_cli_and_the_ledger_holds_the_line() {
    let rig = Rig::on();
    let token = rig.paired("macbook");
    let root = rig.sb.root.path().join("work/alpha");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();

    let add = json!({ "root": root, "name": "alpha", "command_id": "api-cmd-1" }).to_string();
    let reply = rig.http("POST", "/v1/verbs/project_add", Some(&token), Some(&add));
    assert_eq!((reply.curl, reply.status), (0, 200), "{}", reply.body);
    let added = reply.json();
    assert_eq!(added["schema_version"], 1);
    assert_eq!(added["replayed"], false);
    assert_eq!(added["project"]["id"], "P-1");

    let listed = rig.http("POST", "/v1/verbs/project_list", Some(&token), Some("{}"));
    assert_eq!(listed.status, 200, "{}", listed.body);
    let cli = ok_json(&rig.cli(&["project", "list", "--json"]));
    assert_eq!(
        listed.json(),
        cli,
        "the API and the CLI print the same JSON"
    );
    assert_eq!(cli["projects"][0]["name"], "alpha");
    assert_eq!(cli["projects"][0]["root"], root.display().to_string());

    // The same command ID and content is a replay: the first record, no second project.
    let replay = rig.http("POST", "/v1/verbs/project_add", Some(&token), Some(&add));
    assert_eq!(replay.status, 200, "{}", replay.body);
    assert_eq!(replay.json()["replayed"], true);
    assert_eq!(replay.json()["project"]["id"], "P-1");

    // A refusal of the handler is a 422 with its message.
    let bad = json!({ "root": "/no/such/dir/at/all", "command_id": "api-cmd-2" }).to_string();
    let reply = rig.http("POST", "/v1/verbs/project_add", Some(&token), Some(&bad));
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(
        reply.json()["error"]
            .as_str()
            .unwrap()
            .contains("does not exist"),
        "{}",
        reply.body
    );

    // The controller ledger holds the project line, and the device line before it.
    let ledger = rig.ledger().join("\n");
    assert!(
        ledger.contains("device_pair D-1 admitted name=macbook peer=127.0.0.1"),
        "{ledger}"
    );
    assert!(
        ledger.contains("project_add P-1 admitted name=alpha"),
        "{ledger}"
    );
    assert!(ledger.contains("project_add refused reason="), "{ledger}");
}

#[test]
fn the_status_route_names_the_server_key_and_the_calling_device() {
    let rig = Rig::on();
    let token = rig.paired("macbook");
    let reply = rig.http("GET", "/v1/status", Some(&token), None);
    assert_eq!((reply.curl, reply.status), (0, 200), "{}", reply.body);
    let v = reply.json();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["key_spki_sha256"], rig.spki());
    assert_eq!(v["device"]["id"], "D-1");
    assert_eq!(v["device"]["name"], "macbook");
}

#[test]
fn verbs_off_the_allowlist_are_refused_with_403_on_the_real_controller() {
    let rig = Rig::on();
    let token = rig.paired("macbook");
    for verb in [
        "halt",
        "ack",
        "answer",
        "claim",
        "arm",
        "runs",
        "device_pair",
        "device_revoke",
        "api_status",
    ] {
        let reply = rig.http(
            "POST",
            &format!("/v1/verbs/{verb}"),
            Some(&token),
            Some("{}"),
        );
        assert_eq!(reply.status, 403, "{verb}: {}", reply.body);
        assert!(reply.json()["error"].as_str().unwrap().contains(verb));
    }
    // Nothing changed: still one device, and no live code was made.
    let devices = ok_json(&rig.cli(&["device", "list", "--json"]));
    assert_eq!(devices["devices"].as_array().unwrap().len(), 1);
}

// ------------------------------------------------------------------ unpaired, revoke and the list

#[test]
fn a_call_with_no_token_answers_401_and_a_revoke_refuses_the_next_call_at_once() {
    let rig = Rig::on();
    let token = rig.paired("macbook");

    let none = rig.http("POST", "/v1/verbs/project_list", None, Some("{}"));
    assert_eq!(none.status, 401, "{}", none.body);
    let wrong = rig.http(
        "POST",
        "/v1/verbs/project_list",
        Some(&"0".repeat(64)),
        Some("{}"),
    );
    assert_eq!(wrong.status, 401);
    assert_eq!(none.body, wrong.body, "the answer never tells which reason");
    assert_eq!(
        rig.http("POST", "/v1/verbs/project_list", Some(&token), Some("{}"))
            .status,
        200
    );

    let out = rig.cli(&["device", "revoke", "D-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "device D-1 revoked: macbook\n");

    // The next call, with no wait.
    let after = rig.http("POST", "/v1/verbs/project_list", Some(&token), Some("{}"));
    assert_eq!(after.status, 401, "{}", after.body);
    assert_eq!(
        rig.http("GET", "/v1/status", Some(&token), None).status,
        401
    );

    // A second revoke and an unknown id are refusals.
    let out = rig.cli(&["device", "revoke", "D-1"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("revoked"), "{}", stderr(&out));
    let out = rig.cli(&["device", "revoke", "D-9"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("D-9"), "{}", stderr(&out));
    // A revoked device pairs again only with a new code, as a new device.
    let again = rig.pair(&rig.code(), "macbook");
    assert_eq!(again.json()["device"]["id"], "D-2");
}

#[test]
fn device_list_shows_each_device_with_its_name_times_peer_and_revoke_state() {
    let rig = Rig::on();
    let t1 = rig.paired("macbook");
    let _t2 = rig.paired("mac mini");
    // One call makes last-seen and the peer.
    assert_eq!(
        rig.http("POST", "/v1/verbs/project_list", Some(&t1), Some("{}"))
            .status,
        200
    );
    rig.cli(&["device", "revoke", "D-1"]);

    let v = ok_json(&rig.cli(&["device", "list", "--json"]));
    assert_eq!(v["schema_version"], 1);
    let devices = v["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    // Live devices first.
    assert_eq!(devices[0]["id"], "D-2");
    assert_eq!(devices[1]["id"], "D-1");
    let d1 = &devices[1];
    assert_eq!(d1["name"], "macbook");
    assert!(d1["paired_at"].as_str().is_some_and(|t| t.ends_with('Z')));
    assert!(
        d1["last_seen_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z'))
    );
    assert_eq!(d1["last_peer"], "127.0.0.1");
    assert!(d1["revoked_at"].as_str().is_some_and(|t| t.ends_with('Z')));
    let keys: Vec<&str> = d1.as_object().unwrap().keys().map(String::as_str).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        [
            "id",
            "last_peer",
            "last_seen_at",
            "name",
            "paired_at",
            "revoked_at"
        ]
    );
    assert_eq!(
        devices[0]["last_seen_at"],
        Value::Null,
        "D-2 has made no call"
    );
    assert_eq!(devices[0]["revoked_at"], Value::Null);

    let text = stdout(&rig.cli(&["device", "list"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("D-2  mac mini  paired "), "{text}");
    assert!(lines[0].ends_with("never seen"), "{text}");
    assert!(lines[1].starts_with("D-1  macbook  paired "), "{text}");
    assert!(
        lines[1].contains("last seen ") && lines[1].contains(" from 127.0.0.1"),
        "{text}"
    );
    assert!(lines[1].contains("  revoked "), "{text}");
}

#[test]
fn with_no_device_the_list_says_so() {
    let rig = Rig::on();
    assert_eq!(stdout(&rig.cli(&["device", "list"])), "no devices\n");
    assert_eq!(
        ok_json(&rig.cli(&["device", "list", "--json"])),
        json!({ "schema_version": 1, "devices": [] })
    );
}

#[test]
fn device_revoke_with_the_same_command_id_is_a_replay() {
    let rig = Rig::on();
    rig.paired("macbook");
    let v = ok_json(&rig.cli(&["device", "revoke", "D-1", "--command-id", "rev-1", "--json"]));
    assert_eq!(v["replayed"], false);
    assert!(v["device"]["revoked_at"].as_str().is_some());
    let lines = rig.ledger().len();
    let v = ok_json(&rig.cli(&["device", "revoke", "D-1", "--command-id", "rev-1", "--json"]));
    assert_eq!(v["replayed"], true);
    assert_eq!(rig.ledger().len(), lines, "a replay writes no ledger line");
    // The same command ID for another device is a conflict.
    let out = rig.cli(&["device", "revoke", "D-2", "--command-id", "rev-1"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("rev-1"), "{}", stderr(&out));
}

// ------------------------------------------------------------------ the key

#[test]
fn a_lost_key_makes_a_new_hash_and_a_client_pinned_to_the_old_one_fails_before_it_sends() {
    let mut rig = Rig::on();
    let old = rig.spki();
    let token = rig.paired("macbook");
    assert_eq!(
        rig.http("GET", "/v1/status", Some(&token), None).status,
        200
    );

    rig.stop();
    // The key survives a restart: the same hash.
    rig.start(&["--api-listen", &rig.addr()]);
    rig.wait_state("listening");
    assert_eq!(rig.spki(), old, "a restart keeps the key");
    assert_eq!(
        rig.http("GET", "/v1/status", Some(&token), None).status,
        200,
        "and the token"
    );

    rig.stop();
    let dir = rig.sb.state.join("api");
    std::fs::remove_file(dir.join("key.pem")).unwrap();
    std::fs::remove_file(dir.join("cert.pem")).unwrap();
    rig.start(&["--api-listen", &rig.addr()]);
    rig.wait_state("listening");
    let new = rig.spki();
    assert_ne!(new, old, "a new key is a new hash");
    let out = rig.cli(&["api", "status"]);
    assert!(stdout(&out).contains(&new), "{}", stdout(&out));

    // A client pinned to the old hash fails the handshake. curl exits 90 for a pin that differs.
    let stale = rig.http_pinned(&old, "GET", "/v1/status", Some(&token), None);
    assert_eq!(
        stale.curl, 90,
        "status {} body {}",
        stale.status, stale.body
    );
    assert_eq!(stale.body, "");
    // Nothing was sent: a live code still pairs, pinned to the new hash, after a pinned-to-old try.
    let code = rig.code();
    let body = json!({ "code": code, "device_name": "macbook" }).to_string();
    let stale = rig.http_pinned(&old, "POST", "/v1/pair", None, Some(&body));
    assert_eq!(stale.curl, 90);
    let fresh = rig.http_pinned(&new, "POST", "/v1/pair", None, Some(&body));
    assert_eq!(fresh.status, 200, "{}", fresh.body);
    // The old token is still a live device of the store, and the old key is gone. The operator
    // revokes the old devices and pairs again: the CLI lists both.
    let devices = ok_json(&rig.cli(&["device", "list", "--json"]));
    assert_eq!(devices["devices"].as_array().unwrap().len(), 2);
}

#[test]
fn a_certificate_with_no_key_is_replaced_and_a_key_with_no_certificate_keeps_its_hash() {
    let mut rig = Rig::on();
    let hash = rig.spki();
    rig.stop();
    let dir = rig.sb.state.join("api");
    // The key stays, the certificate goes: a new certificate on the same key, the same hash.
    std::fs::remove_file(dir.join("cert.pem")).unwrap();
    rig.start(&["--api-listen", &rig.addr()]);
    rig.wait_state("listening");
    assert_eq!(rig.spki(), hash);
    assert!(dir.join("cert.pem").is_file());
    rig.stop();
    // The key goes, the certificate stays: a new key.
    std::fs::remove_file(dir.join("key.pem")).unwrap();
    rig.start(&["--api-listen", &rig.addr()]);
    rig.wait_state("listening");
    assert_ne!(rig.spki(), hash);
    // The new key is private.
    assert_eq!(
        std::fs::metadata(dir.join("key.pem"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    rig.stop();
    // A key file that is not a key is never replaced: the API fails and says so.
    std::fs::write(dir.join("key.pem"), "not a key").unwrap();
    rig.start(&["--api-listen", &rig.addr()]);
    let v = rig.state_json();
    assert_eq!(v["state"], "failed", "{v}");
    assert!(v["error"].as_str().unwrap().contains("key.pem"), "{v}");
    assert_eq!(
        std::fs::read_to_string(dir.join("key.pem")).unwrap(),
        "not a key"
    );
}

// ------------------------------------------------------------------ seats

#[test]
fn device_pair_and_device_revoke_under_a_seat_token_exit_1_and_change_nothing() {
    let rig = Rig::on();
    let token = rig.paired("macbook");
    let live = rig.code();
    let lines = rig.ledger().len();

    let out = rig.cli_seat(&["device", "pair"]);
    assert_eq!(code(&out), 1);
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("PF_SEAT_TOKEN"), "{}", stderr(&out));
    let out = rig.cli_seat(&["device", "revoke", "D-1"]);
    assert_eq!(code(&out), 1);
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("PF_SEAT_TOKEN"), "{}", stderr(&out));

    // The device is live, no line was written, and the live code was not replaced.
    assert_eq!(
        rig.http("GET", "/v1/status", Some(&token), None).status,
        200
    );
    assert_eq!(rig.ledger().len(), lines);
    let reply = rig.pair(&live, "second");
    assert_eq!(
        reply.status, 200,
        "the code that a refused pair left alone works: {}",
        reply.body
    );
    // The reads are open to a seat.
    assert_eq!(code(&rig.cli_seat(&["device", "list"])), 0);
    assert_eq!(code(&rig.cli_seat(&["api", "status"])), 0);
}

#[test]
fn the_controller_refuses_pair_and_revoke_from_a_process_that_holds_a_seat_token() {
    let sb = Sandbox::new();
    let addr = format!("127.0.0.1:{}", free_port());
    let live = InProc::start(&sb, |cfg| cfg.api_listen = Some(addr));
    live.wait_listening(&sb);
    // A seat is a process with PF_SEAT_TOKEN in its environment. The caller check reads it.
    let mut seat = Command::new("sleep")
        .arg("30")
        .env_clear()
        .env("PF_SEAT_TOKEN", "seat-token-value-1234")
        .spawn()
        .unwrap();
    let peer = pf_ipc::Peer {
        pid: Some(seat.id()),
    };
    use pf_ipc::Handler;
    for (verb, args) in [
        ("device_pair", json!({})),
        ("device_revoke", json!({ "id": "D-1", "command_id": "c1" })),
    ] {
        let reply = live
            .controller
            .handle_from(pf_ipc::Request::new(verb, args.clone()), peer);
        assert!(!reply.ok, "{verb} reached a seat");
        assert!(reply.error.unwrap().contains("PF_SEAT_TOKEN"), "{verb}");
        // The caller that is not a seat (this process) gets through.
        let ours = pf_ipc::Peer {
            pid: Some(std::process::id()),
        };
        let reply = live
            .controller
            .handle_from(pf_ipc::Request::new(verb, args), ours);
        if verb == "device_pair" {
            assert!(reply.ok, "{:?}", reply.error);
        }
    }
    // No pid: the controller cannot tell who calls, and it refuses.
    let reply = live.controller.handle_from(
        pf_ipc::Request::new("device_pair", json!({})),
        pf_ipc::Peer { pid: None },
    );
    assert!(!reply.ok);
    let _ = seat.kill();
    let _ = seat.wait();
}

// ------------------------------------------------------------------ secrets

/// Every file under `dir`, with its bytes.
fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path, bytes));
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
}

#[test]
fn no_token_code_or_key_is_in_a_log_a_ledger_line_or_another_verbs_output() {
    let rig = Rig::on();
    let key_pem = std::fs::read_to_string(rig.sb.state.join("api/key.pem")).unwrap();
    // The PEM body, line by line: no line of it may show anywhere else.
    let key_lines: Vec<String> = key_pem
        .lines()
        .filter(|l| !l.starts_with("-----") && !l.is_empty())
        .map(str::to_string)
        .collect();
    assert!(key_lines.iter().map(String::len).sum::<usize>() > 40);

    // `device pair` prints the code, in text and in JSON: that is where the operator copies it.
    let text_out = rig.cli(&["device", "pair"]);
    let text_code = stdout(&text_out).lines().next().unwrap().to_string();
    let code_text = rig.code();
    let secret_hex = PairingCode::decode(&code_text).unwrap().secret_hex();
    let reply = rig.pair(&code_text, "macbook");
    assert_eq!(reply.status, 200);
    let token = reply.json()["token"].as_str().unwrap().to_string();

    // Every other verb and every refused call, and the API's own answers, as outputs and errors.
    let root = rig.sb.root.path().join("work/p");
    std::fs::create_dir_all(&root).unwrap();
    let root_text = root.display().to_string();
    let mut outputs: Vec<String> = Vec::new();
    for args in [
        vec!["device", "list"],
        vec!["device", "list", "--json"],
        vec!["api", "status"],
        vec!["api", "status", "--json"],
        vec!["project", "list", "--json"],
        vec!["project", "add", &root_text],
        vec!["device", "revoke", "D-9"],
        vec!["device", "revoke", "D-1"],
        vec!["device", "revoke", "D-1"],
        vec!["project", "list"],
    ] {
        let out = rig.cli(&args);
        outputs.push(stdout(&out));
        outputs.push(stderr(&out));
    }
    for (path, token) in [
        ("/v1/verbs/project_list", Some(token.as_str())),
        ("/v1/verbs/project_list", Some(&"1".repeat(64))),
        ("/v1/verbs/halt", Some(token.as_str())),
        ("/v1/status", None),
        ("/v1/nothing", None),
    ] {
        let reply = rig.http("POST", path, token, Some("{}"));
        outputs.push(reply.body);
    }
    // A replayed code, a replaced code, a bad code: the answers.
    outputs.push(rig.pair(&code_text, "macbook").body);
    outputs.push(rig.pair(&text_code, "macbook").body);
    outputs.push(rig.pair("pf1-garbage", "macbook").body);

    let mut secrets = vec![
        ("token", token.clone()),
        ("code", code_text.clone()),
        ("code of the text form", text_code.clone()),
        ("code secret", secret_hex.clone()),
    ];
    secrets.extend(key_lines.iter().map(|l| ("key", l.clone())));
    for output in &outputs {
        for (what, secret) in &secrets {
            assert!(
                !contains(output.as_bytes(), secret),
                "the {what} is in an output: {output}"
            );
        }
    }
    // The controller's stderr.
    let log = rig.controller_stderr();
    for (what, secret) in &secrets {
        assert!(
            !contains(log.as_bytes(), secret),
            "the {what} is in the controller's stderr: {log}"
        );
    }
    // Every file the controller wrote (the store and its journal, the ledger, the certificate)
    // except the key itself.
    for (path, bytes) in files_under(&rig.sb.state) {
        if path.ends_with("api/key.pem") || path.extension().is_some_and(|e| e == "sock") {
            continue;
        }
        for (what, secret) in &secrets {
            assert!(
                !contains(&bytes, secret),
                "the {what} is in {}",
                path.display()
            );
        }
    }
    // The key is in key.pem, which is the one place it belongs.
    let on_disk = std::fs::read(rig.sb.state.join("api/key.pem")).unwrap();
    assert!(key_lines.iter().all(|l| contains(&on_disk, l)));
}

// ------------------------------------------------------------------ the in-process controller

/// A controller in the test process, for the cases that need a `Config` seam.
struct InProc {
    controller: Arc<Controller>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl InProc {
    /// Serve the socket and the API of a controller on the state directory of `sb`.
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
            controller,
            stop: Some(tx),
            thread: Some(thread),
        }
    }

    fn wait_listening(&self, sb: &Sandbox) {
        for _ in 0..400 {
            let state = pf_ipc::call(&sb.state, "api_status", json!({}))
                .map(|r| r.result["state"].clone())
                .unwrap_or(Value::Null);
            if state == "listening" {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("the API never listened");
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
