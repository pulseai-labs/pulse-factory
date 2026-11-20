//! The chat port: a conversation with a coding agent, one live process per chat.
//!
//! It is a separate port from [`AgentPort`](crate::port::agent::AgentPort): a run seat is a
//! terminal session the controller drives, a chat is a process the controller talks to. A chat
//! adapter opens a process (new, or resumed from the runtime's own session id), sends it messages,
//! interrupts and stops it, answers its approvals and questions, changes its model, effort and
//! permission mode while it lives, and reports its context use and its background tasks. Every
//! event reaches the caller as it happens, on a channel. The engine knows no runtime: this module
//! holds data and one grammar, and a second runtime is a new adapter crate (ADR-0002).
//!
//! The contract in full: `docs/contracts/chat-adapter.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::port::host::SessionHosts;

pub use crate::port::agent::{SessionRef, Usage};

/// The most bytes of tool input or output one event carries, mark included.
pub const CAP_BYTES: usize = 16 * 1024;

/// One live chat process. The adapter gives it; it means nothing after a restart.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChatId(pub String);

/// What the adapter needs to open one chat process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatOpen {
    /// The session's worktree. A resume uses the same one.
    pub cwd: String,
    /// `None` starts a new runtime session.
    pub resume: Option<SessionRef>,
    /// The agent's instructions, appended to the runtime's own.
    pub instructions: String,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    /// The agent's launch-environment map.
    pub env: BTreeMap<String, String>,
    /// Runtime-specific options, by option id.
    pub options: BTreeMap<String, String>,
    /// Shell command patterns the runtime must refuse to run, in a runtime-neutral form (see
    /// [`check_deny_command`]). The controller resolves the list at each open (ADR-0016). A deny
    /// rule is a speed bump, not a boundary: it stops the command text the model writes.
    pub deny_commands: Vec<String>,
    /// The store's id of the chat session (`S-<n>`). A host keys its session by it.
    pub chat_session: String,
    /// The id of the session's project.
    pub project: String,
    /// The ids of the session's agent and worktree. The launch record names them in its binding.
    pub agent_id: String,
    pub worktree_id: String,
    /// The scope the deny list came from, as the setting resolver names it. The launch record
    /// keeps it beside the list.
    pub deny_from: String,
    /// The agent's secret variables (`PF_AGENT_ENV_<AGENT>` pairs), apart from `env`. A launch
    /// record keeps `sha256:<hex>` of each value, never the value.
    pub secret_env: BTreeMap<String, String>,
    /// The runtime copy the session was launched with, from its launch record. A resume starts
    /// that copy or nothing (ADR-0007): it never starts another one.
    pub recorded_binary: Option<RecordedBinary>,
}

/// The runtime copy a launch record names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedBinary {
    pub path: String,
    /// The SHA-256 of the file, in lower-case hex.
    pub sha256: String,
    pub version: String,
}

/// One event and where it came from. `at` is the host's output cursor after the output line that
/// gave the event. `checkpoint` is set on the last event of a line after which the adapter is
/// quiet (no turn and no request open): it is the adapter's own state, opaque to the caller, and
/// the caller stores it with the position. An adapter that runs no host sends `at: None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatDelivery {
    pub event: ChatEvent,
    pub at: Option<u64>,
    pub checkpoint: Option<String>,
}

impl ChatDelivery {
    /// An event with no position, as an adapter with no host sends it.
    pub fn plain(event: ChatEvent) -> ChatDelivery {
        ChatDelivery {
            event,
            at: None,
            checkpoint: None,
        }
    }
}

/// The channel that carries a chat's events. It is unbounded, so a reader thread never blocks the
/// runtime. A dropped receiver does not stop the process and fails no call.
pub type ChatEvents = tokio::sync::mpsc::UnboundedReceiver<ChatDelivery>;

/// The host a chat runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatHost {
    pub unit: String,
    pub incarnation: u32,
}

