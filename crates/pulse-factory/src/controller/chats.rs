//! The chat runtimes in the controller (ADR-0001, ADR-0002, ADR-0015, `r1.s3.w3`).
//!
//! The controller drives a chat only through [`ChatPort`]. This module holds:
//!
//! - the server's chat settings, read by one pure function ([`parse_chat_env`]);
//! - the table of live processes, one per chat, and one event pump per process, which writes the
//!   display log and sends the frames of the stream;
//! - the lifetime rules: a process is released after an idle window, never reopened by a timer,
//!   capped by a setting, and stopped by `halt`, `session end` and a controller stop;
//! - `chat.ledger`, the audit trail of the processes and the turns, and the frames of the API stream.
//!
//! The verbs that use these live in `chat_verbs.rs`. The records and the item conversions are in
//! `chat_view.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pf_api::host::{StreamEnd, StreamEvent, StreamFeed, StreamRefusal};
use pf_api::types::{self as wire, DeviceRecord};
use pf_engine::port::agent::SessionRef;
use pf_engine::port::chat::{
    BackgroundTask, Capped, ChatError, ChatEvent, ChatId, ChatOpen, ChatOption, ChatPort,
    ContextUsage, TaskEnd, TurnCause,
};
use pf_engine::port::store::{
    AgentRow, ChatItemKind, ChatItemRow, ChatSessionRow, ChatSessionState, NewChatItem, Store,
    ToolText,
};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

use super::commands::{field, now};
use super::worktrees::WorktreeLive;
use super::{Controller, lock};

/// The chat ledger's file name in the state directory.
pub const CHAT_LEDGER_FILE: &str = "chat.ledger";

/// The cap on live chat processes when the setting is not set (205–537 MB each, measured).
pub const DEFAULT_MAX_LIVE: usize = 4;

/// How long a process may be idle before the controller releases it: 30 minutes.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(30 * 60);

/// The registry the managed copy is fetched from when `PF_CHAT_REGISTRY` is not set.
pub const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

/// The frames the stream keeps for a reader that is slow. A reader that falls further behind is
/// closed with 4002 and reconnects.
const FRAME_CAPACITY: usize = 4096;

/// How many items a reconnect reads at a time.
const ITEM_PAGE: u32 = 500;

// ------------------------------------------------------------------ the settings

/// The server's chat settings, from the controller's environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatEnv {
    /// `PF_SHARE_DIR`, else `$HOME/.local/share/pulse-factory`.
    pub share_dir: PathBuf,
    /// `PF_CHAT_CLAUDE_BIN`: run this binary, unqualified, instead of the managed copy.
    pub claude_bin: Option<PathBuf>,
    /// `PF_CHAT_MAX_LIVE`: 1 to 64, default 4.
    pub max_live: usize,
    /// `PF_AGENT_ENV_<AGENT>`: the agent's server-side environment, by the upper-cased agent key.
    pub agent_env: BTreeMap<String, BTreeMap<String, String>>,
    /// `PF_SERVER_NAME`, else the host name, else `server`.
    pub server_name: String,
    /// `PF_CHAT_REGISTRY`, default [`DEFAULT_REGISTRY`].
    pub registry: String,
}

/// The key of an agent in `PF_AGENT_ENV_<AGENT>`: the ID upper-cased, each other character `_`.
pub fn agent_env_key(agent_id: &str) -> String {
    agent_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// A server name is 1 to 64 characters of `[A-Za-z0-9._-]`.
pub fn server_name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Read the chat settings from the environment `vars`. `hostname` is the kernel's host name, when
/// it could be read. The result holds each setting, and one line for each setting that was bad and
/// fell back to its default. The caller prints those lines on stderr.
pub fn parse_chat_env(
    vars: &BTreeMap<String, String>,
    hostname: Option<&str>,
) -> (ChatEnv, Vec<String>) {
    let mut warnings = Vec::new();
    let get = |name: &str| vars.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
    let share_dir = match get("PF_SHARE_DIR") {
        Some(d) => PathBuf::from(d),
        None => get("HOME")
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join(".local/share/pulse-factory"),
    };
    let claude_bin = get("PF_CHAT_CLAUDE_BIN").map(PathBuf::from);
    let max_live = match get("PF_CHAT_MAX_LIVE") {
        None => DEFAULT_MAX_LIVE,
        Some(text) => match text.parse::<usize>() {
            Ok(n) if (1..=64).contains(&n) => n,
            _ => {
                warnings.push(format!(
                    "pulse-factory: PF_CHAT_MAX_LIVE={text:?} is not a number from 1 to 64; the cap stays {DEFAULT_MAX_LIVE}"
                ));
                DEFAULT_MAX_LIVE
            }
        },
    };
    let mut agent_env: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (name, value) in vars {
        let Some(key) = name.strip_prefix("PF_AGENT_ENV_") else {
            continue;
        };
        let mut pairs = BTreeMap::new();
        for pair in value.split_whitespace() {
            match pair.split_once('=') {
                Some((n, v)) if !n.is_empty() => {
                    pairs.insert(n.to_string(), v.to_string());
                }
                _ => warnings.push(format!(
                    "pulse-factory: {name} holds {pair:?}, which is not NAME=value; it is skipped"
                )),
            }
        }
        agent_env.insert(key.to_string(), pairs);
    }
    let from_host = hostname
        .map(str::trim)
        .filter(|h| server_name_ok(h))
        .map(str::to_string);
    let server_name = match get("PF_SERVER_NAME") {
        Some(n) if server_name_ok(n) => n.to_string(),
        Some(n) => {
            warnings.push(format!(
                "pulse-factory: PF_SERVER_NAME={n:?} is not 1 to 64 characters of A-Z a-z 0-9 . _ -; the server name is {}",
                from_host.as_deref().unwrap_or("server")
            ));
            from_host.unwrap_or_else(|| "server".to_string())
        }
        None => from_host.unwrap_or_else(|| "server".to_string()),
    };
    let registry = get("PF_CHAT_REGISTRY")
        .unwrap_or(DEFAULT_REGISTRY)
        .trim_end_matches('/')
        .to_string();
    (
        ChatEnv {
            share_dir,
            claude_bin,
            max_live,
            agent_env,
            server_name,
            registry,
        },
        warnings,
    )
}

/// The kernel's host name, when it can be read.
pub fn read_hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
}

