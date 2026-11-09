//! `FakeChat`: a scripted chat adapter behind the chat port. It starts no process and reaches no
//! network. It follows the port exactly (the same event order, the same usage shape, the same cap
//! and the same refusals), so the controller's and the API's tests run on it, where a process is not
//! wanted.
//!
//! Every behaviour is driven by the message text, with no test hook:
//!
//! - a plain message gets `fake reply: <text>`, streamed as deltas of at most 8 characters;
//! - `fake:tool`, `fake:big` (a 20000-byte tool output, capped), `fake:approval`, `fake:question`,
//!   `fake:task <ms>`, `fake:slow <ms>`, `fake:fail` and `fake:exit` (the whole trimmed message).
//!
//! A turn's usage is its own: the input is the message's characters, the output the reply's
//! characters, and the cost is `3 × input + 15 × output` in millionths of a dollar.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::chat::{
    BackgroundTask, ChatAnswer, ChatError, ChatEvent, ChatId, ChatOpen, ChatOpened, ChatOption,
    ChatPort, ContextUsage, InputKind, MidTurn, QuestionChoice, QuestionItem, TaskEnd, TurnCause,
    cap,
};
use tokio::sync::mpsc::UnboundedSender;

/// What a `send` during a turn does, as the qualification of the real adapter selected it.
/// `ClaudeChat`'s `MID_TURN` holds the same value, and a test in `pf-adapter-claude` checks it.
pub const MID_TURN: MidTurn = MidTurn::Steer;

/// The context window the fake reports.
const MAX_CONTEXT_TOKENS: u64 = 200_000;

/// Granularity of the waits that must notice an interrupt or a stop.
const SLICE: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Approval,
    Question,
}

struct ChatState {
    tx: UnboundedSender<ChatEvent>,
    alive: bool,
    /// A turn thread is running.
    running: bool,
    interrupted: bool,
    /// Messages sent during a turn under `Steer`, not yet taken into it.
    steer: Vec<String>,
    /// Turns waiting for the running turn to end: a send under `NextTurn`, or a `Task` turn.
    queue: VecDeque<(TurnCause, String)>,
    /// Requests asked and not yet answered or closed.
    open_requests: BTreeMap<String, RequestKind>,
    answer: Option<(String, ChatAnswer)>,
    tasks: Vec<BackgroundTask>,
    used: u64,
    options: BTreeMap<String, String>,
    sent: Vec<String>,
    next_id: u32,
}

impl ChatState {
    /// Push one event. A dropped receiver is not an error.
    fn push(&mut self, event: ChatEvent) {
        if self.alive {
            let _ = self.tx.send(event);
        }
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }
}

struct Shared {
    state: Mutex<ChatState>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, ChatState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Default)]
struct Registry {
    chats: BTreeMap<ChatId, Arc<Shared>>,
    sessions: BTreeSet<String>,
    next_chat: u32,
    next_session: u32,
    opens: usize,
    stops: usize,
}

/// A scripted chat adapter. See the module doc for the message commands.
pub struct FakeChat {
    options: Vec<ChatOption>,
    mid_turn: MidTurn,
    registry: Mutex<Registry>,
}

impl Default for FakeChat {
    fn default() -> Self {
        FakeChat::new()
    }
}

fn option(
    id: &str,
    label: &str,
    values: &[&str],
    default: &str,
    free: bool,
    live: bool,
) -> ChatOption {
    ChatOption {
        id: id.into(),
        label: label.into(),
        values: values.iter().map(|v| v.to_string()).collect(),
        default: default.into(),
        free,
        live,
    }
}

/// The names the chat's environment never takes from a caller, and the name grammar.
fn check_env_name(name: &str) -> Result<(), ChatError> {
    let grammar = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if !grammar {
        return Err(ChatError::Refused(format!(
            "environment name {name:?} does not match [A-Z0-9_]+"
        )));
    }
    let reserved_prefix = ["ANTHROPIC_", "CLAUDE_CODE_", "PF_", "ORCA_", "HERDR_"]
        .iter()
        .any(|p| name.starts_with(p));
    let reserved_name = ["CLAUDE_CONFIG_DIR", "CLAUDECODE", "DISABLE_AUTOUPDATER"].contains(&name);
    if reserved_prefix || reserved_name {
        return Err(ChatError::Refused(format!(
            "environment name {name} is reserved: the adapter sets or forbids it"
        )));
    }
    Ok(())
}

