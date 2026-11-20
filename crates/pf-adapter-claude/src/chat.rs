//! `ClaudeChat`: the chat port over a managed copy of Claude Code's CLI, headless on `stream-json`
//! (ADR-0014). One live process per chat, and the process runs in a session host outside the
//! controller (ADR-0001's amendment of 2026-11-16, r2.s2.w5).
//!
//! The adapter never starts a process. It asks the controller's host service for a pipes host
//! ([`SessionHosts`]), reads the child's output through the host's cursor, and writes the child's
//! input through the host. One thread reads host events, splits the bytes into lines and feeds the
//! decoder; each event it gives carries the cursor after its line, and the last event of a line
//! after which nothing is in flight carries a checkpoint of the decoder. The controller commits
//! the event, then acknowledges the cursor ([`ChatPort::ack`]), so the host keeps every byte a
//! restart could need. After a controller restart [`ChatPort::rebind`] attaches to the same host
//! and reads again from the acknowledged cursor, with the decoder built from the checkpoint.
//!
//! The adapter has no timer and decides no release: the controller does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use pf_engine::port::agent::SessionRef;
use pf_engine::port::chat::{
    BackgroundTask, ChatAnswer, ChatDelivery, ChatError, ChatEvent, ChatHost, ChatId, ChatOpen,
    ChatOpened, ChatOption, ChatPort, ChatRebind, ChatResumePoint, ContextUsage, InputKind,
    MidTurn, PendingKind,
};
use pf_engine::port::host::{
    HostEvent, HostExit, HostHandle, HostLaunch, HostSignal, HostTransport, Rebind, SessionHosts,
};
use pf_engine::status::one_line_reason;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::decode::{Decoder, Out, RequestKind, error_response};
use crate::launch;
use crate::pinned::{self, Pin};
use crate::runtime::Resolver;
use crate::sha256::hash_file;
use crate::version::{ARTIFACTS_SUPPORTED, MID_TURN};

/// The runtime id this adapter answers to. w2 seeds the built-in Orchestrator with it, and w3 checks
/// an agent's runtime against it before a launch.
pub const CLAUDE_CODE_RUNTIME: &str = "claude-code";

/// How long one wait for host events lasts. A detach or a stop reaches the reader within it.
const HOST_POLL: Duration = Duration::from_millis(100);

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
    tx: UnboundedSender<ChatDelivery>,
    decoder: Decoder,
    /// Control responses not yet taken, by request id.
    responses: BTreeMap<String, Value>,
    /// Set when the host reported the child's end, or when the connection to the host ended.
    exit: Option<HostExit>,
    exited_sent: bool,
    exit_acked: bool,
    stopping: bool,
    detached: bool,
    reader_done: bool,
    violation: Option<String>,
    next_request: u32,
    /// True until `initialize` is answered. A `result` before that is the CLI refusing to start
    /// (case 15: a resume of an id with no transcript).
    init_pending: bool,
    early_error: Option<String>,
    /// A line that ends at or below this cursor is in the store already (a re-read after a
    /// restart). It is 0 for a chat that was just opened.
    committed: u64,
    /// The ids of the requests the store holds as pending at a rebind.
    pending: BTreeSet<String>,
}

struct Live {
    handle: HostHandle,
    /// Part of every control request id, so a response a restart left behind in the host's output
    /// never answers a new request. Empty for a chat that was just opened.
    nonce: String,
    state: Mutex<LiveState>,
    wake: Condvar,
}