// ------------------------------------------------------------------ the ledger

/// The chat ledger and its one writer: append-only, no `fsync` per line, not in the store. The
/// store's `turn_result` items are the durable record, and a crash can lose the last lines. A line
/// never holds a message, a tool text, a token or a key.
pub(super) struct ChatLedger {
    path: PathBuf,
    writer: Mutex<()>,
}

impl ChatLedger {
    pub(super) fn new(path: PathBuf) -> ChatLedger {
        ChatLedger {
            path,
            writer: Mutex::new(()),
        }
    }

    /// Append `<UTC> chat <rest>`. A failure prints one line on stderr and costs the chat nothing.
    pub(super) fn append(&self, rest: &str) {
        let _one = lock(&self.writer);
        let line = format!("{} chat {rest}\n", now());
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(e) = result {
            eprintln!(
                "pulse-factory: controller: the chat ledger {} could not be appended to: {e}",
                self.path.display()
            );
        }
    }
}

// ------------------------------------------------------------------ the live processes

/// One pending approval or question of a live process.
pub(super) struct OpenRequest {
    pub request_id: String,
    pub kind: ChatItemKind,
    pub since: SystemTime,
}

/// What changes while a process lives.
pub(super) struct LiveInner {
    pub turn_since: Option<SystemTime>,
    pub tasks: BTreeMap<String, (BackgroundTask, SystemTime)>,
    /// By the `seq` of the request item.
    pub requests: BTreeMap<u64, OpenRequest>,
    /// Tools started and not finished: call id to the tool and its capped input.
    pub tools: BTreeMap<String, (String, Capped)>,
    /// When the process last became idle.
    pub idle_since: SystemTime,
    pub context: Option<ContextUsage>,
    /// The runtime options the process runs with now.
    pub applied: BTreeMap<String, String>,
    pub state: ChatSessionState,
}

/// One live chat process.
pub(super) struct LiveChat {
    pub session: String,
    pub chat: ChatId,
    pub port: Arc<dyn ChatPort>,
    pub opened_at: SystemTime,
    pub version: String,
    pub qualified: bool,
    /// True once the controller asked the process to stop.
    pub stopping: AtomicBool,
    /// One writer at a time for the session's items: the pump and the verbs take it, so the order
    /// of the log is the order of what happened.
    pub io: Mutex<()>,
    pub inner: Mutex<LiveInner>,
}

/// Why a live process holds its place: a turn, a pending request, a task, or nothing.
pub(super) enum Hold {
    Turn(SystemTime),
    Answer(SystemTime),
    Task(String, SystemTime),
}

impl LiveChat {
    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// What holds the process now. Empty means idle.
    pub(super) fn holds(&self) -> Vec<Hold> {
        let inner = lock(&self.inner);
        let mut holds = Vec::new();
        if let Some(t) = inner.turn_since {
            holds.push(Hold::Turn(t));
        }
        if let Some(t) = inner.requests.values().map(|r| r.since).min() {
            holds.push(Hold::Answer(t));
        }
        for (id, (task, since)) in &inner.tasks {
            if !task.ambient {
                holds.push(Hold::Task(id.clone(), *since));
            }
        }
        holds
    }

    /// Is the process idle for the controller? The adapter's own list of tasks is a cross-check.
    pub(super) fn is_idle(&self) -> bool {
        if !self.holds().is_empty() {
            return false;
        }
        !self
            .port
            .background_tasks(&self.chat)
            .map(|tasks| tasks.iter().any(|t| !t.ambient))
            .unwrap_or(false)
    }
}

/// What the controller keeps about a session beyond the store.
#[derive(Default, Clone)]
pub(super) struct SessionMeta {
    pub last_turn_at: Option<String>,
    pub cache_ttl: Option<u32>,
}

/// The chat state of the controller.
pub(super) struct Chats {
    /// Live processes by session ID.
    pub live: Mutex<BTreeMap<String, Arc<LiveChat>>>,
    /// One open at a time, so the cap holds.
    pub open_lock: Mutex<()>,
    /// Append an item and send its frame as one step, so frames keep the order of `seq`.
    pub emit_lock: Mutex<()>,
    /// "Context lost" marks, in memory.
    pub lost: Mutex<BTreeMap<String, String>>,
    pub meta: Mutex<BTreeMap<String, SessionMeta>>,
    /// The branch of each session's worktree as the last live read saw it.
    pub branches: Mutex<BTreeMap<String, (Option<String>, bool)>>,
    pub frames: broadcast::Sender<wire::ChatFrame>,
    /// Open streams: the device and the channel that ends the stream.
    pub streams: Mutex<Vec<(String, mpsc::UnboundedSender<StreamEnd>)>>,
    pub ledger: ChatLedger,
}

impl Chats {
    pub(super) fn new(state_dir: &std::path::Path) -> Chats {
        Chats {
            live: Mutex::new(BTreeMap::new()),
            open_lock: Mutex::new(()),
            emit_lock: Mutex::new(()),
            lost: Mutex::new(BTreeMap::new()),
            meta: Mutex::new(BTreeMap::new()),
            branches: Mutex::new(BTreeMap::new()),
            frames: broadcast::channel(FRAME_CAPACITY).0,
            streams: Mutex::new(Vec::new()),
            ledger: ChatLedger::new(state_dir.join(CHAT_LEDGER_FILE)),
        }
    }
}

/// UTC text of a time.
pub(super) fn utc(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    pf_engine::engine::format_utc(d.as_secs(), d.subsec_millis())
}

/// Why an open failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenFailure {
    /// The adapter or the controller refused; the text names the fix.
    Refused(String),
    /// The runtime cannot resume the session: the context is lost.
    ContextLost(String),
    /// The open failed.
    Failed(String),
}