/// What `open` returns.
#[derive(Debug)]
pub struct ChatOpened {
    pub chat: ChatId,
    /// The runtime's own session id. It survives the process.
    pub session: SessionRef,
    /// The version the runtime printed.
    pub runtime_version: String,
    /// True when the runtime is the version the adapter qualified.
    pub qualified: bool,
    pub events: ChatEvents,
    /// The host the process runs in. `None` for an adapter that runs no host.
    pub host: Option<ChatHost>,
}

/// What kind of request is pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    Approval,
    Question,
}

/// One request the chat awaits an answer to, as the store holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    pub request_id: String,
    pub kind: PendingKind,
    /// The stored input text, when it was not cut. A request with no input can be denied, and an
    /// allow is refused (the adapter has no input to hand back).
    pub input: Option<String>,
}

/// Where a chat stands when the controller rebinds to its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatResumePoint {
    /// The runtime's own session id, from the session row.
    pub runtime_session: SessionRef,
    /// The cursor the controller last acknowledged to the host.
    pub acked: u64,
    /// The cursor after the last output line the store consumed.
    pub committed: u64,
    /// The adapter's opaque state at `acked`.
    pub checkpoint: Option<String>,
    pub pending: Vec<PendingRequest>,
}

/// What `rebind` found.
#[derive(Debug)]
pub enum ChatRebind {
    /// The host runs and the adapter attached. The chat keeps its process.
    Survived(ChatOpened),
    /// The host is gone. `exit` is what it left.
    Lost { exit: Value },
    /// The host may run, and the adapter could not attach to it.
    Unreachable { why: String },
}

/// Who sent a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Operator,
    Notice,
}

/// What started a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnCause {
    Operator,
    Notice,
    /// A background task the chat started finished.
    Task,
}

/// Text cut at [`CAP_BYTES`] on a character boundary, with a mark, and the original size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capped {
    pub text: String,
    /// The size of the original text in bytes.
    pub bytes: u64,
    pub truncated: bool,
}

/// One question the agent asks, with its choices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionItem {
    pub text: String,
    pub header: String,
    pub options: Vec<QuestionChoice>,
    pub multi_select: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionChoice {
    pub label: String,
    pub description: String,
}

/// A task the agent runs in the background.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub id: String,
    pub kind: String,
    pub description: String,
    /// True for housekeeping that is not activity.
    pub ambient: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEnd {
    Completed,
    Failed,
    Stopped,
}

/// One thing that happened in a chat. The adapter pushes each event as it happens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    TurnStarted {
        cause: TurnCause,
    },
    /// Text as it streams.
    TextDelta {
        text: String,
    },
    /// One finished assistant text block.
    Text {
        text: String,
    },
    /// `input` is the tool's input as JSON text.
    ToolStarted {
        call_id: String,
        tool: String,
        input: Capped,
    },
    ToolFinished {
        call_id: String,
        output: Capped,
        is_error: bool,
    },
    ApprovalRequest {
        request_id: String,
        tool: String,
        input: Capped,
        reason: Option<String>,
    },
    Question {
        request_id: String,
        items: Vec<QuestionItem>,
    },
    /// The runtime withdrew the request unanswered.
    RequestClosed {
        request_id: String,
    },
    TaskStarted {
        task: BackgroundTask,
    },
    TaskFinished {
        task_id: String,
        end: TaskEnd,
        summary: String,
    },
    /// The end of a turn. `usage` is the turn's own usage, never a running total.
    TurnResult {
        cause: TurnCause,
        usage: Usage,
        is_error: bool,
        stop: String,
        duration_ms: u64,
        /// 300 or 3600 when the turn's usage says which cache it wrote.
        cache_ttl_seconds: Option<u32>,
    },
    /// The runtime reports another session id. The caller keeps the new one.
    SessionMoved {
        session: SessionRef,
    },
    /// The host dropped output from cursor `from` to `to` before it was read. It may come anywhere
    /// before `Exited`.
    OutputLost {
        from: u64,
        to: u64,
    },
    /// The process ended. Always the last event.
    Exited {
        code: Option<i32>,
        reason: String,
    },
}

