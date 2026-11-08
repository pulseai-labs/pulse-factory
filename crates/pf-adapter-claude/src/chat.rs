//! `ClaudeChat`: the chat port over a managed copy of Claude Code's CLI, headless on `stream-json`
//! (ADR-0014). One live process per chat. One thread reads stdout and pushes each event as it
//! happens; one keeps the tail of stderr; one reaps the process. The adapter has no timer and
//! decides no release: the controller does.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use pf_engine::port::agent::SessionRef;
use pf_engine::port::chat::{
    BackgroundTask, ChatAnswer, ChatError, ChatEvent, ChatId, ChatOpen, ChatOpened, ChatOption,
    ChatPort, ContextUsage, InputKind, MidTurn,
};
use pf_engine::status::one_line_reason;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::decode::{Decoder, Out, RequestKind, error_response};
use crate::launch;
use crate::pinned::Pin;
use crate::runtime::Resolver;
use crate::version::MID_TURN;

/// The runtime id this adapter answers to. w2 seeds the built-in Orchestrator with it, and w3 checks
/// an agent's runtime against it before a launch.
pub const CLAUDE_CODE_RUNTIME: &str = "claude-code";

/// The last bytes of stderr kept for a reason.
const STDERR_KEEP: usize = 8 * 1024;

/// How the adapter is set up.
#[derive(Clone, Debug)]
pub struct ClaudeConfig {
    /// Pulse Factory's own data directory, where the managed copy lives. The caller passes it.
    pub share_dir: PathBuf,
    /// The pinned CLI version and the SHA-256 and size of its binary, per platform. A test passes
    /// its own.
    pub pin: Pin,
    /// `None` runs the managed copy. `Some` runs any other binary, unqualified, with no check.
    pub claude_bin_override: Option<PathBuf>,
    /// How long `open` waits for `initialize`.
    pub init_seconds: u64,
    /// How long a control request waits for its response.
    pub control_seconds: u64,
    /// The transcript root for the resume check. Default `$HOME/.claude`.
    pub config_dir: PathBuf,
    /// The environment to filter. `None` reads the process's own.
    pub inherited_env: Option<BTreeMap<String, String>>,
}

impl ClaudeConfig {
    pub fn new(share_dir: impl Into<PathBuf>) -> ClaudeConfig {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
        ClaudeConfig {
            share_dir: share_dir.into(),
            pin: Pin::release(),
            claude_bin_override: None,
            init_seconds: 60,
            control_seconds: 30,
            config_dir: home.join(".claude"),
            inherited_env: None,
        }
    }
}

struct LiveState {
    tx: UnboundedSender<ChatEvent>,
    decoder: Decoder,
    /// Control responses not yet taken, by request id.
    responses: BTreeMap<String, Value>,
    /// `Some(code)` once the leader is reaped.
    exit: Option<Option<i32>>,
    exited_sent: bool,
    stopping: bool,
    violation: Option<String>,
    stderr_tail: String,
    stderr_done: bool,
    next_request: u32,
    /// True until `initialize` is answered. A `result` before that is the CLI refusing to start
    /// (case 15: a resume of an id with no transcript).
    init_pending: bool,
    early_error: Option<String>,
}

struct Live {
    pid: u32,
    stdin: Mutex<Option<ChildStdin>>,
    state: Mutex<LiveState>,
    wake: Condvar,
}

