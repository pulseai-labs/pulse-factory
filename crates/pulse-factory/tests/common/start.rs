//! The rig of the start card tests (`r1.s4.w2`): a controller in the test process with a recording
//! `FakeChat`, a temp share directory that holds `workflows/paper-spine-d20` (a copy of
//! `scripts/demo/paper-d20/workflow`), a paper env, one project and one live Orchestrator session per
//! chat, and, when asked, the API on `127.0.0.1:0` with a pinned TLS client.
//!
//! A chat is a real shell whose PID the test registers in `Rec.pids`, and the CLI runs below it
//! (`claim_authority.rs::cli_below_a_shell` is the model). Nothing here starts the real `claude`,
//! spends a token or reaches a network beyond `127.0.0.1`.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pf_client::pin::{PinVerifier, client_config};
use pf_engine::port::chat::ChatId;
use pf_engine::port::store::Store as _;
use pf_ipc::{Handler, Peer, Request, Response};
use pulse_factory::controller::Config;
use pulse_factory::controller::api;
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, HandshakeError, Message, WebSocket};

use super::chat::{Rec, controller_ledger, lines, start_with, started, strict_fake};
use super::ctl::Live;
use super::{BIN, real_scripts_dir};

pub const MARKER: &str = "PULSE_FACTORY_CHAT";

/// The name the package is installed under.
pub const PACKAGE: &str = "paper-spine-d20";

/// How the chat shell starts the process that calls the controller.
#[derive(Clone, Copy, PartialEq)]
pub enum Style {
    /// A plain child.
    Direct,
    /// `( ( cmd ) & )`: reparented to the nearest subreaper.
    DoubleFork,
}

impl Style {
    fn name(self) -> &'static str {
        match self {
            Style::Direct => "direct",
            Style::DoubleFork => "dfork",
        }
    }
}

/// The inner script: it records its own parent chain, waits until the first parent is gone, then
/// runs the command with its output in files.
const INNER: &str = r#"sleep 0.3
p=$$
chain=""
while [ "$p" -gt 1 ] 2>/dev/null; do
  chain="$chain $p"
  p=$(sed 's/^.*) //' "/proc/$p/stat" | cut -d' ' -f2)
done
echo "$chain" >"$DIR/chain"
"$CMD" "$@" >"$DIR/out" 2>"$DIR/err"
echo $? >"$DIR/rc"
"#;

const OUTER: &str = r#"while [ ! -e "$GO" ]; do sleep 0.02; done
case "$STYLE" in
  direct) sh "$DIR/inner.sh" "$@" ;;
  dfork) ( ( sh "$DIR/inner.sh" "$@" ) & ) ;;
esac
i=0
while [ ! -e "$DIR/rc" ] && [ "$i" -lt 750 ]; do sleep 0.02; i=$((i+1)); done
"#;

static RUNS: AtomicUsize = AtomicUsize::new(0);

/// What a process below a chat shell did.
pub struct Ran {
    pub rc: i32,
    pub out: String,
    pub err: String,
    /// The parent chain of the process that called, from itself up to (not including) PID 1.
    pub chain: Vec<u32>,
    pub shell: u32,
}

impl Ran {
    /// The controller's answer, when the process was the helper `call_below` runs.
    pub fn response(&self) -> Value {
        let line = self
            .out
            .lines()
            .find_map(|l| l.split_once("HELPER_RESPONSE:").map(|(_, rest)| rest))
            .unwrap_or_else(|| panic!("no response from the helper: {}\n{}", self.out, self.err));
        serde_json::from_str(line).unwrap()
    }
}