impl ChatEvent {
    /// The event's name, as its `type` tag spells it.
    pub fn name(&self) -> &'static str {
        match self {
            ChatEvent::TurnStarted { .. } => "TurnStarted",
            ChatEvent::TextDelta { .. } => "TextDelta",
            ChatEvent::Text { .. } => "Text",
            ChatEvent::ToolStarted { .. } => "ToolStarted",
            ChatEvent::ToolFinished { .. } => "ToolFinished",
            ChatEvent::ApprovalRequest { .. } => "ApprovalRequest",
            ChatEvent::Question { .. } => "Question",
            ChatEvent::RequestClosed { .. } => "RequestClosed",
            ChatEvent::TaskStarted { .. } => "TaskStarted",
            ChatEvent::TaskFinished { .. } => "TaskFinished",
            ChatEvent::TurnResult { .. } => "TurnResult",
            ChatEvent::SessionMoved { .. } => "SessionMoved",
            ChatEvent::OutputLost { .. } => "OutputLost",
            ChatEvent::Exited { .. } => "Exited",
        }
    }
}

/// The answer to an approval or a question.
///
/// It carries a `type` tag and its data under `value`, so a question's text can never collide with
/// the tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ChatAnswer {
    Allow,
    Deny {
        message: String,
    },
    /// Question text to the chosen answer.
    Answers(BTreeMap<String, String>),
}

/// One option an adapter declares: a setting of the chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatOption {
    pub id: String,
    pub label: String,
    pub values: Vec<String>,
    pub default: String,
    /// True when a value outside `values` is allowed (it still has to be well formed).
    pub free: bool,
    /// True when `set_option` changes it while the chat lives.
    pub live: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub max_tokens: u64,
}

/// Why a chat call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatError {
    /// The adapter will not do the call: an unqualified version, a bad option value, a reserved
    /// environment name, an answer to an unknown request.
    Refused(String),
    /// The adapter tried and failed.
    Failed(String),
    /// `open` with `resume` cannot resume that session. An adapter never answers this by starting
    /// a fresh context.
    ResumeFailed(String),
    /// The runtime's own binary is missing or fails its check. The text names the fix.
    RuntimeNotReady(String),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Refused(why) => write!(f, "chat call refused by the adapter: {why}"),
            ChatError::Failed(why) => write!(f, "chat adapter failed: {why}"),
            ChatError::ResumeFailed(why) => write!(f, "chat resume failed: {why}"),
            ChatError::RuntimeNotReady(why) => write!(f, "chat runtime not ready: {why}"),
        }
    }
}

impl std::error::Error for ChatError {}

/// What a `send` during a running turn does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidTurn {
    /// The message goes into the running turn. No new `TurnStarted` comes.
    Steer,
    /// The message becomes the next turn.
    NextTurn,
}