impl Live {
    fn lock(&self) -> MutexGuard<'_, LiveState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self, value: &Value) -> Result<(), ChatError> {
        let mut line = serde_json::to_string(value)
            .map_err(|e| ChatError::Failed(format!("cannot encode a message: {e}")))?;
        line.push('\n');
        let mut guard = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        let Some(stdin) = guard.as_mut() else {
            return Err(ChatError::Failed(
                "the chat process takes no more input".into(),
            ));
        };
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| ChatError::Failed(format!("cannot write to the chat process: {e}")))
    }

    /// Wait for a condition on the state, until `end`. Returns whether it held.
    fn wait_until(&self, end: Instant, mut done: impl FnMut(&mut LiveState) -> bool) -> bool {
        let mut st = self.lock();
        loop {
            if done(&mut st) {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            st = self
                .wake
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// How a wait for a control response ended.
enum Reply {
    Response(Value),
    Exited,
    Timeout,
}

fn signal_group(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .args(["-s", signal, "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn group_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-s", "0", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The chat adapter. See the module doc.
pub struct ClaudeChat {
    cfg: ClaudeConfig,
    resolver: Resolver,
    chats: Mutex<BTreeMap<ChatId, Arc<Live>>>,
    next_chat: AtomicU32,
}

impl ClaudeChat {
    pub fn new(cfg: ClaudeConfig) -> ClaudeChat {
        ClaudeChat {
            cfg,
            resolver: Resolver::default(),
            chats: Mutex::new(BTreeMap::new()),
            next_chat: AtomicU32::new(0),
        }
    }

    /// How many times the managed copy was hashed. A test reads it.
    pub fn hash_count(&self) -> usize {
        self.resolver.hash_count()
    }

    fn find(&self, chat: &ChatId) -> Option<Arc<Live>> {
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(chat)
            .cloned()
    }

    fn known(&self, chat: &ChatId) -> Result<Arc<Live>, ChatError> {
        self.find(chat)
            .ok_or_else(|| ChatError::Refused(format!("unknown chat {}", chat.0)))
    }

    fn control_timeout(&self) -> Duration {
        Duration::from_secs(self.cfg.control_seconds)
    }

    fn wait_response(&self, live: &Live, id: &str, timeout: Duration) -> Reply {
        let end = Instant::now() + timeout;
        let mut found = None;
        let ended = live.wait_until(end, |st| {
            if let Some(r) = st.responses.remove(id) {
                found = Some(r);
                return true;
            }
            st.exit.is_some() || st.early_error.is_some()
        });
        match (found, ended) {
            (Some(r), _) => Reply::Response(r),
            (None, true) => Reply::Exited,
            (None, false) => Reply::Timeout,
        }
    }

    /// Send a control request and wait for its response. A failure response is `Failed`.
    fn control(&self, live: &Live, request: Value) -> Result<Value, ChatError> {
        let id = {
            let mut st = live.lock();
            if st.exit.is_some() {
                return Err(ChatError::Failed("the chat process has ended".into()));
            }
            st.next_request += 1;
            format!("pf-req-{}", st.next_request)
        };
        live.write(&json!({"type": "control_request", "request_id": id, "request": request}))?;
        match self.wait_response(live, &id, self.control_timeout()) {
            Reply::Response(r) if r["subtype"] == "success" => Ok(r["response"].clone()),
            Reply::Response(r) => Err(ChatError::Failed(format!(
                "the chat process refused the request: {}",
                r["error"].as_str().unwrap_or("no error text")
            ))),
            Reply::Exited => Err(ChatError::Failed(
                "the chat process ended before it answered".into(),
            )),
            Reply::Timeout => Err(ChatError::Failed(format!(
                "no response to the control request in {} s",
                self.cfg.control_seconds
            ))),
        }
    }

    /// Does `<config_dir>/projects/*/<id>.jsonl` exist?
    fn transcript_exists(&self, id: &str) -> bool {
        let Ok(projects) = std::fs::read_dir(self.cfg.config_dir.join("projects")) else {
            return false;
        };
        projects
            .flatten()
            .any(|p| p.path().join(format!("{id}.jsonl")).is_file())
    }
}

fn reader(live: Arc<Live>, stdout: std::process::ChildStdout) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            eprintln!(
                "pf-adapter-claude: a stdout line that is not JSON was skipped: {}",
                one_line_reason(&line.chars().take(120).collect::<String>())
            );
            continue;
        };
        if msg["type"] == "control_response" {
            let mut st = live.lock();
            if let Some(id) = msg["response"]["request_id"].as_str() {
                st.responses.insert(id.to_string(), msg["response"].clone());
            }
            live.wake.notify_all();
            continue;
        }
        if msg["type"] == "result" {
            let mut st = live.lock();
            if st.init_pending {
                let text = msg["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ");
                let text = if text.is_empty() {
                    msg["result"]
                        .as_str()
                        .unwrap_or("an error result")
                        .to_string()
                } else {
                    text
                };
                st.early_error = Some(text);
                live.wake.notify_all();
                continue;
            }
        }
        let mut writes = Vec::new();
        let mut kill = false;
        {
            let mut st = live.lock();
            for out in st.decoder.feed(&msg) {
                match out {
                    Out::Event(e) => {
                        let _ = st.tx.send(e);
                    }
                    Out::Respond { request_id, error } => {
                        writes.push(error_response(&request_id, &error));
                    }
                    Out::Violation(why) => {
                        if st.violation.is_none() {
                            st.violation = Some(why);
                        }
                        kill = true;
                    }
                    Out::Log(l) => eprintln!("pf-adapter-claude: {l}"),
                }
            }
        }
        for w in writes {
            let _ = live.write(&w);
        }
        if kill {
            signal_group(live.pid, "KILL");
        }
    }
    // stdout ended: wait for the leader to be reaped and stderr to drain, then say so once.
    live.wait_until(Instant::now() + Duration::from_secs(30), |st| {
        st.exit.is_some()
    });
    live.wait_until(Instant::now() + Duration::from_secs(2), |st| st.stderr_done);
    let mut st = live.lock();
    let code = st.exit.flatten();
    let reason = if let Some(v) = st.violation.take() {
        one_line_reason(&v)
    } else if st.stopping {
        "stopped".to_string()
    } else if st.stderr_tail.trim().is_empty() {
        "process exited".to_string()
    } else {
        one_line_reason(&st.stderr_tail)
    };
    let _ = st.tx.send(ChatEvent::Exited { code, reason });
    st.exited_sent = true;
    live.wake.notify_all();
}

fn stderr_reader(live: Arc<Live>, pipe: std::process::ChildStderr) {
    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
        let mut st = live.lock();
        st.stderr_tail.push_str(&line);
        st.stderr_tail.push('\n');
        if st.stderr_tail.len() > STDERR_KEEP {
            let mut cut = st.stderr_tail.len() - STDERR_KEEP;
            while !st.stderr_tail.is_char_boundary(cut) {
                cut += 1;
            }
            st.stderr_tail.drain(..cut);
        }
    }
    let mut st = live.lock();
    st.stderr_done = true;
    live.wake.notify_all();
}

impl ChatPort for ClaudeChat {
    fn options(&self) -> Vec<ChatOption> {
        launch::options()
    }

    fn mid_turn(&self) -> MidTurn {
        MID_TURN
    }

    fn open(&self, spec: &ChatOpen) -> Result<ChatOpened, ChatError> {
        let valid = launch::validate(spec)?;
        if valid.degraded {
            eprintln!(
                "pf-adapter-claude: the Artifact tools are unsupported at this version: the chat starts with artifacts off although the agent stores on"
            );
        }
        let inherited = self
            .cfg
            .inherited_env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let env = launch::build_env(&inherited, &spec.env, valid.artifacts)?;
        let resuming = spec.resume.is_some();
        let session = match &spec.resume {
            Some(SessionRef(id)) => {
                if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err(ChatError::ResumeFailed(format!(
                        "{id:?} is not a session id"
                    )));
                }
                if !self.transcript_exists(id) {
                    return Err(ChatError::ResumeFailed(format!(
                        "no transcript <config_dir>/projects/*/{id}.jsonl: the runtime cannot resume this session, and the context is lost"
                    )));
                }
                id.clone()
            }
            None => launch::uuid_v4()?,
        };
        let runtime = self.resolver.resolve(
            &self.cfg.share_dir,
            &self.cfg.pin,
            self.cfg.claude_bin_override.as_deref(),
        )?;

        let mut cmd = Command::new(&runtime.bin);
        cmd.args(launch::argv(spec, &session))
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = crate::runtime::spawn_retry(&mut cmd).map_err(|e| {
            ChatError::Failed(format!("cannot start {}: {e}", runtime.bin.display()))
        })?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        let live = Arc::new(Live {
            pid,
            stdin: Mutex::new(stdin),
            state: Mutex::new(LiveState {
                tx,
                decoder: Decoder::new(&runtime.version, &session, valid.artifacts),
                responses: BTreeMap::new(),
                exit: None,
                exited_sent: false,
                stopping: false,
                violation: None,
                stderr_tail: String::new(),
                stderr_done: false,
                next_request: 0,
                init_pending: true,
                early_error: None,
            }),
            wake: Condvar::new(),
        });
        {
            let live = Arc::clone(&live);
            std::thread::spawn(move || {
                let status = child.wait();
                let mut st = live.lock();
                st.exit = Some(status.ok().and_then(|s| s.code()));
                live.wake.notify_all();
            });
        }
        {
            let live = Arc::clone(&live);
            std::thread::spawn(move || stderr_reader(live, stderr));
        }
        {
            let live = Arc::clone(&live);
            std::thread::spawn(move || reader(live, stdout));
        }

        // `initialize` first: no hooks, no MCP servers, no per-task stop.
        let failed = |live: &Live, why: String| {
            signal_group(live.pid, "KILL");
            live.wait_until(Instant::now() + Duration::from_secs(5), |st| {
                st.exit.is_some()
            });
            live.wait_until(Instant::now() + Duration::from_secs(2), |st| st.stderr_done);
            let tail = one_line_reason(&live.lock().stderr_tail);
            let text = format!("{why}: {tail}");
            if resuming {
                ChatError::ResumeFailed(text)
            } else {
                ChatError::Failed(text)
            }
        };
        let init_id = "pf-init";
        live.write(&json!({"type": "control_request", "request_id": init_id, "request": {"subtype": "initialize"}}))
            .map_err(|e| failed(&live, e.to_string()))?;
        match self.wait_response(&live, init_id, Duration::from_secs(self.cfg.init_seconds)) {
            Reply::Response(r) if r["subtype"] == "success" => {}
            Reply::Response(r) => {
                return Err(failed(
                    &live,
                    format!(
                        "initialize was refused: {}",
                        r["error"].as_str().unwrap_or("no error text")
                    ),
                ));
            }
            Reply::Exited => {
                // The leader ended first, or the CLI wrote an error result and refused to start.
                let why = match live.lock().early_error.clone() {
                    Some(e) => format!("the chat process refused to start: {e}"),
                    None => "the chat process ended before it answered initialize".into(),
                };
                return Err(failed(&live, why));
            }
            Reply::Timeout => {
                return Err(failed(
                    &live,
                    format!("initialize was not answered in {} s", self.cfg.init_seconds),
                ));
            }
        }

        live.lock().init_pending = false;
        let chat = ChatId(format!(
            "claude-chat-{}",
            self.next_chat.fetch_add(1, Ordering::SeqCst) + 1
        ));
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(chat.clone(), live);
        Ok(ChatOpened {
            chat,
            session: SessionRef(session),
            runtime_version: runtime.version,
            qualified: runtime.qualified,
            events,
        })
    }

    fn send(&self, chat: &ChatId, kind: InputKind, text: &str) -> Result<(), ChatError> {
        let live = self.known(chat)?;
        let uuid = launch::uuid_v4()?;
        {
            let mut st = live.lock();
            if st.exit.is_some() || st.stopping {
                return Err(ChatError::Refused("the chat has ended".into()));
            }
            st.decoder.note_send(&uuid, kind);
        }
        live.write(&json!({
            "type": "user",
            "message": {"role": "user", "content": text},
            "parent_tool_use_id": null,
            "uuid": uuid,
        }))
    }

    fn answer(
        &self,
        chat: &ChatId,
        request_id: &str,
        answer: &ChatAnswer,
    ) -> Result<(), ChatError> {
        let live = self.known(chat)?;
        let response = {
            let mut st = live.lock();
            let Some(open) = st.decoder.open_request(request_id) else {
                return Err(ChatError::Refused(format!(
                    "no open request {request_id}: it is unknown, answered or closed"
                )));
            };
            let body = match (open.kind, answer) {
                (RequestKind::Approval, ChatAnswer::Allow) => {
                    json!({"behavior": "allow", "updatedInput": open.input})
                }
                (RequestKind::Approval, ChatAnswer::Deny { message }) => {
                    json!({"behavior": "deny", "message": message})
                }
                (RequestKind::Question, ChatAnswer::Answers(answers)) => {
                    let mut input = open.input.clone();
                    if !input.is_object() {
                        input = json!({});
                    }
                    input["answers"] = json!(answers);
                    json!({"behavior": "allow", "updatedInput": input})
                }
                (RequestKind::Approval, ChatAnswer::Answers(_)) => {
                    return Err(ChatError::Refused(format!(
                        "request {request_id} is an approval: answer it with Allow or Deny"
                    )));
                }
                (RequestKind::Question, _) => {
                    return Err(ChatError::Refused(format!(
                        "request {request_id} is a question: answer it with Answers"
                    )));
                }
            };
            st.decoder.close_request(request_id);
            body
        };
        live.write(&json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request_id, "response": response
        }}))
    }

    fn interrupt(&self, chat: &ChatId) -> Result<(), ChatError> {
        let live = self.known(chat)?;
        if !live.lock().decoder.in_turn() {
            return Ok(());
        }
        self.control(&live, json!({"subtype": "interrupt"}))
            .map(|_| ())
    }

    fn set_option(&self, chat: &ChatId, id: &str, value: &str) -> Result<(), ChatError> {
        let live = self.known(chat)?;
        let options = launch::options();
        let Some(option) = options.iter().find(|o| o.id == id) else {
            return Err(ChatError::Refused(format!("unknown option {id}")));
        };
        if !option.live {
            return Err(ChatError::Refused(format!(
                "option {id} is not live: it applies at the chat's next process"
            )));
        }
        let fits = match id {
            "model" => launch::model_ok(value),
            _ => option.values.iter().any(|v| v == value),
        };
        if !fits {
            return Err(ChatError::Refused(format!(
                "value {value:?} does not fit option {id}"
            )));
        }
        let request = match id {
            "model" => json!({"subtype": "set_model", "model": value}),
            "effort" => {
                json!({"subtype": "apply_flag_settings", "settings": {"effortLevel": value}})
            }
            _ => json!({"subtype": "set_permission_mode", "mode": value}),
        };
        self.control(&live, request).map(|_| ())
    }

    fn context_usage(&self, chat: &ChatId) -> Result<ContextUsage, ChatError> {
        let live = self.known(chat)?;
        let body = self.control(
            &live,
            json!({"subtype": "get_context_usage", "detail": "summary"}),
        )?;
        match (body["totalTokens"].as_u64(), body["maxTokens"].as_u64()) {
            (Some(used_tokens), Some(max_tokens)) => Ok(ContextUsage {
                used_tokens,
                max_tokens,
            }),
            _ => Err(ChatError::Failed(
                "the usage response names no totalTokens and maxTokens".into(),
            )),
        }
    }

    fn background_tasks(&self, chat: &ChatId) -> Result<Vec<BackgroundTask>, ChatError> {
        let live = self.known(chat)?;
        Ok(live.lock().decoder.tasks())
    }

    fn is_alive(&self, chat: &ChatId) -> Result<bool, ChatError> {
        Ok(self.find(chat).is_some_and(|l| l.lock().exit.is_none()))
    }

    fn stop(&self, chat: &ChatId) -> Result<(), ChatError> {
        let Some(live) = self.find(chat) else {
            return Ok(());
        };
        live.lock().stopping = true;
        // Close stdin, and give the process 5 s to end by itself.
        drop(live.stdin.lock().unwrap_or_else(|e| e.into_inner()).take());
        live.wait_until(Instant::now() + Duration::from_secs(5), |st| {
            st.exit.is_some()
        });
        // Then TERM to the group: it ends the leader if it still runs, and any child left in the
        // group. Wait 2 s, then KILL what remains, and wait for the leader.
        signal_group(live.pid, "TERM");
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end && (live.lock().exit.is_none() || group_alive(live.pid)) {
            std::thread::sleep(Duration::from_millis(25));
        }
        if live.lock().exit.is_none() || group_alive(live.pid) {
            signal_group(live.pid, "KILL");
        }
        live.wait_until(Instant::now() + Duration::from_secs(5), |st| {
            st.exit.is_some()
        });
        // The reader says `Exited` when stdout ends: after the group is gone.
        live.wait_until(Instant::now() + Duration::from_secs(5), |st| st.exited_sent);
        Ok(())
    }

    fn process_id(&self, chat: &ChatId) -> Option<u32> {
        let live = self.find(chat)?;
        let alive = live.lock().exit.is_none();
        alive.then_some(live.pid)
    }
}