impl OpenFailure {
    pub fn text(&self) -> &str {
        match self {
            OpenFailure::Refused(t) | OpenFailure::ContextLost(t) | OpenFailure::Failed(t) => t,
        }
    }
}

/// A value is well formed when it is not empty, has at most 64 characters and holds no whitespace
/// or control character. It is the grammar of a free option.
fn well_formed(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Check one value against a declared option. `Err` names the declared values.
pub(super) fn check_option_value(option: &ChatOption, value: &str) -> Result<(), String> {
    if !well_formed(value) {
        return Err(format!(
            "the value {value:?} of option {} is empty, longer than 64 characters, or holds whitespace or a control character",
            option.id
        ));
    }
    if !option.free && !option.values.iter().any(|v| v == value) {
        return Err(format!(
            "the value {value:?} is not one of the values of option {}: {}",
            option.id,
            option.values.join(", ")
        ));
    }
    Ok(())
}

/// The launch check (C4, P3, N5). Model, effort and permission mode pass the port's declared
/// options. Each key of `options` is a declared option. A runtime option whose stored value the
/// port no longer offers is no refusal: the result holds one note for it, and the launch goes on.
pub(super) fn check_launch(
    port: &dyn ChatPort,
    model: &str,
    effort: &str,
    permission_mode: &str,
    options: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let declared = port.options();
    for (id, value) in [
        ("model", model),
        ("effort", effort),
        ("permission_mode", permission_mode),
    ] {
        if let Some(option) = declared.iter().find(|o| o.id == id) {
            check_option_value(option, value)?;
        }
    }
    let mut notes = Vec::new();
    for (id, value) in options {
        let Some(option) = declared.iter().find(|o| &o.id == id) else {
            let ids: Vec<&str> = declared.iter().map(|o| o.id.as_str()).collect();
            return Err(format!(
                "the option {id} is not declared by this runtime; it declares: {}",
                ids.join(", ")
            ));
        };
        if check_option_value(option, value).is_err() {
            notes.push(format!(
                "pulse-factory: the stored value {value:?} of the chat option {id} is not offered any more (the runtime offers: {}); the chat launches with the runtime's own setting",
                option.values.join(", ")
            ));
        }
    }
    Ok(notes)
}

/// The runtime options a session launches with: its own, or its agent's when its own are empty.
pub(super) fn effective_options(
    row: &ChatSessionRow,
    agent: &AgentRow,
) -> BTreeMap<String, String> {
    if row.runtime_options.is_empty() {
        agent.runtime_options.clone()
    } else {
        row.runtime_options.clone()
    }
}

// ------------------------------------------------------------------ the controller's half

impl Controller {
    /// The live process of a session, if it has one that is not being stopped.
    pub(super) fn live_chat(&self, session: &str) -> Option<Arc<LiveChat>> {
        lock(&self.chats.live)
            .get(session)
            .filter(|l| !l.is_stopping())
            .cloned()
    }

    /// Every live process, in session order.
    pub(super) fn live_chats(&self) -> Vec<Arc<LiveChat>> {
        lock(&self.chats.live).values().cloned().collect()
    }

    /// The port of a runtime, or the refusal.
    pub(super) fn chat_port(&self, runtime: &str) -> Result<Arc<dyn ChatPort>, String> {
        self.cfg
            .chat_runtimes
            .get(runtime)
            .cloned()
            .ok_or_else(|| format!("no chat runtime {runtime} on this server"))
    }

    /// The session IDs that have a live process.
    pub fn live_chat_sessions(&self) -> Vec<String> {
        lock(&self.chats.live).keys().cloned().collect()
    }

    /// Subscribe to the frames of the chat stream. A test reads the frames the controller sends.
    pub fn subscribe_frames(&self) -> broadcast::Receiver<wire::ChatFrame> {
        self.chats.frames.subscribe()
    }

    /// The server's id and name, as every API answer names them.
    pub fn server_ref(&self) -> wire::ServerRef {
        wire::ServerRef {
            id: self
                .api
                .key
                .as_ref()
                .map(pf_api::tls::ServerKey::spki_sha256_hex),
            name: self.cfg.server_name.clone(),
        }
    }

    // -------------------------------------------------------------- items and frames

    /// Send one frame to the open streams. No reader is no error.
    pub(super) fn emit(&self, frame: wire::ChatFrame) {
        let _ = self.chats.frames.send(frame);
    }

    /// Send the session's record as a `session` frame.
    pub(super) fn emit_session(&self, session_id: &str) {
        let Ok(Some(row)) = self.store.chat_session(session_id) else {
            return;
        };
        if let Ok(record) = self.session_record(&row, false) {
            self.emit(wire::ChatFrame::Session { session: record });
        }
    }

    /// Append one item to the display log and send its frame. The two are one step, so the frames
    /// carry the `seq` in order.
    pub(super) fn append_item(&self, item: NewChatItem) -> Result<ChatItemRow, String> {
        let _one = lock(&self.chats.emit_lock);
        let row = self
            .store
            .append_chat_item(item)
            .map_err(|e| e.to_string())?;
        self.emit(wire::ChatFrame::Item {
            item: super::chat_view::item_record(&row),
        });
        Ok(row)
    }

    /// A notice item: a line the controller adds to the log.
    pub(super) fn notice(&self, session_id: &str, code: &str, text: &str) {
        let body = wire::ChatItemBody::Notice {
            code: code.to_string(),
            text: text.to_string(),
        };
        let item = super::chat_view::new_item(session_id, &body);
        if let Err(why) = self.append_item(item) {
            eprintln!("pulse-factory: a notice of chat session {session_id} is not stored: {why}");
        }
    }

    /// Close the pending requests of a stopped process with a `lost` answer. Returns how many it
    /// closed. It is scoped to the process it is given: when another process of the same session
    /// is live, it closes only the requests this one held and leaves the other's untouched. It
    /// holds the open lock, so no process of the session opens while it reads the table and the
    /// store.
    pub(super) fn close_pending(&self, live: &Arc<LiveChat>, reason: &str) -> usize {
        let session_id = live.session.as_str();
        let _no_open = lock(&self.chats.open_lock);
        let replaced = lock(&self.chats.live)
            .get(session_id)
            .is_some_and(|other| !Arc::ptr_eq(other, live));
        let own: BTreeSet<u64> = lock(&live.inner).requests.keys().copied().collect();
        let pending = match self.store.pending_requests(session_id) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "pulse-factory: the requests of chat session {session_id} are not read: {e}"
                );
                return 0;
            }
        };
        let mut closed = 0;
        for request in pending {
            if replaced && !own.contains(&request.seq) {
                continue;
            }
            let Some(answer) =
                ChatItemKind::parse(&request.kind).and_then(ChatItemKind::answered_by)
            else {
                continue;
            };
            let body = super::chat_view::closing_body(answer, wire::AnswerOutcome::Lost, reason);
            let mut item = super::chat_view::new_item(session_id, &body);
            item.answers_seq = Some(request.seq);
            if self.append_item(item).is_ok() {
                closed += 1;
            }
        }
        lock(&live.inner).requests.clear();
        closed
    }

    /// Write the session's state when it differs from the one last written. The state follows what
    /// the process does: a pending request is `needs_you`, a running turn is `working`.
    pub(super) fn sync_state(&self, live: &LiveChat) {
        let state = {
            let inner = lock(&live.inner);
            if !inner.requests.is_empty() {
                ChatSessionState::NeedsYou
            } else if inner.turn_since.is_some() {
                ChatSessionState::Working
            } else {
                ChatSessionState::Idle
            }
        };
        let changed = {
            let mut inner = lock(&live.inner);
            let changed = inner.state != state;
            inner.state = state;
            changed
        };
        if changed {
            let _ = self
                .store
                .set_chat_session_state(&live.session, state, &now());
        }
    }

    // -------------------------------------------------------------- the open and the stop

    /// Open the chat process of a session, new or resumed. One open at a time, so the cap holds.
    /// A process that is already live is returned as it is.
    pub(super) fn open_chat(&self, row: &ChatSessionRow) -> Result<Arc<LiveChat>, OpenFailure> {
        let _one = lock(&self.chats.open_lock);
        if let Some(live) = self.live_chat(&row.id) {
            return Ok(live);
        }
        // A process that is being stopped still holds the session. A second process opens only
        // after the stop has taken the first out of the table.
        if lock(&self.chats.live)
            .get(&row.id)
            .is_some_and(|l| l.is_stopping())
        {
            return Err(OpenFailure::Refused(format!(
                "the chat process of session {} is stopping; send again in a moment",
                row.id
            )));
        }
        let failed = |why: String| OpenFailure::Failed(why);
        let port = self.chat_port(&row.runtime).map_err(OpenFailure::Refused)?;
        let agent = self
            .agent(&row.agent_id)
            .map_err(failed)?
            .ok_or_else(|| OpenFailure::Refused(format!("no agent {}", row.agent_id)))?;
        let project = self
            .store
            .project(&row.project_id)
            .map_err(|e| failed(e.to_string()))?
            .ok_or_else(|| OpenFailure::Refused(format!("no project {}", row.project_id)))?;
        let worktree = self
            .store
            .worktree(&row.worktree_id)
            .map_err(|e| failed(e.to_string()))?
            .ok_or_else(|| OpenFailure::Refused(format!("no worktree {}", row.worktree_id)))?;
        // A resume checks the worktree live, as the admission of a start does.
        if row.runtime_session_ref.is_some() {
            match self.check_worktree(&project, &worktree.path, Some(&worktree)) {
                Ok(WorktreeLive::Gone(why)) => {
                    return Err(OpenFailure::Refused(format!(
                        "the worktree {} is gone: {why}",
                        worktree.path
                    )));
                }
                Ok(WorktreeLive::Live { .. }) => {}
                Err(why) => return Err(OpenFailure::Failed(why)),
            }
        }
        if let Some(why) = self.cap_refusal() {
            return Err(OpenFailure::Refused(why));
        }
        let options = effective_options(row, &agent);
        let notes = check_launch(
            port.as_ref(),
            &row.model,
            &row.effort,
            &row.permission_mode,
            &options,
        )
        .map_err(OpenFailure::Refused)?;
        for note in notes {
            eprintln!("{note}");
        }
        let mut env = agent.launch_env.clone();
        if let Some(pairs) = self
            .cfg
            .chat_agent_env
            .get(&super::chats::agent_env_key(&agent.id))
        {
            for (name, value) in pairs {
                env.insert(name.clone(), value.clone());
            }
        }
        // Only a session that has had a message has a conversation to resume. The runtime writes
        // no transcript before the first message, so a resume of a session with none would fail
        // as "context lost" (operator ruling on PR #41, class C1). A session with no message
        // opens fresh and stores the new runtime ID.
        let resume = match &row.runtime_session_ref {
            Some(r) if self.has_operator_message(&row.id).map_err(failed)? => {
                Some(SessionRef(r.clone()))
            }
            _ => None,
        };
        let spec = ChatOpen {
            cwd: worktree.path.clone(),
            resume,
            instructions: super::agents::instructions(
                &agent,
                &project.name,
                &project.root,
                &self.cfg.server_name,
                &row.id,
                &self.cfg.chat_share_dir,
            ),
            model: row.model.clone(),
            effort: row.effort.clone(),
            permission_mode: row.permission_mode.clone(),
            env,
            options: options.clone(),
        };
        let resuming = spec.resume.is_some();
        let opened = port.open(&spec).map_err(|e| match e {
            ChatError::ResumeFailed(why) => OpenFailure::ContextLost(why),
            ChatError::Refused(why) | ChatError::RuntimeNotReady(why) => OpenFailure::Refused(why),
            ChatError::Failed(why) => OpenFailure::Failed(why),
        })?;
        let pid = port.process_id(&opened.chat);
        let now_t = SystemTime::now();
        let live = Arc::new(LiveChat {
            session: row.id.clone(),
            chat: opened.chat.clone(),
            port: port.clone(),
            opened_at: now_t,
            version: opened.runtime_version.clone(),
            qualified: opened.qualified,
            stopping: AtomicBool::new(false),
            io: Mutex::new(()),
            inner: Mutex::new(LiveInner {
                turn_since: None,
                tasks: BTreeMap::new(),
                requests: BTreeMap::new(),
                tools: BTreeMap::new(),
                idle_since: now_t,
                context: None,
                applied: options,
                state: ChatSessionState::Idle,
            }),
        });
        if row.runtime_session_ref.as_deref() != Some(opened.session.0.as_str()) {
            let _ = self
                .store
                .set_chat_runtime_ref(&row.id, &opened.session.0, &now());
        }
        lock(&self.chats.lost).remove(&row.id);
        lock(&self.chats.live).insert(row.id.clone(), live.clone());
        self.chats.ledger.append(&format!(
            "open session={} runtime={} version={} qualified={} resume={} pid={}",
            field(&row.id),
            field(&row.runtime),
            field(&opened.runtime_version),
            opened.qualified,
            if resuming { "yes" } else { "no" },
            pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
        ));
        self.spawn_pump(live.clone(), opened.events);
        self.sync_state(&live);
        self.emit_session(&row.id);
        Ok(live)
    }

    /// Whether the session's log holds an `operator_message` item.
    fn has_operator_message(&self, session_id: &str) -> Result<bool, String> {
        let mut after = 0;
        loop {
            let page = self
                .store
                .chat_items_after(session_id, after, ITEM_PAGE)
                .map_err(|e| e.to_string())?;
            if page
                .iter()
                .any(|i| i.kind == ChatItemKind::OperatorMessage.as_str())
            {
                return Ok(true);
            }
            match page.last() {
                Some(last) if page.len() >= ITEM_PAGE as usize => after = last.seq,
                _ => return Ok(false),
            }
        }
    }

    /// One thread per live process drains its events. It never blocks the controller's runtime.
    fn spawn_pump(&self, live: Arc<LiveChat>, mut events: pf_engine::port::chat::ChatEvents) {
        let me = self.me.clone();
        let name = format!("chat-pump-{}", live.session);
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            while let Some(event) = events.blocking_recv() {
                let exited = matches!(event, ChatEvent::Exited { .. });
                let Some(controller) = me.upgrade() else {
                    break;
                };
                controller.handle_event(&live, event);
                if exited {
                    break;
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("pulse-factory: the chat pump cannot start: {e}");
        }
    }

    /// Stop a process and its group, take it out of the table and write its `stop` line. The
    /// caller writes the items and the frame that belong to the reason.
    pub(super) fn stop_chat(&self, live: &Arc<LiveChat>, reason: &str) {
        live.stopping.store(true, Ordering::SeqCst);
        if let Err(e) = live.port.stop(&live.chat) {
            eprintln!(
                "pulse-factory: chat session {} did not stop cleanly: {e}",
                live.session
            );
        }
        {
            let mut table = lock(&self.chats.live);
            if table
                .get(&live.session)
                .is_some_and(|l| Arc::ptr_eq(l, live))
            {
                table.remove(&live.session);
            }
        }
        self.chats.ledger.append(&format!(
            "stop session={} reason={reason}",
            field(&live.session)
        ));
        // The requests stay in the set: `close_pending` reads them to close this process's own
        // requests as lost, and it clears the set. The process left the table, so no answer
        // reaches them.
        lock(&live.inner).context = None;
    }

    // -------------------------------------------------------------- the cap

    /// Why each live process holds its place, one clause per reason (C7).
    fn holder_clauses(&self) -> Vec<String> {
        let mut out = Vec::new();
        for live in self.live_chats() {
            let holds = live.holds();
            if holds.is_empty() {
                let idle_since = lock(&live.inner).idle_since;
                out.push(format!(
                    "{} is idle until {}",
                    live.session,
                    utc(idle_since + self.cfg.chat_idle)
                ));
                continue;
            }
            for hold in holds {
                out.push(match hold {
                    Hold::Turn(t) => format!("{} runs a turn since {}", live.session, utc(t)),
                    Hold::Answer(t) => {
                        format!("{} waits for an answer since {}", live.session, utc(t))
                    }
                    Hold::Task(id, t) => {
                        format!(
                            "{} holds background task {id} since {}",
                            live.session,
                            utc(t)
                        )
                    }
                });
            }
        }
        out
    }

    /// The refusal of an open that would pass the cap, or `None` when there is room. It names every
    /// live chat and why it holds its process.
    pub(super) fn cap_refusal(&self) -> Option<String> {
        let live = lock(&self.chats.live).len();
        if live < self.cfg.chat_max_live {
            return None;
        }
        Some(format!(
            "the chat process cap ({}) is reached: {}; end one with pulse-factory session end",
            self.cfg.chat_max_live,
            self.holder_clauses().join("; ")
        ))
    }

    // -------------------------------------------------------------- the events

    /// Apply one event of a live process to the log, the session and the stream. A test feeds a
    /// hand-built sequence through [`Controller::apply_chat_event`].
    pub(super) fn handle_event(&self, live: &Arc<LiveChat>, event: ChatEvent) {
        let result = {
            let _io = lock(&live.io);
            // A stopper sets `stopping` while it holds `io`, so an event that waited for the lock
            // sees the flag here. A stopped process owns no state any more: only the result of a
            // finished turn and a moved runtime session still apply.
            if live.is_stopping()
                && !matches!(
                    event,
                    ChatEvent::TurnResult { .. }
                        | ChatEvent::SessionMoved { .. }
                        | ChatEvent::Exited { .. }
                )
            {
                return;
            }
            self.apply_event(live, event)
        };
        if result.turn_ended {
            // The usage call can take a while, so it runs outside the session's lock.
            let usage = live.port.context_usage(&live.chat).ok();
            lock(&live.inner).context = usage;
            self.emit_session(&live.session);
        }
    }

    /// Feed one event to the live process of a session. For tests.
    pub fn apply_chat_event(&self, session_id: &str, event: ChatEvent) -> Result<(), String> {
        let live = lock(&self.chats.live)
            .get(session_id)
            .cloned()
            .ok_or_else(|| format!("chat session {session_id} has no live process"))?;
        self.handle_event(&live, event);
        Ok(())
    }

    fn apply_event(&self, live: &Arc<LiveChat>, event: ChatEvent) -> EventResult {
        use super::chat_view::new_item;
        use wire::ChatItemBody as Body;
        let session = live.session.as_str();
        let mut result = EventResult::default();
        match event {
            ChatEvent::TurnStarted { .. } => {
                lock(&live.inner).turn_since = Some(SystemTime::now());
            }
            ChatEvent::TextDelta { text } => {
                self.emit(wire::ChatFrame::Delta {
                    session_id: session.to_string(),
                    text,
                });
                return result;
            }
            ChatEvent::Text { text } => {
                let _ = self.append_item(new_item(session, &Body::AssistantText { text }));
            }
            ChatEvent::ToolStarted {
                call_id,
                tool,
                input,
            } => {
                lock(&live.inner)
                    .tools
                    .insert(call_id.clone(), (tool.clone(), input));
                self.emit(wire::ChatFrame::ToolRunning {
                    session_id: session.to_string(),
                    call_id,
                    tool,
                });
                return result;
            }
            ChatEvent::ToolFinished {
                call_id,
                output,
                is_error,
            } => {
                let started = lock(&live.inner).tools.remove(&call_id);
                let (tool, input) = started.map_or_else(
                    || ("?".to_string(), None),
                    |(tool, input)| (tool, Some(input)),
                );
                let mut item = new_item(
                    session,
                    &Body::ToolCall {
                        call_id,
                        tool,
                        input: None,
                        output: None,
                        is_error,
                        finished: true,
                    },
                );
                item.tool_input = input.map(tool_text);
                item.tool_output = Some(tool_text(output));
                let _ = self.append_item(item);
            }
            ChatEvent::ApprovalRequest {
                request_id,
                tool,
                input,
                reason,
            } => {
                let mut item = new_item(
                    session,
                    &Body::ApprovalRequest {
                        request_id: request_id.clone(),
                        tool,
                        input: None,
                        reason,
                    },
                );
                item.tool_input = Some(tool_text(input));
                if let Ok(row) = self.append_item(item) {
                    lock(&live.inner).requests.insert(
                        row.seq,
                        OpenRequest {
                            request_id,
                            kind: ChatItemKind::ApprovalRequest,
                            since: SystemTime::now(),
                        },
                    );
                }
            }
            ChatEvent::Question { request_id, items } => {
                let items = items
                    .into_iter()
                    .map(|q| wire::QuestionRecord {
                        text: q.text,
                        header: q.header,
                        options: q
                            .options
                            .into_iter()
                            .map(|o| wire::QuestionChoiceRecord {
                                label: o.label,
                                description: o.description,
                            })
                            .collect(),
                        multi_select: q.multi_select,
                    })
                    .collect();
                let item = new_item(
                    session,
                    &Body::QuestionRequest {
                        request_id: request_id.clone(),
                        items,
                    },
                );
                if let Ok(row) = self.append_item(item) {
                    lock(&live.inner).requests.insert(
                        row.seq,
                        OpenRequest {
                            request_id,
                            kind: ChatItemKind::QuestionRequest,
                            since: SystemTime::now(),
                        },
                    );
                }
            }
            ChatEvent::RequestClosed { request_id } => {
                let found = {
                    let mut inner = lock(&live.inner);
                    let seq = inner
                        .requests
                        .iter()
                        .find(|(_, r)| r.request_id == request_id)
                        .map(|(seq, _)| *seq);
                    let removed = seq.and_then(|s| inner.requests.remove(&s).map(|r| (s, r)));
                    // A process freed by the withdrawal is idle from now, as after an answer.
                    if removed.is_some() && inner.requests.is_empty() && inner.turn_since.is_none()
                    {
                        inner.idle_since = SystemTime::now();
                    }
                    removed
                };
                if let Some((seq, request)) = found
                    && let Some(answer) = request.kind.answered_by()
                {
                    let body = super::chat_view::closing_body(
                        answer,
                        wire::AnswerOutcome::Withdrawn,
                        "the runtime withdrew the request",
                    );
                    let mut item = new_item(session, &body);
                    item.answers_seq = Some(seq);
                    let _ = self.append_item(item);
                }
            }
            ChatEvent::TaskStarted { task } => {
                let body = Body::BackgroundTask {
                    event: "started".to_string(),
                    task_id: task.id.clone(),
                    task_kind: Some(task.kind.clone()),
                    description: Some(task.description.clone()),
                    end: None,
                    summary: None,
                };
                lock(&live.inner)
                    .tasks
                    .insert(task.id.clone(), (task, SystemTime::now()));
                let _ = self.append_item(new_item(session, &body));
            }
            ChatEvent::TaskFinished {
                task_id,
                end,
                summary,
            } => {
                let finished = lock(&live.inner).tasks.remove(&task_id);
                let body = Body::BackgroundTask {
                    event: "finished".to_string(),
                    task_id,
                    task_kind: finished.as_ref().map(|(t, _)| t.kind.clone()),
                    description: finished.as_ref().map(|(t, _)| t.description.clone()),
                    end: Some(
                        match end {
                            TaskEnd::Completed => "completed",
                            TaskEnd::Failed => "failed",
                            TaskEnd::Stopped => "stopped",
                        }
                        .to_string(),
                    ),
                    summary: Some(summary),
                };
                let _ = self.append_item(new_item(session, &body));
                self.note_idle_if_free(live);
            }
            ChatEvent::TurnResult {
                cause,
                usage,
                is_error,
                stop,
                duration_ms,
                cache_ttl_seconds,
            } => {
                let body = Body::TurnResult {
                    cause: cause_text(cause).to_string(),
                    usage: wire::ChatUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_read_tokens: usage.cache_read_tokens,
                        cache_write_tokens: usage.cache_write_tokens,
                        cost_micros: usage.cost_micros,
                    },
                    is_error,
                    stop: stop.clone(),
                    duration_ms,
                    cache_ttl_seconds,
                };
                let mut item = new_item(session, &body);
                item.usage = Some(usage);
                let _ = self.append_item(item);
                {
                    let mut inner = lock(&live.inner);
                    inner.turn_since = None;
                    if inner.requests.is_empty() {
                        inner.idle_since = SystemTime::now();
                    }
                }
                lock(&self.chats.meta).insert(
                    session.to_string(),
                    SessionMeta {
                        last_turn_at: Some(now()),
                        cache_ttl: cache_ttl_seconds,
                    },
                );
                self.chats.ledger.append(&format!(
                    "turn session={} cause={} input_tokens={} output_tokens={} cache_read_tokens={} cache_write_tokens={} cost_micros={} duration_ms={duration_ms} stop={} is_error={is_error}",
                    field(session),
                    cause_text(cause),
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_tokens,
                    usage.cache_write_tokens,
                    usage.cost_micros,
                    field(&stop),
                ));
                result.turn_ended = true;
            }
            ChatEvent::SessionMoved { session: moved } => {
                let _ = self.store.set_chat_runtime_ref(session, &moved.0, &now());
            }
            ChatEvent::Exited { code, reason } => {
                if live.is_stopping() {
                    return result;
                }
                {
                    let mut table = lock(&self.chats.live);
                    if table.get(session).is_some_and(|l| Arc::ptr_eq(l, live)) {
                        table.remove(session);
                    }
                }
                let code_text = code.map_or_else(|| "none".to_string(), |c| c.to_string());
                self.notice(
                    session,
                    "exited",
                    &format!("the chat process exited (code {code_text}): {reason}"),
                );
                self.close_pending(live, "the chat process exited");
                // A tool still open at the exit is stored with no output and `finished: false`.
                let open_tools = std::mem::take(&mut lock(&live.inner).tools);
                for (call_id, (tool, input)) in open_tools {
                    let mut item = new_item(
                        session,
                        &Body::ToolCall {
                            call_id,
                            tool,
                            input: None,
                            output: None,
                            is_error: false,
                            finished: false,
                        },
                    );
                    item.tool_input = Some(tool_text(input));
                    let _ = self.append_item(item);
                }
                {
                    let mut inner = lock(&live.inner);
                    inner.turn_since = None;
                    inner.tasks.clear();
                    inner.requests.clear();
                    inner.context = None;
                }
                self.chats.ledger.append(&format!(
                    "stop session={} reason=exited code={code_text}",
                    field(session)
                ));
            }
        }
        self.sync_state(live);
        self.emit_session(session);
        result
    }

    /// When a finished task leaves the process with no holder, the idle clock starts.
    fn note_idle_if_free(&self, live: &LiveChat) {
        if live.holds().is_empty() {
            lock(&live.inner).idle_since = SystemTime::now();
        }
    }

    // -------------------------------------------------------------- the lifetime

    /// Release each process that has been idle for the idle window (Q2 rule 1). A release stops
    /// the process, adds a notice, and starts nothing. It runs on the controller's tick.
    pub fn sweep_chats(&self) {
        for live in self.live_chats() {
            if live.is_stopping() || !live.is_idle() {
                continue;
            }
            let idle_since = lock(&live.inner).idle_since;
            let idle_for = SystemTime::now()
                .duration_since(idle_since)
                .unwrap_or_default();
            if idle_for < self.cfg.chat_idle {
                continue;
            }
            let _io = lock(&live.io);
            // A message may have arrived while this waited for the lock.
            if live.is_stopping() || !live.is_idle() {
                continue;
            }
            self.stop_chat(&live, "idle");
            self.notice(
                &live.session,
                "idle_release",
                "released after 30 idle minutes; the next message resumes it",
            );
            self.sync_state(&live);
            self.emit_session(&live.session);
        }
    }

    /// Stop every live process for `halt`: close each pending request as `lost`, add a notice, and
    /// leave the session live and idle. Returns the sessions it stopped.
    pub(super) fn halt_chats(&self) -> Vec<String> {
        let mut stopped = Vec::new();
        for live in self.live_chats() {
            let _io = lock(&live.io);
            self.stop_chat(&live, "halt");
            self.close_pending(&live, "halted");
            self.notice(
                &live.session,
                "halted",
                "halted by the operator; the next message resumes this chat",
            );
            {
                let mut inner = lock(&live.inner);
                inner.turn_since = None;
                inner.tasks.clear();
            }
            self.sync_state(&live);
            self.emit_session(&live.session);
            stopped.push(live.session.clone());
        }
        stopped
    }

    /// Stop every live process and end every stream, for a controller stop.
    pub fn shutdown_chats(&self) {
        for live in self.live_chats() {
            let _io = lock(&live.io);
            self.stop_chat(&live, "shutdown");
        }
        let mut streams = lock(&self.chats.streams);
        for (_, end) in streams.drain(..) {
            let _ = end.send(StreamEnd::Shutdown);
        }
    }

    // -------------------------------------------------------------- the stream

    /// Open the stream of a device (`GET /v1/stream`). The reader is subscribed to the live frames
    /// first. Then a task sends `hello`, the stored items after `after`, `caught`, and the live
    /// frames, and drops an `item` frame whose `seq` it already sent.
    pub(super) fn open_stream(
        &self,
        device: &DeviceRecord,
        session: Option<&str>,
        after: u64,
    ) -> Result<StreamFeed, StreamRefusal> {
        if let Some(id) = session {
            match self.store.chat_session(id) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(StreamRefusal::NotFound(format!("no chat session {id}")));
                }
                Err(e) => return Err(StreamRefusal::BadRequest(e.to_string())),
            }
        }
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| StreamRefusal::BadRequest("the controller has no runtime".to_string()))?;
        let rx = self.chats.frames.subscribe();
        let (end_tx, end_rx) = mpsc::unbounded_channel();
        {
            let mut streams = lock(&self.chats.streams);
            streams.retain(|(_, tx)| !tx.is_closed());
            streams.push((device.id.clone(), end_tx));
        }
        let (tx, feed) = mpsc::channel(256);
        let me = self.me.clone();
        handle.spawn(forward(
            me,
            session.map(str::to_string),
            after,
            rx,
            end_rx,
            tx,
        ));
        Ok(feed)
    }

    /// End every open stream of a device: a revoke closes them at once (C13).
    pub(super) fn close_device_streams(&self, device_id: &str) {
        let mut streams = lock(&self.chats.streams);
        streams.retain(|(id, tx)| {
            if id == device_id {
                let _ = tx.send(StreamEnd::Revoked);
                false
            } else {
                !tx.is_closed()
            }
        });
    }

    /// The `hello` frame of a stream.
    fn hello_frame(&self, session: Option<&str>, after: u64) -> Result<wire::ChatFrame, String> {
        let rows = self.store.live_chat_sessions().map_err(|e| e.to_string())?;
        let mut sessions = Vec::new();
        for row in &rows {
            sessions.push(self.session_record(row, true)?);
        }
        Ok(wire::ChatFrame::Hello {
            server: self.server_ref(),
            sessions,
            following: session.map(str::to_string),
            after_seq: after,
        })
    }
}