impl Live {
    fn lock(&self) -> MutexGuard<'_, LiveState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pid(&self) -> u32 {
        self.handle.child_pid
    }

    fn write(&self, value: &Value) -> Result<(), ChatError> {
        let mut line = serde_json::to_string(value)
            .map_err(|e| ChatError::Failed(format!("cannot encode a message: {e}")))?;
        line.push('\n');
        if self.lock().exit.is_some() {
            return Err(ChatError::Failed(
                "the chat process takes no more input".into(),
            ));
        }
        self.handle
            .input(line.as_bytes())
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

/// Why the chat ended, in words: the violation, `stopped`, or how the process ended.
fn exit_reason(st: &mut LiveState, exit: &HostExit) -> String {
    if let Some(v) = st.violation.take() {
        one_line_reason(&v)
    } else if st.stopping {
        "stopped".to_string()
    } else if let Some(code) = exit.code {
        format!("process exited (code {code})")
    } else if let Some(signal) = exit.signal {
        format!("process exited (signal {signal})")
    } else {
        "process exited".to_string()
    }
}

/// Send `Exited` once, the last event. `at` is the exit's cursor, when the host gave one.
fn finish(live: &Live, exit: HostExit, at: Option<u64>, reason: Option<String>) {
    let mut st = live.lock();
    if st.exited_sent {
        return;
    }
    let reason = reason.unwrap_or_else(|| exit_reason(&mut st, &exit));
    let code = exit.code;
    st.exit = Some(exit);
    let _ = st.tx.send(ChatDelivery {
        event: ChatEvent::Exited { code, reason },
        at,
        checkpoint: None,
    });
    st.exited_sent = true;
    live.wake.notify_all();
}

/// The reader thread: read host events from cursor `from`, split the bytes into lines, feed each
/// line to the decoder and push what it gives. It ends at the child's exit, when the host's
/// connection ends, or at a detach.
fn reader(live: Arc<Live>, from: u64) {
    let mut cursor = from;
    if let Err(e) = live.handle.read(from) {
        let why = format!("the session host does not stream its output: {e}");
        finish(
            &live,
            HostExit {
                code: None,
                signal: None,
                at: cursor,
            },
            None,
            Some(why),
        );
        mark_reader_done(&live);
        return;
    }
    // The bytes of a line not finished yet, and the cursor of the first one.
    let mut buf: Vec<u8> = Vec::new();
    let mut buf_start = from;
    // After a gap: drop bytes up to the next newline, because the line they belong to is cut.
    let mut skipping = false;
    'outer: loop {
        if live.lock().detached {
            break;
        }
        let events = match live.handle.events(HOST_POLL) {
            Ok(events) => events,
            Err(e) => {
                let stopping = live.lock().stopping;
                let reason = if stopping {
                    "stopped".to_string()
                } else {
                    format!("the session host connection ended: {e}")
                };
                finish(
                    &live,
                    HostExit {
                        code: None,
                        signal: None,
                        at: cursor,
                    },
                    None,
                    Some(reason),
                );
                break;
            }
        };
        for event in events {
            match event {
                HostEvent::Out { at, data } => {
                    if !buf.is_empty() && at != buf_start + buf.len() as u64 {
                        // A jump the host did not name as a gap: the line in progress is cut.
                        buf.clear();
                        skipping = true;
                    }
                    if buf.is_empty() {
                        buf_start = at;
                    }
                    cursor = at + data.len() as u64;
                    buf.extend_from_slice(&data);
                    if skipping {
                        match buf.iter().position(|b| *b == b'\n') {
                            Some(i) => {
                                buf.drain(..=i);
                                buf_start += i as u64 + 1;
                                skipping = false;
                            }
                            None => {
                                buf_start += buf.len() as u64;
                                buf.clear();
                                continue;
                            }
                        }
                    }
                    while let Some(i) = buf.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=i).collect();
                        buf_start += line.len() as u64;
                        process_line(&live, &line[..line.len() - 1], buf_start);
                    }
                }
                HostEvent::Gap { from, to } => {
                    cursor = cursor.max(to);
                    buf.clear();
                    skipping = true;
                    buf_start = to;
                    let mut st = live.lock();
                    st.decoder.after_gap();
                    // The part of the stretch the store has not consumed is lost for good.
                    let lost_from = from.max(st.committed);
                    if to > lost_from {
                        let _ = st.tx.send(ChatDelivery {
                            event: ChatEvent::OutputLost {
                                from: lost_from,
                                to,
                            },
                            at: Some(to),
                            checkpoint: None,
                        });
                    }
                }
                HostEvent::Exit(exit) => {
                    // The bytes after the last newline are a last line with no end.
                    if !buf.is_empty() && !skipping {
                        let end = buf_start + buf.len() as u64;
                        let line = std::mem::take(&mut buf);
                        process_line(&live, &line, end);
                    }
                    let at = exit.at;
                    finish(&live, exit, Some(at), None);
                    break 'outer;
                }
            }
        }
    }
    mark_reader_done(&live);
}