/// The chat adapter port. The calls block until done, at most a timeout (`ChatError::Failed` on a
/// timeout). An async caller runs them with `spawn_blocking`.
pub trait ChatPort: Send + Sync {
    /// The options this adapter declares.
    fn options(&self) -> Vec<ChatOption>;
    /// What a `send` during a running turn does.
    fn mid_turn(&self) -> MidTurn;
    /// Open a chat process: new, or resumed.
    fn open(&self, spec: &ChatOpen) -> Result<ChatOpened, ChatError>;
    /// Send a message. During a turn it follows [`ChatPort::mid_turn`].
    fn send(&self, chat: &ChatId, kind: InputKind, text: &str) -> Result<(), ChatError>;
    /// Answer an approval or a question.
    fn answer(&self, chat: &ChatId, request_id: &str, answer: &ChatAnswer)
    -> Result<(), ChatError>;
    /// End the running turn. The process stays.
    fn interrupt(&self, chat: &ChatId) -> Result<(), ChatError>;
    /// Change a `live` option of the running chat. It applies to this chat only.
    fn set_option(&self, chat: &ChatId, id: &str, value: &str) -> Result<(), ChatError>;
    fn context_usage(&self, chat: &ChatId) -> Result<ContextUsage, ChatError>;
    /// The background tasks that run now. It still answers after a stop.
    fn background_tasks(&self, chat: &ChatId) -> Result<Vec<BackgroundTask>, ChatError>;
    /// Is the process alive? An unknown chat is `Ok(false)`.
    fn is_alive(&self, chat: &ChatId) -> Result<bool, ChatError>;
    /// End the process and every process in its group. An unknown chat is `Ok(())`. It is safe to
    /// repeat.
    fn stop(&self, chat: &ChatId) -> Result<(), ChatError>;
    /// The operating-system process id of the chat's process, when the adapter runs one.
    fn process_id(&self, _chat: &ChatId) -> Option<u32> {
        None
    }
    /// Hand the adapter the controller's host service. The controller calls it once, before any
    /// chat opens or rebinds. An adapter that runs no host ignores it.
    fn set_hosts(&self, _hosts: Arc<dyn SessionHosts>) {}
    /// Attach to the host of `unit` again after a controller restart, and read on from the
    /// controller's position. The default loses the chat.
    fn rebind(&self, _unit: &str, _point: &ChatResumePoint) -> ChatRebind {
        ChatRebind::Lost {
            exit: json!({"reason": "no session host"}),
        }
    }
    /// Say the output below `to` is stored. The adapter passes it to the host. The default does
    /// nothing.
    fn ack(&self, _chat: &ChatId, _to: u64) -> Result<(), ChatError> {
        Ok(())
    }
    /// Drop the connection to the chat and stop nothing: the controller stops, the process lives
    /// on. The default does nothing.
    fn detach(&self, _chat: &ChatId) -> Result<(), ChatError> {
        Ok(())
    }
}

/// The most characters of one deny pattern.
pub const DENY_COMMAND_MAX_CHARS: usize = 100;

/// The most patterns in one deny list.
pub const DENY_COMMANDS_MAX: usize = 32;

/// Check one shell command pattern of [`ChatOpen::deny_commands`]. One rule for the controller and
/// both adapters.
///
/// A pattern is words; `*` stands for any text.
///
/// - 1 to 100 characters of `A-Z a-z 0-9`, space, `.`, `_`, `/`, `:`, `=`, `@`, `+`, `-` and `*`;
/// - the first character is a letter or a digit; no trailing space; no two spaces in a row;
/// - `*` only as a whole word (`git push * --force`) or as the last character of a word
///   (`--force*`, `--repo=*`, `+*`); never inside a word, never `**`;
/// - no `:*` anywhere: a runtime's rule syntax may read a rule that ends `:*` as a legacy prefix
///   form, so `Bash(git push * :*)` would deny every push;
/// - so no `(`, `)`, `,`, quote, `;`, `&`, `|` or control character can reach a rule.
pub fn check_deny_command(pattern: &str) -> Result<(), String> {
    let shown = |c: char| -> String {
        if c.is_control() {
            format!("U+{:04X}", u32::from(c))
        } else {
            format!("{c:?}")
        }
    };
    let n = pattern.chars().count();
    if n == 0 {
        return Err("a deny pattern is empty".to_string());
    }
    if n > DENY_COMMAND_MAX_CHARS {
        return Err(format!(
            "a deny pattern has {n} characters, and the limit is {DENY_COMMAND_MAX_CHARS}"
        ));
    }
    if let Some(c) = pattern.chars().find(|c| {
        !(c.is_ascii_alphanumeric()
            || matches!(c, ' ' | '.' | '_' | '/' | ':' | '=' | '@' | '+' | '-' | '*'))
    }) {
        return Err(format!(
            "deny pattern {pattern:?} holds {}, which a pattern may not hold",
            shown(c)
        ));
    }
    if !pattern.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return Err(format!(
            "deny pattern {pattern:?} must start with a letter or a digit"
        ));
    }
    if pattern.ends_with(' ') {
        return Err(format!("deny pattern {pattern:?} ends with a space"));
    }
    if pattern.contains("  ") {
        return Err(format!(
            "deny pattern {pattern:?} holds two spaces in a row"
        ));
    }
    if pattern.contains("**") {
        return Err(format!(
            "deny pattern {pattern:?} holds ** : a * stands alone"
        ));
    }
    if pattern.contains(":*") {
        return Err(format!(
            "deny pattern {pattern:?} holds :* , which a runtime's rule syntax may read as a legacy prefix form"
        ));
    }
    for word in pattern.split(' ') {
        let inside = word
            .char_indices()
            .any(|(i, c)| c == '*' && i + 1 != word.len());
        if inside {
            return Err(format!(
                "deny pattern {pattern:?}: a * may stand only as a whole word or end a word, not sit inside {word:?}"
            ));
        }
    }
    Ok(())
}