/// What applying one event found.
#[derive(Default)]
struct EventResult {
    turn_ended: bool,
}

fn cause_text(cause: TurnCause) -> &'static str {
    match cause {
        TurnCause::Operator => "operator",
        TurnCause::Notice => "notice",
        TurnCause::Task => "task",
    }
}

/// An adapter's capped text as the store keeps it, byte for byte with its size and flag.
fn tool_text(c: Capped) -> ToolText {
    ToolText {
        text: c.text,
        bytes: c.bytes,
        truncated: c.truncated,
    }
}

fn frame_text(frame: &wire::ChatFrame) -> StreamEvent {
    StreamEvent::Frame(serde_json::to_string(frame).unwrap_or_default())
}

/// The task of one stream: the reconnect, then the live frames.
async fn forward(
    me: std::sync::Weak<Controller>,
    session: Option<String>,
    after: u64,
    mut rx: broadcast::Receiver<wire::ChatFrame>,
    mut end: mpsc::UnboundedReceiver<StreamEnd>,
    tx: mpsc::Sender<StreamEvent>,
) {
    let Some(controller) = me.upgrade() else {
        return;
    };
    let mut last = after;
    // `hello`, the stored items after `after`, then `caught`.
    let hello = {
        let c = controller.clone();
        let s = session.clone();
        tokio::task::spawn_blocking(move || c.hello_frame(s.as_deref(), after)).await
    };
    match hello {
        Ok(Ok(frame)) => {
            if tx.send(frame_text(&frame)).await.is_err() {
                return;
            }
        }
        Ok(Err(why)) => {
            eprintln!("pulse-factory: the stream's hello cannot be built: {why}");
            return;
        }
        Err(_) => return,
    }
    if let Some(id) = &session {
        loop {
            let page = {
                let c = controller.clone();
                let id = id.clone();
                tokio::task::spawn_blocking(move || c.store.chat_items_after(&id, last, ITEM_PAGE))
                    .await
            };
            let Ok(Ok(rows)) = page else {
                return;
            };
            let n = rows.len();
            for row in rows {
                last = last.max(row.seq);
                let frame = wire::ChatFrame::Item {
                    item: super::chat_view::item_record(&row),
                };
                if tx.send(frame_text(&frame)).await.is_err() {
                    return;
                }
            }
            if n < ITEM_PAGE as usize {
                break;
            }
        }
        let caught = wire::ChatFrame::Caught {
            session_id: id.clone(),
            last_seq: last,
        };
        if tx.send(frame_text(&caught)).await.is_err() {
            return;
        }
    }
    drop(controller);
    // The live frames. An `item` frame whose `seq` was already sent is dropped, unless it is a
    // redaction's re-send (`redacted_at` set).
    loop {
        tokio::select! {
            biased;
            ended = end.recv() => {
                let reason = ended.unwrap_or(StreamEnd::Shutdown);
                let _ = tx.send(StreamEvent::End(reason)).await;
                return;
            }
            received = rx.recv() => match received {
                Ok(frame) => {
                    let wanted = match (&frame, &session) {
                        (wire::ChatFrame::Session { .. }, _) => true,
                        (wire::ChatFrame::Item { item }, Some(s)) => {
                            if &item.session_id != s {
                                false
                            } else if item.seq > last {
                                last = item.seq;
                                true
                            } else {
                                // A seq already sent is dropped, except a redaction's re-send:
                                // the same seq with `redacted_at` set. Only `session redact`
                                // emits one, and the reader needs the marks.
                                item.redacted_at.is_some()
                            }
                        }
                        (wire::ChatFrame::Delta { session_id, .. }
                            | wire::ChatFrame::ToolRunning { session_id, .. }, Some(s)) => session_id == s,
                        _ => false,
                    };
                    if wanted && tx.send(frame_text(&frame)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = tx.send(StreamEvent::End(StreamEnd::Lagged { after_seq: last })).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let _ = tx.send(StreamEvent::End(StreamEnd::Shutdown)).await;
                    return;
                }
            },
            () = tx.closed() => return,
        }
    }
}

/// The payload text of a stored item: the body without the tool texts, which live in their own
/// columns.
pub(super) fn payload_of(body: &wire::ChatItemBody) -> String {
    let mut value = serde_json::to_value(body).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.remove("input");
        map.remove("output");
    }
    value.to_string()
}