fn mark_reader_done(live: &Live) {
    live.lock().reader_done = true;
    live.wake.notify_all();
}

/// Feed one output line (without its newline) that ends at cursor `end`.
fn process_line(live: &Live, line: &[u8], end: u64) {
    let Ok(msg) = serde_json::from_slice::<Value>(line) else {
        eprintln!(
            "pf-adapter-claude: a stdout line that is not JSON was skipped: {}",
            one_line_reason(&String::from_utf8_lossy(&line[..line.len().min(120)]))
        );
        return;
    };
    let mut writes = Vec::new();
    let mut kill = false;
    {
        let mut st = live.lock();
        let reread = end <= st.committed;
        if msg["type"] == "control_response" {
            // A response of an earlier life of the controller answers nobody now.
            if !reread && let Some(id) = msg["response"]["request_id"].as_str() {
                st.responses.insert(id.to_string(), msg["response"].clone());
            }
            live.wake.notify_all();
            return;
        }
        if msg["type"] == "result" && st.init_pending {
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
            return;
        }
        st.decoder.set_reread(reread);
        let mut events: Vec<ChatEvent> = Vec::new();
        for out in st.decoder.feed(&msg) {
            match out {
                Out::Event(e) => events.push(e),
                // A control response or a kill for a line the store holds was sent in the life
                // before the restart: never twice.
                Out::Respond { request_id, error } if !reread => {
                    writes.push(error_response(&request_id, &error));
                }
                Out::Respond { .. } => {}
                Out::Violation(why) if !reread => {
                    if st.violation.is_none() {
                        st.violation = Some(why);
                    }
                    kill = true;
                }
                Out::Violation(_) => {}
                Out::Log(l) => eprintln!("pf-adapter-claude: {l}"),
            }
        }
        if reread {
            // A request re-read below `committed` that the store no longer holds as pending was
            // answered or closed before the restart: it stays closed.
            for event in &events {
                if let ChatEvent::ApprovalRequest { request_id, .. }
                | ChatEvent::Question { request_id, .. } = event
                    && !st.pending.contains(request_id)
                {
                    st.decoder.close_request(request_id);
                }
            }
        }
        // `at` is the cursor after the line for the last event. An earlier event of the same line
        // takes a smaller one, one byte apart, so the store can commit the events of a line one at
        // a time and a restart between two of them still stores each event once.
        let quiet = st.decoder.is_quiet();
        let n = events.len();
        for (i, event) in events.into_iter().enumerate() {
            let last = i + 1 == n;
            let checkpoint = (last && quiet).then(|| st.decoder.checkpoint());
            let at = end.saturating_sub((n - 1 - i) as u64);
            let _ = st.tx.send(ChatDelivery {
                event,
                at: Some(at),
                checkpoint,
            });
        }
    }
    for w in writes {
        let _ = live.write(&w);
    }
    if kill {
        let _ = live.handle.signal(HostSignal::Kill);
    }
}

/// The chat adapter. See the module doc.
pub struct ClaudeChat {
    cfg: ClaudeConfig,
    resolver: Resolver,
    hosts: Mutex<Option<Arc<dyn SessionHosts>>>,
    chats: Mutex<BTreeMap<ChatId, Arc<Live>>>,
    next_chat: AtomicU32,
}

impl ClaudeChat {
    pub fn new(cfg: ClaudeConfig) -> ClaudeChat {
        ClaudeChat {
            cfg,
            resolver: Resolver::default(),
            hosts: Mutex::new(None),
            chats: Mutex::new(BTreeMap::new()),
            next_chat: AtomicU32::new(0),
        }
    }