/// Check a deny list: at most 32 patterns, no duplicate, each passing [`check_deny_command`].
pub fn check_deny_commands(list: &[String]) -> Result<(), String> {
    if list.len() > DENY_COMMANDS_MAX {
        return Err(format!(
            "the deny list has {} patterns, and the limit is {DENY_COMMANDS_MAX}",
            list.len()
        ));
    }
    let mut seen = BTreeSet::new();
    for pattern in list {
        check_deny_command(pattern)?;
        if !seen.insert(pattern.as_str()) {
            return Err(format!("the deny list holds {pattern:?} twice"));
        }
    }
    Ok(())
}

/// Cut `text` to at most [`CAP_BYTES`] bytes, mark included.
///
/// Text of at most [`CAP_BYTES`] bytes is kept whole, with no mark. Longer text is cut back to a
/// character boundary so that the content and the mark `…[truncated: <bytes> bytes in all]` fit
/// together in [`CAP_BYTES`].
pub fn cap(text: &str) -> Capped {
    let bytes = text.len() as u64;
    if text.len() <= CAP_BYTES {
        return Capped {
            text: text.to_string(),
            bytes,
            truncated: false,
        };
    }
    let mark = format!("…[truncated: {bytes} bytes in all]");
    let mut end = CAP_BYTES.saturating_sub(mark.len()).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Capped {
        text: format!("{}{mark}", &text[..end]),
        bytes,
        truncated: true,
    }
}