/// The API end of a rig.
pub struct ApiEnd {
    pub addr: SocketAddr,
    pub pin: [u8; 32],
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ApiEnd {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// An answer of the API.
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

pub struct StartOptions {
    /// How many projects, each with one live Orchestrator session.
    pub sessions: usize,
    /// Serve the API on `127.0.0.1:0`.
    pub api: bool,
    /// Adjust the controller's config.
    pub tweak: Box<dyn FnOnce(&mut Config) + Send>,
}

impl Default for StartOptions {
    fn default() -> StartOptions {
        StartOptions {
            sessions: 1,
            api: false,
            tweak: Box::new(|_| {}),
        }
    }
}

pub struct Start {
    // Drop order: the API first, then the controller, then the share directory.
    pub api: Option<ApiEnd>,
    pub live: Live,
    pub rec: Arc<Rec>,
    _share_dir: tempfile::TempDir,
    pub share: PathBuf,
    pub projects: Vec<String>,
    pub sessions: Vec<String>,
    pub chats: Vec<ChatId>,
    pub paper: PathBuf,
    pub spine_dir: PathBuf,
    pub worktrees: PathBuf,
    pub ledger: PathBuf,
    /// A file the package's `plan_check` touches: it shows that a plan was built.
    pub marker: PathBuf,
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The paper package, the shipped `rule.sh` in its `scripts/`, with a `plan_check` that touches
/// `marker`.
pub fn install_package(share: &Path, marker: &Path) -> PathBuf {
    let src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/demo/paper-d20/workflow");
    let dest = share.join("workflows").join(PACKAGE);
    copy_dir(&src, &dest);
    std::fs::create_dir_all(dest.join("scripts")).unwrap();
    std::fs::copy(
        real_scripts_dir().join("rule.sh"),
        dest.join("scripts/rule.sh"),
    )
    .unwrap();
    let file = dest.join("workflow.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let old = "plan_check: [echo, \"1\"]";
    assert!(text.contains(old), "the paper workflow has no `{old}`");
    std::fs::write(
        &file,
        text.replace(
            old,
            &format!(
                "plan_check: [sh, -c, \"touch {}; echo 1\"]",
                marker.display()
            ),
        ),
    )
    .unwrap();
    dest
}

impl Start {
    pub fn new() -> Start {
        Start::with(StartOptions::default())
    }

    pub fn with(o: StartOptions) -> Start {
        let share_dir = tempfile::tempdir().unwrap();
        let share = std::fs::canonicalize(share_dir.path()).unwrap();
        let marker = share.join("plan-built");
        install_package(&share, &marker);
        let rec = Rec::new(strict_fake());
        let tweak = o.tweak;
        let api_on = o.api;
        let share_for_cfg = share.clone();
        let live = start_with(rec.clone(), move |cfg| {
            cfg.chat_share_dir = share_for_cfg;
            if api_on {
                cfg.api_listen = Some("127.0.0.1:0".to_string());
            }
            tweak(cfg);
        });
        let mut projects = Vec::new();
        let mut sessions = Vec::new();
        for i in 0..o.sessions {
            let (project, session) = started(&live, &format!("proj{i}"));
            projects.push(project);
            sessions.push(session);
        }
        let chats = rec.chats.lock().unwrap().clone();
        assert_eq!(chats.len(), o.sessions, "one chat for each session");
        let root = std::fs::canonicalize(live.sb.root.path()).unwrap();
        // `live.bare` is `<root>/forge.git`: the paper env is the root.
        assert!(root.join("forge.git").is_dir());
        let spine_dir = root.join("spine");
        let worktrees = root.join("worktrees");
        std::fs::create_dir_all(&spine_dir).unwrap();
        std::fs::create_dir_all(&worktrees).unwrap();
        let ledger = root.join("run-ledger.md");
        let api = o.api.then(|| serve_api(&live));
        Start {
            api,
            live,
            rec,
            _share_dir: share_dir,
            share,
            projects,
            sessions,
            chats,
            paper: root,
            spine_dir,
            worktrees,
            ledger,
            marker,
        }
    }

    /// The arguments of `propose` for a spine, with the inputs of the paper package.
    pub fn propose_args(&self, spine: &str) -> Vec<String> {
        let mut a = vec!["propose".to_string(), spine.to_string()];
        a.extend(
            [
                "--workflow",
                PACKAGE,
                "--ledger",
                self.ledger.to_str().unwrap(),
                "--paper",
                self.paper.to_str().unwrap(),
            ]
            .map(String::from),
        );
        for (k, v) in self.params() {
            a.push("--param".to_string());
            a.push(format!("{k}={v}"));
        }
        a
    }

    /// The parameters the paper package declares.
    pub fn params(&self) -> Vec<(String, String)> {
        vec![
            ("spine_branch".into(), "spine-s1".into()),
            ("spine_dir".into(), self.spine_dir.display().to_string()),
            ("worktree_root".into(), self.worktrees.display().to_string()),
            ("base_branch".into(), "main".into()),
        ]
    }

    /// The inputs of a proposal as the socket takes them, and as `arm` takes them.
    pub fn inputs(&self, spine: &str) -> Value {
        json!({
            "spine": spine,
            "workflow": PACKAGE,
            "ledger": self.ledger,
            "params": self.params().iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>(),
            "plugin_roots": [],
            "paper": self.paper,
        })
    }

    /// The arguments of the operator's `arm --dry-run --json` with the same inputs.
    pub fn arm_dry_run_args(&self, spine: &str) -> Vec<String> {
        let mut a = self.propose_args(spine);
        a[0] = "arm".to_string();
        a.push("--dry-run".to_string());
        a.push("--json".to_string());
        a
    }

    /// Run `cmd args` below the chat shell of session `n` (0-based). `registered` puts the shell's
    /// PID in `Rec.pids` as the chat's process; `marker` puts `PULSE_FACTORY_CHAT=S-<n+1>` in the
    /// shell's environment, as the adapter does for a chat. `marker_value` replaces the value.
    #[allow(clippy::too_many_arguments)]
    pub fn below(
        &self,
        n: usize,
        style: Style,
        registered: bool,
        marker: Option<&str>,
        cmd: &str,
        args: &[&str],
        extra: &[(&str, String)],
    ) -> Ran {
        let run = RUNS.fetch_add(1, Ordering::SeqCst);
        let dir: PathBuf = self.live.sb.root.path().join(format!("below-{run}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inner.sh"), INNER).unwrap();
        let go = dir.join("go");
        let mut shell = Command::new("/bin/sh");
        shell
            .arg("-c")
            .arg(OUTER)
            .arg("sh")
            .args(args)
            .env_clear()
            .env("PATH", self.live.sb.path_env())
            .env("HOME", self.live.sb.root.path())
            .env("PF_STATE_DIR", &self.live.sb.state)
            .env("PF_SHARE_DIR", &self.share)
            .env("GO", &go)
            .env("DIR", &dir)
            .env("CMD", cmd)
            .env("STYLE", style.name());
        if let Some(value) = marker {
            shell.env(MARKER, value);
        }
        for (k, v) in extra {
            shell.env(k, v);
        }
        let mut child = shell.spawn().unwrap();
        // A chat shell of `chat_shell` may hold the registration: put it back afterwards.
        let mut previous = None;
        if registered {
            previous = self
                .rec
                .pids
                .lock()
                .unwrap()
                .insert(self.chats[n].clone(), child.id());
        }
        std::fs::write(&go, "").unwrap();
        child.wait().unwrap();
        {
            let mut pids = self.rec.pids.lock().unwrap();
            match previous {
                Some(pid) => pids.insert(self.chats[n].clone(), pid),
                None => pids.remove(&self.chats[n]),
            };
        }
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .unwrap_or_else(|e| panic!("{name} of run {run}: {e}"))
        };
        Ran {
            rc: read("rc").trim().parse().unwrap(),
            out: read("out"),
            err: read("err"),
            chain: read("chain")
                .split_whitespace()
                .map(|p| p.parse().unwrap())
                .collect(),
            shell: child.id(),
        }
    }

    /// The marker value of session `n`.
    pub fn marker_of(&self, n: usize) -> String {
        self.sessions[n].clone()
    }

    /// The CLI below the chat shell of session `n`, registered, with its own marker.
    pub fn cli_below(&self, n: usize, args: &[String]) -> Ran {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.below(
            n,
            Style::Direct,
            true,
            Some(&self.marker_of(n)),
            BIN,
            &args,
            &[],
        )
    }

    /// `propose` from session `n`, as the chat's own `Bash`. It must exit 0, and answers its JSON.
    pub fn propose(&self, n: usize, spine: &str, extra: &[&str]) -> Value {
        let mut args = self.propose_args(spine);
        args.extend(extra.iter().map(|s| s.to_string()));
        args.push("--json".to_string());
        let ran = self.cli_below(n, &args);
        assert_eq!(ran.rc, 0, "propose failed: {}\n{}", ran.err, ran.out);
        serde_json::from_str(&ran.out).unwrap_or_else(|e| panic!("{e}: {}", ran.out))
    }

    /// A raw verb call below the chat shell of session `n`, by this test binary as the helper (its
    /// own `helper_calls_the_controller` test, which each test file that uses this declares).
    pub fn call_below(
        &self,
        n: usize,
        style: Style,
        registered: bool,
        marker: Option<&str>,
        verb: &str,
        args: &Value,
    ) -> Ran {
        let exe = std::env::current_exe().unwrap();
        self.below(
            n,
            style,
            registered,
            marker,
            exe.to_str().unwrap(),
            &[
                "--exact",
                "helper_calls_the_controller",
                "--nocapture",
                "--test-threads=1",
            ],
            &[
                (
                    "PF_TEST_HELPER_STATE",
                    self.live.sb.state.display().to_string(),
                ),
                ("PF_TEST_HELPER_VERB", verb.to_string()),
                ("PF_TEST_HELPER_ARGS", args.to_string()),
            ],
        )
    }

    /// The items of session `n` of one kind, as JSON bodies with their `seq`.
    pub fn bodies(&self, n: usize, kind: &str) -> Vec<(u64, Value)> {
        super::chat::items(&self.live, &self.sessions[n])
            .into_iter()
            .filter(|i| i.kind == kind)
            .map(|i| {
                let mut body: Value = serde_json::from_str(&i.payload).unwrap();
                body["answers_seq"] = json!(i.answers_seq);
                (i.seq, body)
            })
            .collect()
    }

    /// The controller ledger lines that hold `needle`.
    pub fn ledger_with(&self, needle: &str) -> Vec<String> {
        controller_ledger(&self.live)
            .into_iter()
            .filter(|l| l.contains(needle))
            .collect()
    }

    pub fn api_ledger(&self) -> Vec<String> {
        lines(&self.live.sb.state.join("api.ledger"))
    }

    /// The runs the controller knows, from the `runs` verb.
    pub fn runs(&self) -> Vec<Value> {
        let r = pf_ipc::call(&self.live.sb.state, "runs", json!({})).unwrap();
        r.result["runs"].as_array().cloned().unwrap_or_default()
    }

    // -------------------------------------------------------------- the API

    fn api_end(&self) -> &ApiEnd {
        self.api.as_ref().expect("the rig serves the API")
    }

    pub fn http(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> Answer {
        let end = self.api_end();
        http_at(end.addr, end.pin, method, path, token, body)
    }

    /// The address and the key pin of the API, for a call from a thread that holds no `Start`.
    pub fn endpoint(&self) -> (SocketAddr, [u8; 32]) {
        let end = self.api_end();
        (end.addr, end.pin)
    }

    /// One verb call as a device.
    pub fn verb(&self, token: &str, verb: &str, args: Value) -> Answer {
        self.http(
            "POST",
            &format!("/v1/verbs/{verb}"),
            Some(token),
            Some(&args.to_string()),
        )
    }

    /// Pair a device through the real route and answer its token.
    pub fn pair(&self, name: &str) -> String {
        let made = pf_ipc::call(&self.live.sb.state, "device_pair", json!({})).unwrap();
        assert!(made.ok, "{:?}", made.error);
        let body = json!({"code": made.result["code"], "device_name": name}).to_string();
        let reply = self.http("POST", "/v1/pair", None, Some(&body));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.body["token"].as_str().unwrap().to_string()
    }

    /// `confirm_code` for the arm subject. Answers the HTTP answer.
    pub fn confirm_arm(
        &self,
        token: &str,
        proposal: &str,
        plan_sha256: &str,
        command: &str,
    ) -> Answer {
        self.verb(
            token,
            "confirm_code",
            json!({
                "subject": {"kind": "arm", "proposal": proposal, "plan_sha256": plan_sha256},
                "command_id": command,
            }),
        )
    }

    /// `arm` as a device.
    pub fn arm_with(
        &self,
        token: &str,
        proposal: &str,
        plan_sha256: &str,
        code: &str,
        command: &str,
    ) -> Answer {
        self.verb(
            token,
            "arm",
            json!({
                "proposal": proposal,
                "plan_sha256": plan_sha256,
                "code": code,
                "command_id": command,
            }),
        )
    }

    /// A code for a proposal, from a device: it must be issued.
    pub fn code_for(
        &self,
        token: &str,
        proposal: &str,
        plan_sha256: &str,
        command: &str,
    ) -> String {
        let r = self.confirm_arm(token, proposal, plan_sha256, command);
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["code"].as_str().unwrap().to_string()
    }

    /// The text of every file a secret could land in: the ledgers, the stderr of nothing (the
    /// controller runs in process), the store's admission rows and `status --json`.
    pub fn secret_surface(&self) -> String {
        let state = &self.live.sb.state;
        let mut all = String::new();
        for name in ["api.ledger", "controller.ledger", "chat.ledger"] {
            all.push_str(&std::fs::read_to_string(state.join(name)).unwrap_or_default());
        }
        all.push_str(&std::fs::read_to_string(&self.ledger).unwrap_or_default());
        use pf_engine::port::store::{CONTROLLER_SCOPE, Store};
        for a in self
            .live
            .controller
            .store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
        {
            all.push_str(&a.content);
            all.push('\n');
            all.push_str(&a.reason.unwrap_or_default());
            all.push('\n');
        }
        for run in self.runs() {
            let id = run["id"].as_str().unwrap_or_default().to_string();
            let status = pf_ipc::call(state, "status", json!({"run": id})).unwrap();
            all.push_str(&status.result.to_string());
        }
        all
    }
}

/// One HTTP call over pinned TLS.
pub fn http_at(
    addr: SocketAddr,
    pin: [u8; 32],
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Answer {
    let mut stream = tls_stream(addr, pin);
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

fn serve_api(live: &Live) -> ApiEnd {
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
        pf_ipc::call(&live.sb.state, "api_status", json!({}))
            .unwrap()
            .result
    };
    let end = Instant::now() + Duration::from_secs(10);
    while status()["state"] != "listening" {
        assert!(Instant::now() < end, "the API never listened: {}", status());
        std::thread::sleep(Duration::from_millis(25));
    }
    let st = status();
    ApiEnd {
        addr: st["listen"].as_str().unwrap().parse().unwrap(),
        pin: unhex(st["key_spki_sha256"].as_str().unwrap()),
        stop: Some(tx),
        thread: Some(thread),
    }
}

/// The text of `a.ledger` lines for the controller kind, in order.
pub fn kind_lines(start: &Start, kind: &str) -> Vec<String> {
    start.ledger_with(&format!(" {kind} "))
}

/// A long-lived shell that stands for a chat process (or for a caller that is none), so a test
/// calls the controller in process with the shell's PID as the peer.
pub struct ChatShell {
    child: std::process::Child,
    pub peer: Peer,
    rec: Arc<Rec>,
    chat: Option<ChatId>,
}

impl Drop for ChatShell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(chat) = &self.chat {
            self.rec.pids.lock().unwrap().remove(chat);
        }
    }
}

impl Start {
    /// A shell with `marker` in its environment. With `registered`, its PID is the chat process of
    /// session `n`.
    pub fn chat_shell(
        &self,
        n: usize,
        registered: bool,
        marker: Option<&str>,
        env: &[(&str, &str)],
    ) -> ChatShell {
        let mut cmd = Command::new("/bin/sh");
        // `exec`: the process that `Drop` kills is the `sleep` itself, so none is left to hold a
        // descriptor (the build lock) after the test.
        cmd.args(["-c", "exec sleep 600"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        if let Some(m) = marker {
            cmd.env(MARKER, m);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().unwrap();
        let chat = registered.then(|| self.chats[n].clone());
        if let Some(chat) = &chat {
            self.rec
                .pids
                .lock()
                .unwrap()
                .insert(chat.clone(), child.id());
        }
        // The shell has its environment once `exec` is done: wait for `/proc` to show it.
        let environ = PathBuf::from("/proc")
            .join(child.id().to_string())
            .join("environ");
        let end = Instant::now() + Duration::from_secs(5);
        while marker.is_some() && !std::fs::read(&environ).is_ok_and(|e| !e.is_empty()) {
            assert!(Instant::now() < end, "the shell has no environment");
            std::thread::sleep(Duration::from_millis(10));
        }
        let peer = Peer {
            pid: Some(child.id()),
        };
        ChatShell {
            child,
            peer,
            rec: self.rec.clone(),
            chat,
        }
    }

    /// A verb call with `shell` as the peer.
    pub fn call_as(&self, shell: &ChatShell, verb: &str, args: Value) -> Response {
        self.live
            .controller
            .handle_from(Request::new(verb, args), shell.peer)
    }

    /// A `propose` call with `shell` as the peer.
    pub fn propose_as(&self, shell: &ChatShell, spine: &str, command: &str) -> Response {
        self.call_as(
            shell,
            "propose",
            json!({"inputs": self.inputs(spine), "command_id": command}),
        )
    }
}

// ------------------------------------------------------------------ the stream, claims and launches

pub type Socket = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

impl Start {
    /// Open the stream of a device. `query` is `?session=S-1&after=0` and the like.
    pub fn stream(&self, token: &str, query: &str) -> Socket {
        let end = self.api_end();
        let url = format!("wss://127.0.0.1:{}/v1/stream{query}", end.addr.port());
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        match tungstenite::client(request, tls_stream(end.addr, end.pin)) {
            Ok((socket, _)) => socket,
            Err(HandshakeError::Failure(tungstenite::Error::Http(r))) => {
                panic!("the stream was refused: {}", r.status())
            }
            Err(other) => panic!("the upgrade failed: {other:?}"),
        }
    }

    /// The claim rows of the store.
    pub fn claims(&self) -> Vec<pf_engine::port::store::ClaimRow> {
        self.live.controller.store().claims().unwrap()
    }

    /// The launch admissions of a run.
    pub fn launches(&self, run: &str) -> Vec<pf_engine::port::store::AdmissionRow> {
        self.live
            .controller
            .store()
            .admissions(run)
            .unwrap()
            .into_iter()
            .filter(|a| a.kind == "launch")
            .collect()
    }
}

/// Read frames of a stream until `done` says so. It fails with what it saw after 20 seconds, and at
/// a close.
pub fn read_until(socket: &mut Socket, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let end = Instant::now() + Duration::from_secs(20);
    let mut frames: Vec<Value> = Vec::new();
    while !done(&frames) {
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}; frames: {frames:?}"
        );
        match socket.read() {
            Ok(Message::Text(text)) => frames.push(serde_json::from_str(text.as_str()).unwrap()),
            Ok(Message::Close(c)) => panic!("closed {c:?} while waiting for {what}: {frames:?}"),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("the stream failed while waiting for {what}: {e}"),
        }
    }
    frames
}
