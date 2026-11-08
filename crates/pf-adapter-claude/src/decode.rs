//! The stream decoder: what the CLI writes, as chat events. It is a pure state machine, with no
//! process and no clock, so the adapter's reader thread, the fixture tests and the engine's grammar
//! test run the same code.
//!
//! This file imports only `pf_engine`, `serde_json` and `std`, so `pf-engine`'s own test can include
//! it by path and decode the recordings without a dependency on this crate.
//!
//! One CLI message becomes zero or more [`Out`] values: chat events, an error response for an
//! inbound control request the adapter does not serve, a violation that ends the process, or a log
//! line. Facts the recordings of CLI 2.1.319 settled (`docs/adr/adr-0014-*`):
//!
//! - a turn starts at the `system/init` message and ends at `result`; a turn with no sent message
//!   behind it (no matching id in `user_message_uuids`) is a `Task` turn, which the CLI starts by
//!   itself when a background task finishes;
//! - a message sent during a turn is taken into it: one `init`, one `result`, both ids in the
//!   result's `user_message_uuids`;
//! - `result.total_cost_usd` and `result.modelUsage` are cumulative for the process, so a turn's
//!   usage is the difference to the last totals;
//! - `system/background_tasks_changed` replaces the whole task set.

use std::collections::{BTreeMap, BTreeSet};

use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::chat::{
    BackgroundTask, ChatEvent, InputKind, QuestionChoice, QuestionItem, TaskEnd, TurnCause, cap,
};
use serde_json::{Value, json};

/// The tools whose presence ends a process: a timer would start a turn the operator did not.
pub const TIMER_TOOLS: [&str; 5] = [
    "ScheduleWakeup",
    "CronCreate",
    "CronDelete",
    "CronList",
    "RemoteTrigger",
];

/// What one CLI message gave.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Event(ChatEvent),
    /// Answer an inbound control request with an error at once.
    Respond {
        request_id: String,
        error: String,
    },
    /// A check failed: the adapter stops the process, and the reason goes in `Exited`.
    Violation(String),
    /// Something to log: a protocol oddity that is not a violation.
    Log(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Approval,
    Question,
}

/// A request the CLI made and nobody answered yet.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenRequest {
    pub kind: RequestKind,
    /// The tool's original input, uncapped: an answer is written from it.
    pub input: Value,
}

/// The running totals of the process, as integers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Totals {
    cost_micros: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

struct Turn {
    cause: TurnCause,
    /// Tool calls started in this turn.
    calls: BTreeSet<String>,
}

pub struct Decoder {
    expected_version: String,
    artifacts_on: bool,
    session: String,
    turn: Option<Turn>,
    /// Sent messages by id, until a turn takes them.
    sends: BTreeMap<String, InputKind>,
    requests: BTreeMap<String, OpenRequest>,
    started_tasks: BTreeSet<String>,
    tasks: Vec<BackgroundTask>,
    totals: Totals,
}

/// The text of a JSON value for an event: the compact JSON text, capped by the caller.
fn json_text(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

/// The text of a tool result's `content`: a string, or the text blocks joined by a newline.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => json_text(other),
    }
}

fn whole(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| {
            v.as_f64()
                .map(|f| if f > 0.0 { f.round() as u64 } else { 0 })
        })
        .unwrap_or(0)
}

impl Decoder {
    /// `expected_version` is what `--version` printed. `artifacts_on` is the option's value: a
    /// mismatch with the tool list is only logged.
    pub fn new(expected_version: &str, session: &str, artifacts_on: bool) -> Decoder {
        Decoder {
            expected_version: expected_version.to_string(),
            artifacts_on,
            session: session.to_string(),
            turn: None,
            sends: BTreeMap::new(),
            requests: BTreeMap::new(),
            started_tasks: BTreeSet::new(),
            tasks: Vec::new(),
            totals: Totals::default(),
        }
    }

    /// Note a message the adapter sent, by the id it stamped, before it writes it.
    pub fn note_send(&mut self, uuid: &str, kind: InputKind) {
        self.sends.insert(uuid.to_string(), kind);
    }

    /// Is a turn open?
    pub fn in_turn(&self) -> bool {
        self.turn.is_some()
    }

    /// The runtime's session id as the last `init` reported it.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The background tasks as the last `background_tasks_changed` gave them.
    pub fn tasks(&self) -> Vec<BackgroundTask> {
        self.tasks.clone()
    }

    pub fn open_request(&self, request_id: &str) -> Option<&OpenRequest> {
        self.requests.get(request_id)
    }