/// The event grammar, in code: `Ok` when `events` is a legal sequence from one chat.
///
/// - each turn starts with `TurnStarted` and ends with one `TurnResult` whose `cause` matches it;
///   no turn starts inside an open turn;
/// - `TextDelta`, `Text`, `ToolStarted`, `ToolFinished`, `ApprovalRequest`, `Question` and
///   `RequestClosed` come only inside a turn;
/// - each `ToolFinished` follows a `ToolStarted` with its `call_id` in the same turn;
/// - a request id appears once, and `RequestClosed` names an open request, once;
/// - `TaskStarted` and `TaskFinished` may come inside or between turns; a `TaskFinished` names a
///   started task, once;
/// - a turn with cause `Task` follows a `TaskFinished` that no earlier `Task` turn answered;
/// - `SessionMoved` comes between turns, or right after a `TurnStarted` and before the turn's other
///   events;
/// - `OutputLost` may come anywhere before `Exited`;
/// - `Exited` comes once, last. A turn open at `Exited` has no `TurnResult`, which is allowed.
///
/// Both adapters' tests call it on every sequence they produce, so the fake, the real adapter and
/// the recordings cannot drift apart.
pub fn check_order(events: &[ChatEvent]) -> Result<(), String> {
    struct Turn {
        cause: TurnCause,
        tools: BTreeSet<String>,
        /// The events seen since `TurnStarted`.
        seen: usize,
    }
    let mut turn: Option<Turn> = None;
    let mut exited = false;
    // Request id to "still open".
    let mut requests: BTreeMap<String, bool> = BTreeMap::new();
    // Task id to "finished".
    let mut tasks: BTreeMap<String, bool> = BTreeMap::new();
    // `TaskFinished` events that no `Task` turn has answered yet: one turn answers one finish.
    let mut unanswered_finishes: usize = 0;

    for (i, event) in events.iter().enumerate() {
        let at = |why: String| format!("event {i} ({}): {why}", event.name());
        if exited {
            return Err(at("an event after Exited".to_string()));
        }
        match event {
            ChatEvent::TurnStarted { cause } => {
                if turn.is_some() {
                    return Err(at("a TurnStarted inside an open turn".to_string()));
                }
                if *cause == TurnCause::Task {
                    if unanswered_finishes == 0 {
                        return Err(at(
                            "a turn with cause Task follows no finished task".to_string()
                        ));
                    }
                    unanswered_finishes -= 1;
                }
                turn = Some(Turn {
                    cause: *cause,
                    tools: BTreeSet::new(),
                    seen: 0,
                });
                continue;
            }
            ChatEvent::TaskStarted { task } => {
                if tasks.insert(task.id.clone(), false).is_some() {
                    return Err(at(format!("task {} started twice", task.id)));
                }
            }
            ChatEvent::TaskFinished { task_id, .. } => match tasks.get_mut(task_id) {
                None => return Err(at(format!("no started task {task_id}"))),
                Some(true) => return Err(at(format!("task {task_id} finished twice"))),
                Some(done) => {
                    *done = true;
                    unanswered_finishes += 1;
                }
            },
            ChatEvent::SessionMoved { .. } => {
                if turn.as_ref().is_some_and(|t| t.seen > 0) {
                    return Err(at("a session move in the middle of a turn".to_string()));
                }
            }
            ChatEvent::Exited { .. } => exited = true,
            // A stretch the host dropped. It says nothing of the grammar, so it fits anywhere.
            ChatEvent::OutputLost { .. } => {}
            ChatEvent::TurnResult { cause, .. } => {
                let Some(open) = turn.take() else {
                    return Err(at("a TurnResult outside a turn".to_string()));
                };
                if open.cause != *cause {
                    return Err(at(format!(
                        "the TurnResult cause {cause:?} differs from the turn's {:?}",
                        open.cause
                    )));
                }
                continue;
            }
            ChatEvent::TextDelta { .. }
            | ChatEvent::Text { .. }
            | ChatEvent::ToolStarted { .. }
            | ChatEvent::ToolFinished { .. }
            | ChatEvent::ApprovalRequest { .. }
            | ChatEvent::Question { .. }
            | ChatEvent::RequestClosed { .. } => {
                let Some(open) = turn.as_mut() else {
                    return Err(at(
                        "an event that belongs in a turn comes outside a turn".to_string()
                    ));
                };
                match event {
                    ChatEvent::ToolStarted { call_id, .. } => {
                        open.tools.insert(call_id.clone());
                    }
                    ChatEvent::ToolFinished { call_id, .. } if !open.tools.contains(call_id) => {
                        return Err(at(format!(
                            "no ToolStarted with call id {call_id} in this turn"
                        )));
                    }
                    ChatEvent::ApprovalRequest { request_id, .. }
                    | ChatEvent::Question { request_id, .. } => {
                        if requests.insert(request_id.clone(), true).is_some() {
                            return Err(at(format!("request id {request_id} appears twice")));
                        }
                    }
                    ChatEvent::RequestClosed { request_id } => match requests.get_mut(request_id) {
                        Some(open_now @ true) => *open_now = false,
                        _ => {
                            return Err(at(format!("no open request {request_id} to close")));
                        }
                    },
                    _ => {}
                }
            }
        }
        if let Some(open) = turn.as_mut() {
            open.seen += 1;
        }
    }
    Ok(())
}