    /// How many times the managed copy was hashed. A test reads it.
    pub fn hash_count(&self) -> usize {
        self.resolver.hash_count()
    }

    fn hosts(&self) -> Result<Arc<dyn SessionHosts>, ChatError> {
        self.hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| {
                ChatError::Failed(
                    "the chat adapter has no session host service: the controller gives it one at start"
                        .into(),
                )
            })
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

    fn register(&self, live: Arc<Live>) -> ChatId {
        let chat = ChatId(format!(
            "claude-chat-{}",
            self.next_chat.fetch_add(1, Ordering::SeqCst) + 1
        ));
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(chat.clone(), live);
        chat
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
            format!("pf-req-{}{}", live.nonce, st.next_request)
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

    /// The binary to run, its version, whether it is the qualified copy, and its SHA-256.
    ///
    /// A resume with a recorded copy runs that copy, or nothing: the file must exist and hash to
    /// the recorded SHA-256 (G8). It never starts another copy, and it is no lost context.
    fn binary(&self, spec: &ChatOpen) -> Result<(PathBuf, String, bool, String), ChatError> {
        if let (Some(rec), true) = (&spec.recorded_binary, spec.resume.is_some()) {
            let gone = |why: &str| {
                ChatError::RuntimeNotReady(format!(
                    "the CLI copy this chat was launched with is gone or changed: {} (sha256 {}); end the session and start a new one, or put that copy back{why}",
                    rec.path, rec.sha256
                ))
            };
            let path = PathBuf::from(&rec.path);
            if !path.is_file() {
                return Err(gone(""));
            }
            let actual = hash_file(&path).map_err(|e| gone(&format!(" ({e})")))?;
            if actual != rec.sha256 {
                return Err(gone(&format!(" (the file has sha256 {actual} now)")));
            }
            let qualified = pinned::host_platform()
                .and_then(|k| self.cfg.pin.platforms.get(k))
                .is_some_and(|(sha, _)| *sha == rec.sha256)
                && rec.version == self.cfg.pin.cli_version;
            return Ok((path, rec.version.clone(), qualified, actual));
        }
        let runtime = self.resolver.resolve(
            &self.cfg.share_dir,
            &self.cfg.pin,
            self.cfg.claude_bin_override.as_deref(),
        )?;
        let sha = if runtime.qualified {
            // The resolver checked the managed copy against the pin: the pin's hash is the copy's.
            pinned::host_platform()
                .and_then(|k| self.cfg.pin.platforms.get(k))
                .map(|(sha, _)| sha.clone())
                .unwrap_or_default()
        } else {
            hash_file(&runtime.bin).map_err(|e| {
                ChatError::Failed(format!("cannot hash {}: {e}", runtime.bin.display()))
            })?
        };
        Ok((runtime.bin, runtime.version, runtime.qualified, sha))
    }
}

fn request_kind(kind: PendingKind) -> RequestKind {
    match kind {
        PendingKind::Approval => RequestKind::Approval,
        PendingKind::Question => RequestKind::Question,
    }
}

/// A nonce for the control request ids of one attach: the time in milliseconds, in hex, and a dash.
fn nonce() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("{ms:x}-")
}

type Fresh = (
    Arc<Live>,
    tokio::sync::mpsc::UnboundedReceiver<ChatDelivery>,
);

fn new_live(handle: HostHandle, decoder: Decoder, nonce: String, init: bool) -> Fresh {
    let (tx, events) = tokio::sync::mpsc::unbounded_channel();
    let live = Arc::new(Live {
        handle,
        nonce,
        state: Mutex::new(LiveState {
            tx,
            decoder,
            responses: BTreeMap::new(),
            exit: None,
            exited_sent: false,
            exit_acked: false,
            stopping: false,
            detached: false,
            reader_done: false,
            violation: None,
            next_request: 0,
            init_pending: init,
            early_error: None,
            committed: 0,
            pending: BTreeSet::new(),
        }),
        wake: Condvar::new(),
    });
    (live, events)
}

impl ChatPort for ClaudeChat {
    fn options(&self) -> Vec<ChatOption> {
        launch::options()
    }