    /// Take a request out of the open set, for its answer.
    pub fn close_request(&mut self, request_id: &str) -> Option<OpenRequest> {
        self.requests.remove(request_id)
    }

    /// Decode one message from the CLI.
    pub fn feed(&mut self, msg: &Value) -> Vec<Out> {
        let mut out = Vec::new();
        match str_of(msg, "type") {
            "system" => self.system(msg, &mut out),
            "stream_event" => self.stream_event(msg, &mut out),
            "assistant" => self.assistant(msg, &mut out),
            "user" => self.user(msg, &mut out),
            "result" => self.result(msg, &mut out),
            "control_request" => self.control_request(msg, &mut out),
            "control_cancel_request" => self.cancel(msg, &mut out),
            // Lifecycle and rate-limit notes, and anything new: no event.
            _ => {}
        }
        out
    }

    fn event(&self, out: &mut Vec<Out>, event: ChatEvent) {
        out.push(Out::Event(event));
    }

    fn system(&mut self, msg: &Value, out: &mut Vec<Out>) {
        match str_of(msg, "subtype") {
            "init" => self.init(msg, out),
            "background_tasks_changed" => {
                self.tasks = msg["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| BackgroundTask {
                        id: str_of(t, "task_id").to_string(),
                        kind: str_of(t, "task_type").to_string(),
                        description: str_of(t, "description").to_string(),
                        ambient: t["ambient"].as_bool().unwrap_or(false),
                    })
                    .collect();
            }
            "task_started" => {
                let id = str_of(msg, "task_id").to_string();
                if id.is_empty() || !self.started_tasks.insert(id.clone()) {
                    out.push(Out::Log(format!("task_started with no new task id {id:?}")));
                    return;
                }
                let ambient = self
                    .tasks
                    .iter()
                    .find(|t| t.id == id)
                    .is_some_and(|t| t.ambient);
                let task = BackgroundTask {
                    id,
                    kind: str_of(msg, "task_type").to_string(),
                    description: str_of(msg, "description").to_string(),
                    ambient,
                };
                self.event(out, ChatEvent::TaskStarted { task });
            }
            "task_notification" => {
                let id = str_of(msg, "task_id").to_string();
                if !self.started_tasks.remove(&id) {
                    out.push(Out::Log(format!(
                        "task_notification for task {id:?} that this process did not start, or that already finished"
                    )));
                    return;
                }
                let end = match str_of(msg, "status") {
                    "completed" => TaskEnd::Completed,
                    "stopped" | "killed" | "cancelled" => TaskEnd::Stopped,
                    _ => TaskEnd::Failed,
                };
                self.event(
                    out,
                    ChatEvent::TaskFinished {
                        task_id: id,
                        end,
                        summary: str_of(msg, "summary").to_string(),
                    },
                );
            }
            _ => {}
        }
    }

    fn init(&mut self, msg: &Value, out: &mut Vec<Out>) {
        // The checks come at each init, after the fact; the environment allowlist is the guard.
        let key = str_of(msg, "apiKeySource");
        if key != "none" {
            out.push(Out::Violation(format!(
                "check failed: apiKeySource is {key:?}, not \"none\": the chat must run on the subscription login"
            )));
            return;
        }
        let tools: Vec<&str> = msg["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if let Some(timer) = TIMER_TOOLS.iter().find(|t| tools.contains(t)) {
            out.push(Out::Violation(format!(
                "check failed: the tool list holds the timer tool {timer}"
            )));
            return;
        }
        let version = str_of(msg, "claude_code_version");
        if version != self.expected_version {
            out.push(Out::Violation(format!(
                "check failed: init reports version {version:?}, but --version printed {:?}",
                self.expected_version
            )));
            return;
        }
        let has_artifacts = tools.iter().any(|t| t.starts_with("Artifact"));
        if has_artifacts != self.artifacts_on {
            out.push(Out::Log(format!(
                "the Artifact tools are {} in the tool list, but the artifacts option is {}",
                if has_artifacts { "present" } else { "absent" },
                if self.artifacts_on { "on" } else { "off" }
            )));
        }
        if self.turn.is_some() {
            out.push(Out::Log(
                "init inside an open turn: no new turn".to_string(),
            ));
            return;
        }
        // The cause: the first message this init consumed, else the CLI woke itself for a task.
        let cause = msg["user_message_uuids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find_map(|u| self.sends.get(u).copied())
            .map_or(TurnCause::Task, |kind| match kind {
                InputKind::Operator => TurnCause::Operator,
                InputKind::Notice => TurnCause::Notice,
            });
        self.turn = Some(Turn {
            cause,
            calls: BTreeSet::new(),
        });
        self.event(out, ChatEvent::TurnStarted { cause });
        let session = str_of(msg, "session_id");
        if self.session.is_empty() {
            // A recording has no argv: its first init names the session.
            self.session = session.to_string();
        }
        if !session.is_empty() && session != self.session {
            self.session = session.to_string();
            self.event(
                out,
                ChatEvent::SessionMoved {
                    session: SessionRef(session.to_string()),
                },
            );
        }
    }

    fn stream_event(&mut self, msg: &Value, out: &mut Vec<Out>) {
        if !msg["parent_tool_use_id"].is_null() || self.turn.is_none() {
            return;
        }
        let event = &msg["event"];
        let delta = &event["delta"];
        if event["type"] == "content_block_delta"
            && delta["type"] == "text_delta"
            && let Some(text) = delta["text"].as_str()
        {
            self.event(
                out,
                ChatEvent::TextDelta {
                    text: text.to_string(),
                },
            );
        }
    }

    fn assistant(&mut self, msg: &Value, out: &mut Vec<Out>) {
        if !msg["parent_tool_use_id"].is_null() {
            return;
        }
        let Some(turn) = self.turn.as_mut() else {
            out.push(Out::Log("assistant message outside a turn".to_string()));
            return;
        };
        let mut events = Vec::new();
        for block in msg["message"]["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => events.push(ChatEvent::Text {
                    text: str_of(block, "text").to_string(),
                }),
                Some("tool_use") => {
                    let id = str_of(block, "id").to_string();
                    turn.calls.insert(id.clone());
                    events.push(ChatEvent::ToolStarted {
                        call_id: id,
                        tool: str_of(block, "name").to_string(),
                        input: cap(&json_text(&block["input"])),
                    });
                }
                // Thinking blocks and the rest give no event.
                _ => {}
            }
        }
        for e in events {
            self.event(out, e);
        }
    }

    fn user(&mut self, msg: &Value, out: &mut Vec<Out>) {
        if !msg["parent_tool_use_id"].is_null() {
            return;
        }
        for block in msg["message"]["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let id = str_of(block, "tool_use_id").to_string();
            let known = self.turn.as_ref().is_some_and(|t| t.calls.contains(&id));
            if !known {
                out.push(Out::Log(format!(
                    "tool result for call {id:?} that this turn did not start"
                )));
                continue;
            }
            self.event(
                out,
                ChatEvent::ToolFinished {
                    call_id: id,
                    output: cap(&result_text(&block["content"])),
                    is_error: block["is_error"].as_bool().unwrap_or(false),
                },
            );
        }
    }

    fn control_request(&mut self, msg: &Value, out: &mut Vec<Out>) {
        let id = str_of(msg, "request_id").to_string();
        let request = &msg["request"];
        let subtype = str_of(request, "subtype");
        if subtype != "can_use_tool" {
            out.push(Out::Log(format!(
                "inbound control request {subtype:?} refused with an error response"
            )));
            out.push(Out::Respond {
                request_id: id,
                error: format!("unsupported control request: {subtype}"),
            });
            return;
        }
        if self.turn.is_none() || self.requests.contains_key(&id) || id.is_empty() {
            out.push(Out::Log(format!(
                "can_use_tool {id:?} outside a turn or with a repeated id"
            )));
            out.push(Out::Respond {
                request_id: id,
                error: "cannot ask outside a turn".to_string(),
            });
            return;
        }
        let tool = str_of(request, "tool_name").to_string();
        let input = request["input"].clone();
        if tool == "AskUserQuestion" {
            let items = input["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|q| QuestionItem {
                    text: str_of(q, "question").to_string(),
                    header: str_of(q, "header").to_string(),
                    options: q["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|o| QuestionChoice {
                            label: str_of(o, "label").to_string(),
                            description: str_of(o, "description").to_string(),
                        })
                        .collect(),
                    multi_select: q["multiSelect"].as_bool().unwrap_or(false),
                })
                .collect();
            self.requests.insert(
                id.clone(),
                OpenRequest {
                    kind: RequestKind::Question,
                    input,
                },
            );
            self.event(
                out,
                ChatEvent::Question {
                    request_id: id,
                    items,
                },
            );
            return;
        }
        // The reason: the CLI's own decision text, else the tool's own description of the call.
        let reason = ["decision_reason", "description"]
            .iter()
            .find_map(|k| request[*k].as_str().filter(|s| !s.is_empty()))
            .map(str::to_string);
        let capped = cap(&json_text(&input));
        self.requests.insert(
            id.clone(),
            OpenRequest {
                kind: RequestKind::Approval,
                input,
            },
        );
        self.event(
            out,
            ChatEvent::ApprovalRequest {
                request_id: id,
                tool,
                input: capped,
                reason,
            },
        );
    }

    fn cancel(&mut self, msg: &Value, out: &mut Vec<Out>) {
        let id = str_of(msg, "request_id").to_string();
        if self.requests.remove(&id).is_some() {
            self.event(out, ChatEvent::RequestClosed { request_id: id });
        }
    }

    fn result(&mut self, msg: &Value, out: &mut Vec<Out>) {
        let Some(turn) = self.turn.take() else {
            out.push(Out::Log("result outside a turn".to_string()));
            return;
        };
        let usage = self.turn_usage(msg, out);
        // A closed turn takes its messages with it.
        for u in msg["user_message_uuids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            self.sends.remove(u);
        }
        let is_error = msg["is_error"].as_bool().unwrap_or(false);
        let subtype = str_of(msg, "subtype");
        // An error result names its kind; a normal one names why the model stopped.
        let stop = if is_error && !subtype.is_empty() && subtype != "success" {
            subtype.to_string()
        } else {
            msg["stop_reason"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(subtype)
                .to_string()
        };
        let created = &msg["usage"]["cache_creation"];
        let cache_ttl_seconds = if whole(&created["ephemeral_1h_input_tokens"]) > 0 {
            Some(3600)
        } else if whole(&created["ephemeral_5m_input_tokens"]) > 0 {
            Some(300)
        } else {
            None
        };
        self.event(
            out,
            ChatEvent::TurnResult {
                cause: turn.cause,
                usage,
                is_error,
                stop,
                duration_ms: whole(&msg["duration_ms"]),
                cache_ttl_seconds,
            },
        );
    }

    /// The turn's own usage: each running total minus the last one. A total that went down is a
    /// protocol error: it is logged, counts as zero and becomes the new baseline.
    fn turn_usage(&mut self, msg: &Value, out: &mut Vec<Out>) -> Usage {
        let mut now = Totals {
            cost_micros: (msg["total_cost_usd"].as_f64().unwrap_or(0.0) * 1_000_000.0)
                .round()
                .max(0.0) as u64,
            ..Totals::default()
        };
        for model in msg["modelUsage"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
        {
            now.input += whole(&model["inputTokens"]);
            now.output += whole(&model["outputTokens"]);
            now.cache_read += whole(&model["cacheReadInputTokens"]);
            now.cache_write += whole(&model["cacheCreationInputTokens"]);
        }
        let mut diff = |name: &str, last: u64, total: u64| -> u64 {
            if total < last {
                out.push(Out::Log(format!(
                    "protocol error: the cumulative {name} went down from {last} to {total}; it counts as zero for this turn"
                )));
                0
            } else {
                total - last
            }
        };
        let usage = Usage {
            input_tokens: diff("input tokens", self.totals.input, now.input),
            output_tokens: diff("output tokens", self.totals.output, now.output),
            cache_read_tokens: diff("cache read tokens", self.totals.cache_read, now.cache_read),
            cache_write_tokens: diff(
                "cache write tokens",
                self.totals.cache_write,
                now.cache_write,
            ),
            cost_micros: diff("cost", self.totals.cost_micros, now.cost_micros),
        };
        self.totals = now;
        usage
    }
}

// ------------------------------------------------------------------ recordings

/// The events a recording gives: the lines of a fixture body, decoded as the adapter decodes them.
/// A message the recorder sent (`to` is `cli`) is noted as an operator send by its id.
/// Returns the events and the log lines.
pub fn decode_recording(
    version: &str,
    session: &str,
    body: &[(String, Value)],
) -> (Vec<ChatEvent>, Vec<String>) {
    let mut decoder = Decoder::new(version, session, false);
    let mut events = Vec::new();
    let mut logs = Vec::new();
    for (to, msg) in body {
        if to == "cli" {
            if msg["type"] == "user"
                && let Some(u) = msg["uuid"].as_str()
            {
                decoder.note_send(u, InputKind::Operator);
            }
            continue;
        }
        for o in decoder.feed(msg) {
            match o {
                Out::Event(e) => events.push(e),
                Out::Log(l) => logs.push(l),
                Out::Violation(v) => logs.push(format!("violation: {v}")),
                Out::Respond { request_id, error } => {
                    logs.push(format!("respond {request_id}: {error}"));
                }
            }
        }
    }
    (events, logs)
}

/// The JSON of an error response to an inbound control request.
pub fn error_response(request_id: &str, error: &str) -> Value {
    json!({"type": "control_response", "response": {"subtype": "error", "request_id": request_id, "error": error}})
}