/// A value is well formed when it is not empty, has at most 64 characters and holds no whitespace
/// or control character.
fn well_formed(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn check_value(option: &ChatOption, value: &str) -> Result<(), ChatError> {
    if !well_formed(value) {
        return Err(ChatError::Refused(format!(
            "value {value:?} of option {} is empty, longer than 64 characters, or holds whitespace or a control character",
            option.id
        )));
    }
    if !option.free && !option.values.iter().any(|v| v == value) {
        return Err(ChatError::Refused(format!(
            "value {value:?} is not one of the values of option {}: {}",
            option.id,
            option.values.join(", ")
        )));
    }
    Ok(())
}

impl FakeChat {
    /// The default options: `model`, `effort` and `permission_mode` (each free and live) and
    /// `artifacts` (`on` or `off`, not live), and the `MidTurn` mode the qualification selected.
    pub fn new() -> FakeChat {
        FakeChat::with_options(vec![
            option("model", "Model", &[], "fake-model", true, true),
            option("effort", "Effort", &[], "high", true, true),
            option(
                "permission_mode",
                "Permission mode",
                &[],
                "bypass",
                true,
                true,
            ),
            option("artifacts", "Artifacts", &["on", "off"], "on", false, false),
        ])
    }

    /// A fake with this exact option list, for a test that needs a strict one.
    pub fn with_options(options: Vec<ChatOption>) -> FakeChat {
        FakeChat {
            options,
            mid_turn: MID_TURN,
            registry: Mutex::new(Registry::default()),
        }
    }

    /// A fake with the other `send` behaviour during a turn, for a test.
    pub fn with_mid_turn(mode: MidTurn) -> FakeChat {
        FakeChat {
            mid_turn: mode,
            ..FakeChat::new()
        }
    }

    /// How many chats `open` gave. A refused open is not counted.
    pub fn open_count(&self) -> usize {
        self.registry().opens
    }

    /// How many chats `stop` ended. A repeated stop ends nothing and is not counted.
    pub fn stop_count(&self) -> usize {
        self.registry().stops
    }

    /// The texts `send` took for this chat, oldest first.
    pub fn sent(&self, chat: &ChatId) -> Vec<String> {
        self.find(chat)
            .map(|c| c.lock().sent.clone())
            .unwrap_or_default()
    }

    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn find(&self, chat: &ChatId) -> Option<Arc<Shared>> {
        self.registry().chats.get(chat).cloned()
    }

    fn known(&self, chat: &ChatId) -> Result<Arc<Shared>, ChatError> {
        self.find(chat)
            .ok_or_else(|| ChatError::Refused(format!("unknown chat {}", chat.0)))
    }

    fn declared(&self, id: &str) -> Option<&ChatOption> {
        self.options.iter().find(|o| o.id == id)
    }
}

impl ChatPort for FakeChat {
    fn options(&self) -> Vec<ChatOption> {
        self.options.clone()
    }

    fn mid_turn(&self) -> MidTurn {
        self.mid_turn
    }

    fn open(&self, spec: &ChatOpen) -> Result<ChatOpened, ChatError> {
        for name in spec.env.keys() {
            check_env_name(name)?;
        }
        let mut values: BTreeMap<String, String> = self
            .options
            .iter()
            .map(|o| (o.id.clone(), o.default.clone()))
            .collect();
        for (id, value) in [
            ("model", &spec.model),
            ("effort", &spec.effort),
            ("permission_mode", &spec.permission_mode),
        ] {
            if let Some(o) = self.declared(id) {
                check_value(o, value)?;
                values.insert(id.to_string(), value.clone());
            }
        }
        for (id, value) in &spec.options {
            if matches!(id.as_str(), "model" | "effort" | "permission_mode") {
                return Err(ChatError::Refused(format!(
                    "option {id} belongs in its own field of the open, not in the options map"
                )));
            }
            let Some(o) = self.declared(id) else {
                return Err(ChatError::Refused(format!("unknown option {id}")));
            };
            check_value(o, value)?;
            values.insert(id.clone(), value.clone());
        }

        let mut reg = self.registry();
        let session = match &spec.resume {
            Some(SessionRef(s)) if reg.sessions.contains(s) => s.clone(),
            Some(SessionRef(s)) => {
                return Err(ChatError::ResumeFailed(format!(
                    "this adapter never gave session {s}"
                )));
            }
            None => {
                reg.next_session += 1;
                let s = format!("fake-session-{}", reg.next_session);
                reg.sessions.insert(s.clone());
                s
            }
        };
        reg.next_chat += 1;
        reg.opens += 1;
        let chat = ChatId(format!("fake-chat-{}", reg.next_chat));
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        reg.chats.insert(
            chat.clone(),
            Arc::new(Shared {
                state: Mutex::new(ChatState {
                    tx,
                    alive: true,
                    running: false,
                    interrupted: false,
                    steer: Vec::new(),
                    queue: VecDeque::new(),
                    open_requests: BTreeMap::new(),
                    answer: None,
                    tasks: Vec::new(),
                    used: 0,
                    options: values,
                    sent: Vec::new(),
                    next_id: 0,
                }),
                wake: Condvar::new(),
            }),
        );
        Ok(ChatOpened {
            chat,
            session: SessionRef(session),
            runtime_version: "fake".into(),
            qualified: true,
            events,
        })
    }

    fn send(&self, chat: &ChatId, kind: InputKind, text: &str) -> Result<(), ChatError> {
        let shared = self.known(chat)?;
        let cause = match kind {
            InputKind::Operator => TurnCause::Operator,
            InputKind::Notice => TurnCause::Notice,
        };
        let mut st = shared.lock();
        if !st.alive {
            return Err(ChatError::Refused("the chat has ended".into()));
        }
        st.sent.push(text.to_string());
        if st.running {
            match self.mid_turn {
                MidTurn::Steer => st.steer.push(text.to_string()),
                MidTurn::NextTurn => st.queue.push_back((cause, text.to_string())),
            }
            shared.wake.notify_all();
            return Ok(());
        }
        st.running = true;
        drop(st);
        spawn_turns(shared, cause, text.to_string());
        Ok(())
    }

    fn answer(
        &self,
        chat: &ChatId,
        request_id: &str,
        answer: &ChatAnswer,
    ) -> Result<(), ChatError> {
        let shared = self.known(chat)?;
        let mut st = shared.lock();
        let Some(kind) = st.open_requests.get(request_id).copied() else {
            return Err(ChatError::Refused(format!(
                "no open request {request_id} to answer"
            )));
        };
        match (kind, answer) {
            (RequestKind::Approval, ChatAnswer::Answers(_)) => {
                return Err(ChatError::Refused(format!(
                    "request {request_id} is an approval: answer it with Allow or Deny"
                )));
            }
            (RequestKind::Question, ChatAnswer::Allow | ChatAnswer::Deny { .. }) => {
                return Err(ChatError::Refused(format!(
                    "request {request_id} is a question: answer it with Answers"
                )));
            }
            _ => {}
        }
        st.open_requests.remove(request_id);
        st.answer = Some((request_id.to_string(), answer.clone()));
        shared.wake.notify_all();
        Ok(())
    }

    fn interrupt(&self, chat: &ChatId) -> Result<(), ChatError> {
        let shared = self.known(chat)?;
        let mut st = shared.lock();
        if st.alive && st.running {
            st.interrupted = true;
            shared.wake.notify_all();
        }
        Ok(())
    }

    fn set_option(&self, chat: &ChatId, id: &str, value: &str) -> Result<(), ChatError> {
        let shared = self.known(chat)?;
        let Some(option) = self.declared(id) else {
            return Err(ChatError::Refused(format!("unknown option {id}")));
        };
        if !option.live {
            return Err(ChatError::Refused(format!(
                "option {id} is not live: it applies at the chat's next process"
            )));
        }
        check_value(option, value)?;
        shared
            .lock()
            .options
            .insert(id.to_string(), value.to_string());
        Ok(())
    }

    fn context_usage(&self, chat: &ChatId) -> Result<ContextUsage, ChatError> {
        let shared = self.known(chat)?;
        let used = shared.lock().used;
        Ok(ContextUsage {
            used_tokens: used,
            max_tokens: MAX_CONTEXT_TOKENS,
        })
    }

    fn background_tasks(&self, chat: &ChatId) -> Result<Vec<BackgroundTask>, ChatError> {
        let shared = self.known(chat)?;
        Ok(shared.lock().tasks.clone())
    }

    fn is_alive(&self, chat: &ChatId) -> Result<bool, ChatError> {
        Ok(self.find(chat).is_some_and(|c| c.lock().alive))
    }

    fn stop(&self, chat: &ChatId) -> Result<(), ChatError> {
        let Some(shared) = self.find(chat) else {
            return Ok(());
        };
        let mut st = shared.lock();
        if !st.alive {
            return Ok(());
        }
        st.push(ChatEvent::Exited {
            code: None,
            reason: "stopped".into(),
        });
        st.alive = false;
        shared.wake.notify_all();
        drop(st);
        self.registry().stops += 1;
        Ok(())
    }
}

// ------------------------------------------------------------------ the turns

/// What a message asks for.
enum Script {
    Plain,
    Tool,
    Big,
    Approval,
    Question,
    Task(u64),
    Slow(u64),
    Fail,
    Exit,
}

fn script_of(text: &str) -> Script {
    let t = text.trim();
    match t {
        "fake:tool" => Script::Tool,
        "fake:big" => Script::Big,
        "fake:approval" => Script::Approval,
        "fake:question" => Script::Question,
        "fake:fail" => Script::Fail,
        "fake:exit" => Script::Exit,
        _ => {
            let ms = |rest: &str| rest.trim().parse::<u64>().ok();
            if let Some(ms) = t.strip_prefix("fake:task ").and_then(ms) {
                Script::Task(ms)
            } else if let Some(ms) = t.strip_prefix("fake:slow ").and_then(ms) {
                Script::Slow(ms)
            } else {
                Script::Plain
            }
        }
    }
}

/// Run this turn and every turn queued behind it, on one thread.
fn spawn_turns(shared: Arc<Shared>, cause: TurnCause, text: String) {
    std::thread::spawn(move || {
        let mut next = Some((cause, text));
        while let Some((cause, text)) = next.take() {
            run_turn(&shared, cause, &text);
            let mut st = shared.lock();
            if st.alive {
                next = st.queue.pop_front();
            }
            if next.is_none() {
                st.running = false;
            }
        }
    });
}

/// How a turn's wait ended.
enum Wait<T> {
    Done(T),
    /// The turn was interrupted.
    Interrupted,
    /// The chat ended.
    Dead,
}

struct Turn<'a> {
    shared: &'a Shared,
    started: Instant,
    input: u64,
    output: u64,
}