    fn mid_turn(&self) -> MidTurn {
        MID_TURN
    }

    fn set_hosts(&self, hosts: Arc<dyn SessionHosts>) {
        *self.hosts.lock().unwrap_or_else(|e| e.into_inner()) = Some(hosts);
    }

    fn open(&self, spec: &ChatOpen) -> Result<ChatOpened, ChatError> {
        let hosts = self.hosts()?;
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
        // The names of both maps pass the same checks. The secret values travel apart, so the
        // launch record never holds them.
        let mut named = spec.env.clone();
        for (name, value) in &spec.secret_env {
            named.insert(name.clone(), value.clone());
        }
        let mut env = launch::build_env(&inherited, &named, valid.artifacts)?;
        for name in spec.secret_env.keys() {
            env.remove(name);
        }
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
        let (bin, version, qualified, binary_sha256) = self.binary(spec)?;

        let mut argv = vec![bin.display().to_string()];
        argv.extend(launch::argv(spec, &session));
        let launch_request = HostLaunch {
            session_kind: "chat".to_string(),
            session_key: spec.chat_session.clone(),
            argv,
            cwd: spec.cwd.clone(),
            env,
            files: Vec::new(),
            secrets: spec.secret_env.clone(),
            transport: HostTransport::Pipes,
            runtime: json!({
                "id": CLAUDE_CODE_RUNTIME,
                "version": version,
                "qualified": qualified,
                "binary_path": bin.display().to_string(),
                "binary_sha256": binary_sha256,
            }),
            settings: json!({
                "model": spec.model,
                "effort": spec.effort,
                "permission_mode": spec.permission_mode,
                "options": spec.options,
                "instructions": spec.instructions,
                "chat.deny_commands": {"value": spec.deny_commands, "scope": spec.deny_from},
                // The agent's launch environment (never a secret: those travel apart). A resume
                // starts the chat with the same one.
                "launch_env": spec.env,
            }),
            run_binding: json!({
                "chat_session": spec.chat_session,
                "project_id": spec.project,
                "agent_id": spec.agent_id,
                "worktree_id": spec.worktree_id,
            }),
            resume: json!({"flag": "--resume", "id_from": "session_row"}),
        };
        let handle = hosts
            .launch(launch_request)
            .map_err(|e| ChatError::Failed(format!("cannot start the session host: {e}")))?;
        let host = ChatHost {
            unit: handle.unit.clone(),
            incarnation: handle.incarnation,
        };
        let decoder = Decoder::new(&version, &session, valid.artifacts);
        let (live, events) = new_live(handle, decoder, String::new(), true);
        {
            let live = Arc::clone(&live);
            std::thread::spawn(move || reader(live, 0));
        }

        // `initialize` first: no hooks, no MCP servers, no per-task stop. A failure after the host
        // started stops that host. Its launch record stays: a record is write-once.
        let failed = |live: &Live, why: String| {
            let _ = live.handle.stop();
            live.wait_until(Instant::now() + Duration::from_secs(5), |st| {
                st.exit.is_some()
            });
            if resuming {
                ChatError::ResumeFailed(why)
            } else {
                ChatError::Failed(why)
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
        let chat = self.register(live);
        Ok(ChatOpened {
            chat,
            session: SessionRef(session),
            runtime_version: version,
            qualified,
            events,
            host: Some(host),
        })
    }

    fn rebind(&self, unit: &str, point: &ChatResumePoint) -> ChatRebind {
        let hosts = match self.hosts() {
            Ok(h) => h,
            Err(e) => {
                return ChatRebind::Unreachable { why: e.to_string() };
            }
        };
        let handle = match hosts.rebind(unit) {
            Rebind::Survived(handle) => handle,
            Rebind::Lost { exit } => return ChatRebind::Lost { exit },
            Rebind::Unreachable { why } => return ChatRebind::Unreachable { why },
        };
        let runtime = &handle.record["runtime"];
        let version = runtime["version"].as_str().unwrap_or("").to_string();
        let qualified = runtime["qualified"].as_bool().unwrap_or(false);
        let artifacts = ARTIFACTS_SUPPORTED
            && handle.record["settings"]["options"]["artifacts"]
                .as_str()
                .is_none_or(|v| v != "off");
        let session = point.runtime_session.0.clone();
        let mut decoder = point
            .checkpoint
            .as_deref()
            .and_then(|c| Decoder::restore(&version, artifacts, c))
            .unwrap_or_else(|| Decoder::new(&version, &session, artifacts));
        // The pending requests of the store seed the open ones, so a request whose line is lost, or
        // not read again yet, can still be answered.
        let mut pending = BTreeSet::new();
        for p in &point.pending {
            let input = p
                .input
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok());
            decoder.seed_request(&p.request_id, request_kind(p.kind), input);
            pending.insert(p.request_id.clone());
        }
        let host = ChatHost {
            unit: handle.unit.clone(),
            incarnation: handle.incarnation,
        };
        let (live, events) = new_live(handle, decoder, nonce(), false);
        {
            let mut st = live.lock();
            st.committed = point.committed;
            st.pending = pending;
        }
        {
            let live = Arc::clone(&live);
            let from = point.acked;
            std::thread::spawn(move || reader(live, from));
        }
        let chat = self.register(live);
        ChatRebind::Survived(ChatOpened {
            chat,
            session: SessionRef(session),
            runtime_version: version,
            qualified,
            events,
            host: Some(host),
        })
    }

    fn ack(&self, chat: &ChatId, to: u64) -> Result<(), ChatError> {
        let live = self.known(chat)?;
        live.handle.ack(to).map_err(|e| {
            ChatError::Failed(format!("cannot acknowledge output to the host: {e}"))
        })?;
        // The exit was consumed: tell the host, so it ends at once and the service ends its row.
        let send_exit_ack = {
            let mut st = live.lock();
            let consumed = st.exited_sent && st.exit.as_ref().is_some_and(|e| to >= e.at);
            let first = consumed && !st.exit_acked && !st.stopping;
            if first {
                st.exit_acked = true;
            }
            first
        };
        if send_exit_ack {
            live.handle
                .exit_ack()
                .map_err(|e| ChatError::Failed(format!("cannot acknowledge the exit: {e}")))?;
        }
        Ok(())
    }

    fn detach(&self, chat: &ChatId) -> Result<(), ChatError> {
        let removed = self
            .chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(chat);
        let Some(live) = removed else {
            return Ok(());
        };
        live.lock().detached = true;
        live.wake.notify_all();
        // Let the reader drop its read of the host, so a later attach finds the host free.
        live.wait_until(Instant::now() + Duration::from_secs(2), |st| st.reader_done);
        Ok(())
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
            if open.cut && !matches!(answer, ChatAnswer::Deny { .. }) {
                return Err(ChatError::Refused(format!(
                    "the stored input of request {request_id} was cut when it was kept, so there is no input to hand back: deny it, or end the chat"
                )));
            }
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

    /// `Terminate` to the child's group, up to 5 s for its exit, then the host's own stop (its
    /// `SIGTERM`, 5 s, `SIGKILL`; the service commits the host's end). The host cannot close the
    /// child's input, so there is no step that does. It is safe to repeat.
    fn stop(&self, chat: &ChatId) -> Result<(), ChatError> {
        let Some(live) = self.find(chat) else {
            return Ok(());
        };
        live.lock().stopping = true;
        let _ = live.handle.signal(HostSignal::Terminate);
        live.wait_until(Instant::now() + Duration::from_secs(5), |st| {
            st.exit.is_some()
        });
        let _ = live.handle.stop();
        // The reader says `Exited` when the host's exit event or its end of connection comes.
        live.wait_until(Instant::now() + Duration::from_secs(5), |st| {
            st.exited_sent || st.reader_done
        });
        Ok(())
    }

    fn process_id(&self, chat: &ChatId) -> Option<u32> {
        let live = self.find(chat)?;
        let alive = live.lock().exit.is_none();
        alive.then_some(live.pid())
    }
}
