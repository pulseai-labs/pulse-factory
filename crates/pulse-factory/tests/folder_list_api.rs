//! AC-1 of `r1.s5.w5`: `folder_list` through the real network API. The controller is the built binary
//! (`pulse-factory controller`) on a loopback port, and the HTTP calls are `curl --pinnedpubkey`, as in
//! `api_guards.rs`. The binary's home folder is the sandbox root (`Sandbox::cmd` sets `HOME`). A paired
//! device gets the listing and exactly one `api.ledger` line; no token is 401; a revoked device is 401
//! at once, with its line. Nothing here reaches beyond `127.0.0.1`, and nothing spends money.

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::*;
use serde_json::{Value, json};

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

struct Http {
    status: u16,
    body: String,
}

impl Http {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// A controller process on its own loopback port, killed on drop.
struct Rig {
    sb: Sandbox,
    addr: String,
    child: Child,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Rig {
    fn on() -> Rig {
        let sb = Sandbox::new();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let child = sb
            .cmd(&["controller", "--tick-ms", "3600000", "--api-listen", &addr])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the controller");
        let rig = Rig { sb, addr, child };
        for _ in 0..400 {
            let up = pf_ipc::call(&rig.sb.state, "api_status", json!({}))
                .is_ok_and(|r| r.ok && r.result["state"] == "listening");
            if up {
                return rig;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the API never listened");
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.sb.cmd(args).output().expect("run pulse-factory")
    }

    fn spki(&self) -> String {
        pf_ipc::call(&self.sb.state, "api_status", json!({}))
            .unwrap()
            .result["key_spki_sha256"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn http(&self, path: &str, token: Option<&str>, body: &str) -> Http {
        let pin = format!("sha256//{}", b64(&unhex(&self.spki())));
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "-k", "--max-time", "20", "--pinnedpubkey", &pin])
            .args(["-X", "POST", "-w", "\n%{http_code}"])
            .args([
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                body,
            ])
            .arg(format!("https://{}{path}", self.addr));
        if let Some(t) = token {
            cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
        }
        let out = cmd.output().expect("curl is on this host");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", "0"));
        Http {
            status: status.trim().parse().unwrap_or(0),
            body: body.to_string(),
        }
    }

    /// A paired device's token.
    fn pair(&self, name: &str) -> String {
        let out = self.cli(&["device", "pair", "--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
        let body = json!({ "code": v["code"], "device_name": name }).to_string();
        let paired = self.http("/v1/pair", None, &body);
        assert_eq!(paired.status, 200, "{}", paired.body);
        paired.json()["token"].as_str().unwrap().to_string()
    }

    fn ledger(&self) -> Vec<String> {
        std::fs::read_to_string(self.sb.state.join("api.ledger"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn folder_list(&self, token: Option<&str>, args: &Value) -> Http {
        self.http("/v1/verbs/folder_list", token, &args.to_string())
    }

    /// The lines of the request ledger that name `folder_list`.
    fn folder_lines(&self) -> Vec<String> {
        self.ledger()
            .into_iter()
            .filter(|l| l.contains("verb=folder_list"))
            .collect()
    }

    /// A folder under the home folder (the sandbox root).
    fn dir(&self, rel: &str) -> PathBuf {
        let d = self.sb.root.path().join(rel);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }
}

#[test]
fn a_paired_device_gets_the_listing_and_exactly_one_api_ledger_line() {
    let rig = Rig::on();
    let work = rig.dir("work");
    rig.dir("work/alpha");
    rig.dir("work/beta");
    let token = rig.pair("macbook");
    assert!(rig.folder_lines().is_empty());

    let reply = rig.folder_list(Some(&token), &json!({ "path": work }));

    assert_eq!(reply.status, 200, "{}", reply.body);
    let list = reply.json();
    let names: Vec<&str> = list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert_eq!(list["parent"], work.parent().unwrap().to_str().unwrap());
    let lines = rig.folder_lines();
    assert_eq!(lines.len(), 1, "one line for one call: {lines:?}");
    for word in [
        "route=verb",
        "verb=folder_list",
        "device=D-1",
        "status=200",
        "outcome=ok",
    ] {
        assert!(lines[0].contains(word), "{word} in {}", lines[0]);
    }
    // The call without a path lists the home folder, and is a second call with its own line.
    let home = rig.folder_list(Some(&token), &json!({}));
    assert_eq!(home.status, 200, "{}", home.body);
    assert_eq!(
        home.json()["home"],
        std::fs::canonicalize(rig.sb.root.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(rig.folder_lines().len(), 2);
    // A read writes no controller.ledger line.
    let controller_ledger =
        std::fs::read_to_string(rig.sb.state.join("controller.ledger")).unwrap_or_default();
    assert!(
        !controller_ledger.contains("folder_list"),
        "{controller_ledger}"
    );
}

#[test]
fn a_path_outside_the_home_folder_is_refused_over_the_api_with_its_line() {
    let rig = Rig::on();
    let token = rig.pair("macbook");

    let reply = rig.folder_list(Some(&token), &json!({ "path": "/etc" }));

    assert_eq!(reply.status, 422, "{}", reply.body);
    assert_eq!(reply.json()["error"], "the path is outside the home folder");
    assert!(!reply.body.contains("passwd"), "{}", reply.body);
    let lines = rig.folder_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("status=422") && lines[0].contains("outcome=refused"),
        "{}",
        lines[0]
    );
    // The path is not in the ledger.
    assert!(!lines[0].contains("/etc"), "{}", lines[0]);
}

#[test]
fn no_token_is_401_and_the_listing_is_not_read() {
    let rig = Rig::on();
    rig.dir("work/alpha");

    let none = rig.folder_list(None, &json!({}));
    assert_eq!(none.status, 401, "{}", none.body);
    assert_eq!(none.json()["error"], "not authorized");
    assert!(!none.body.contains("alpha"), "{}", none.body);
    let wrong = rig.folder_list(Some("0".repeat(64).as_str()), &json!({}));
    assert_eq!(wrong.status, 401, "{}", wrong.body);

    let lines = rig.folder_lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    for line in &lines {
        assert!(
            line.contains("status=401") && line.contains("outcome=unauthorized"),
            "{line}"
        );
    }
}

#[test]
fn a_revoked_device_is_401_at_once_with_its_line() {
    let rig = Rig::on();
    rig.dir("work/alpha");
    let token = rig.pair("macbook");
    assert_eq!(rig.folder_list(Some(&token), &json!({})).status, 200);

    let out = rig.cli(&["device", "revoke", "D-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let after = rig.folder_list(Some(&token), &json!({}));

    assert_eq!(after.status, 401, "{}", after.body);
    assert!(!after.body.contains("alpha"), "{}", after.body);
    let lines = rig.folder_lines();
    assert_eq!(lines.len(), 2, "one line per call: {lines:?}");
    assert!(lines[0].contains("status=200"), "{}", lines[0]);
    assert!(
        lines[1].contains("status=401") && lines[1].contains("outcome=unauthorized"),
        "{}",
        lines[1]
    );
}