fn chars(text: &str) -> u64 {
    text.chars().count() as u64
}

fn pieces(text: &str) -> Vec<String> {
    let all: Vec<char> = text.chars().collect();
    all.chunks(8).map(|c| c.iter().collect()).collect()
}

impl Turn<'_> {
    /// Stream `reply` as deltas, then one finished block. `pace` is the time for each delta. It
    /// returns how the turn goes on.
    fn say(&mut self, reply: &str, pace: Duration) -> Wait<()> {
        for piece in pieces(reply) {
            {
                let mut st = self.shared.lock();
                if !st.alive {
                    return Wait::Dead;
                }
                if st.interrupted {
                    return Wait::Interrupted;
                }
                st.push(ChatEvent::TextDelta { text: piece });
            }
            if let Some(w) = self.pause(pace) {
                return w;
            }
        }
        let mut st = self.shared.lock();
        if !st.alive {
            return Wait::Dead;
        }
        st.push(ChatEvent::Text {
            text: reply.to_string(),
        });
        self.output += chars(reply);
        Wait::Done(())
    }

    /// Wait `total`, in slices that take steered messages and notice an interrupt or a stop.
    fn pause<T>(&mut self, total: Duration) -> Option<Wait<T>> {
        let end = Instant::now() + total;
        loop {
            {
                let mut st = self.shared.lock();
                if !st.alive {
                    return Some(Wait::Dead);
                }
                if st.interrupted {
                    return Some(Wait::Interrupted);
                }
                self.take_steered(&mut st);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            std::thread::sleep(left.min(SLICE));
        }
    }

    /// Take the messages sent during the turn into it: their deltas and one finished block each.
    fn take_steered(&mut self, st: &mut ChatState) {
        for message in std::mem::take(&mut st.steer) {
            let reply = format!("steered: {message}");
            for piece in pieces(&reply) {
                st.push(ChatEvent::TextDelta { text: piece });
            }
            st.push(ChatEvent::Text {
                text: reply.clone(),
            });
            self.input += chars(&message);
            self.output += chars(&reply);
        }
    }

    fn tool(&mut self, output: &str) -> Wait<()> {
        let mut st = self.shared.lock();
        if !st.alive {
            return Wait::Dead;
        }
        let call_id = st.id("call");
        st.push(ChatEvent::ToolStarted {
            call_id: call_id.clone(),
            tool: "Bash".into(),
            input: cap("{}"),
        });
        st.push(ChatEvent::ToolFinished {
            call_id,
            output: cap(output),
            is_error: false,
        });
        Wait::Done(())
    }

    /// Ask a request and wait for its answer with no time limit.
    fn ask(
        &mut self,
        kind: RequestKind,
        event: impl FnOnce(String) -> ChatEvent,
    ) -> Wait<ChatAnswer> {
        let request_id = {
            let mut st = self.shared.lock();
            if !st.alive {
                return Wait::Dead;
            }
            let id = st.id("req");
            st.open_requests.insert(id.clone(), kind);
            st.push(event(id.clone()));
            id
        };
        let mut st = self.shared.lock();
        loop {
            if !st.alive {
                return Wait::Dead;
            }
            if st.interrupted {
                if st.open_requests.remove(&request_id).is_some() {
                    st.push(ChatEvent::RequestClosed { request_id });
                }
                return Wait::Interrupted;
            }
            self.take_steered(&mut st);
            if let Some((id, answer)) = st.answer.take() {
                if id == request_id {
                    return Wait::Done(answer);
                }
                st.answer = Some((id, answer));
            }
            st = self.shared.wake.wait(st).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn finish(&mut self, cause: TurnCause, is_error: bool, stop: &str) {
        let mut st = self.shared.lock();
        if !st.alive {
            return;
        }
        self.take_steered(&mut st);
        let usage = Usage {
            input_tokens: self.input,
            output_tokens: self.output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_micros: 3 * self.input + 15 * self.output,
        };
        st.used += self.input + self.output;
        st.interrupted = false;
        st.push(ChatEvent::TurnResult {
            cause,
            usage,
            is_error,
            stop: stop.into(),
            duration_ms: self.started.elapsed().as_millis() as u64,
            cache_ttl_seconds: Some(300),
        });
    }
}

fn run_turn(shared: &Arc<Shared>, cause: TurnCause, text: &str) {
    let mut turn = Turn {
        shared: shared.as_ref(),
        started: Instant::now(),
        input: if cause == TurnCause::Task {
            0
        } else {
            chars(text)
        },
        output: 0,
    };
    {
        let mut st = shared.lock();
        if !st.alive {
            return;
        }
        st.interrupted = false;
        st.push(ChatEvent::TurnStarted { cause });
    }
    let no_pace = Duration::ZERO;
    let ended = |w: Wait<()>, turn: &mut Turn| -> bool {
        match w {
            Wait::Done(()) => false,
            Wait::Interrupted => {
                turn.finish(cause, true, "interrupted");
                true
            }
            Wait::Dead => true,
        }
    };
    let plain = format!("fake reply: {}", text.trim_end());
    let mut task: Option<(u64, BackgroundTask)> = None;
    match script_of(text) {
        Script::Plain => {
            let reply = if cause == TurnCause::Task {
                "task finished".to_string()
            } else {
                plain
            };
            if ended(turn.say(&reply, no_pace), &mut turn) {
                return;
            }
        }
        Script::Tool | Script::Big => {
            let output = if matches!(script_of(text), Script::Big) {
                "x".repeat(20_000)
            } else {
                "ok".to_string()
            };
            if ended(turn.tool(&output), &mut turn) || ended(turn.say(&plain, no_pace), &mut turn) {
                return;
            }
        }
        Script::Approval => {
            let w = turn.ask(RequestKind::Approval, |request_id| {
                ChatEvent::ApprovalRequest {
                    request_id,
                    tool: "Bash".into(),
                    input: cap("{}"),
                    reason: None,
                }
            });
            let answer = match w {
                Wait::Done(a) => a,
                Wait::Interrupted => {
                    turn.finish(cause, true, "interrupted");
                    return;
                }
                Wait::Dead => return,
            };
            let reply = match answer {
                ChatAnswer::Deny { message } => format!("denied: {message}"),
                _ => {
                    if ended(turn.tool("ok"), &mut turn) {
                        return;
                    }
                    "approved".to_string()
                }
            };
            if ended(turn.say(&reply, no_pace), &mut turn) {
                return;
            }
        }
        Script::Question => {
            let w = turn.ask(RequestKind::Question, |request_id| ChatEvent::Question {
                request_id,
                items: vec![QuestionItem {
                    text: "Which one?".into(),
                    header: "Pick".into(),
                    options: ["A", "B"]
                        .iter()
                        .map(|l| QuestionChoice {
                            label: l.to_string(),
                            description: format!("option {l}"),
                        })
                        .collect(),
                    multi_select: false,
                }],
            });
            let answer = match w {
                Wait::Done(a) => a,
                Wait::Interrupted => {
                    turn.finish(cause, true, "interrupted");
                    return;
                }
                Wait::Dead => return,
            };
            let chosen = match answer {
                ChatAnswer::Answers(map) => map.values().next().cloned().unwrap_or_default(),
                _ => String::new(),
            };
            if ended(turn.say(&format!("answer: {chosen}"), no_pace), &mut turn) {
                return;
            }
        }
        Script::Task(ms) => {
            let mut st = shared.lock();
            if !st.alive {
                return;
            }
            let t = BackgroundTask {
                id: st.id("task"),
                kind: "fake".into(),
                description: "a fake task".into(),
                ambient: false,
            };
            st.tasks.push(t.clone());
            st.push(ChatEvent::TaskStarted { task: t.clone() });
            drop(st);
            task = Some((ms, t));
            if ended(turn.say(&plain, no_pace), &mut turn) {
                return;
            }
        }
        Script::Slow(ms) => {
            let count = pieces(&plain).len().max(1) as u64;
            let pace = Duration::from_millis(ms / count);
            if ended(turn.say(&plain, pace), &mut turn) {
                return;
            }
        }
        Script::Fail => {
            turn.finish(cause, true, "error_during_execution");
            return;
        }
        Script::Exit => {
            let mut st = shared.lock();
            st.push(ChatEvent::Exited {
                code: Some(1),
                reason: "fake:exit".into(),
            });
            st.alive = false;
            shared.wake.notify_all();
            return;
        }
    }
    turn.finish(cause, false, "end_turn");
    if let Some((ms, t)) = task {
        // The task runs after the turn that started it ends.
        let shared_again = Arc::clone(shared);
        std::thread::spawn(move || finish_task(shared_again, ms, t));
    }
}

/// After `ms`, finish the task, and queue the turn it causes.
fn finish_task(shared: Arc<Shared>, ms: u64, task: BackgroundTask) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if !shared.lock().alive {
            return;
        }
        std::thread::sleep(SLICE.min(end.saturating_duration_since(Instant::now())));
    }
    let mut st = shared.lock();
    if !st.alive {
        return;
    }
    st.tasks.retain(|t| t.id != task.id);
    st.push(ChatEvent::TaskFinished {
        task_id: task.id.clone(),
        end: TaskEnd::Completed,
        summary: "done".into(),
    });
    if st.running {
        st.queue.push_back((TurnCause::Task, String::new()));
        return;
    }
    st.running = true;
    drop(st);
    spawn_turns(shared, TurnCause::Task, String::new());
}
