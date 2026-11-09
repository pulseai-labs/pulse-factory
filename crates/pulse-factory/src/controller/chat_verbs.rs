//! The chat verbs: `session_start`, `session_send`, `session_answer`, `session_interrupt`,
//! `session_set`, `session_end`, `session_redact`, `session_list` and `session_items`
//! (`docs/contracts/cli.md`, `docs/contracts/api.md`; ADR-0003, ADR-0004, ADR-0005, ADR-0015).
//!
//! Each state-changing verb takes a command ID and goes through `run_command`: one admission under
//! `@controller`, one `controller.ledger` line, a refusal that is an admission of its own, a replay
//! that returns the first record and makes no port call, and a conflict that names the ID. The
//! content holds no message text: a send stores the text's SHA-256.
//!
//! A verb waits for no reply. It sends the message and answers; the operator reads the chat with
//! `session items`, and the app reads it on the stream.

use std::collections::BTreeMap;

use pf_api::types::{self as wire, SCHEMA_VERSION};
use pf_engine::claim::sha256_hex;
use pf_engine::port::chat::{ChatAnswer, InputKind};
use pf_engine::port::store::{CONTROLLER_SCOPE, ChatItemKind, ChatSessionRow, Effect, Store};
use pf_ipc::Response;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::chats::{LiveChat, OpenFailure, check_launch, check_option_value, effective_options};
use super::commands::{Plan, check_command_id, field, now};
use super::sessions::{
    ChatItemRedactArgs, ChatSessionEndArgs, ChatSessionStartArgs, check_session_id,
};
use super::worktrees::WorktreeLive;
use super::{Controller, lock};

/// The most bytes of one sent text: 256 KiB.
pub const SEND_MAX_BYTES: usize = 256 * 1024;

/// The most characters of a deny message.
pub const DENY_MAX_CHARS: usize = 1000;

/// The most items `session_items` answers.
const ITEMS_MAX: u32 = 500;

#[derive(Serialize)]
struct SendContent<'a> {
    session_id: &'a str,
    text_sha256: &'a str,
}

#[derive(Serialize)]
struct AnswerContent<'a> {
    answer: &'a wire::ChatAnswerArg,
    seq: u64,
    session_id: &'a str,
}

#[derive(Serialize)]
struct InterruptContent<'a> {
    session_id: &'a str,
}

#[derive(Serialize)]
struct SetContent<'a> {
    effort: &'a Option<String>,
    model: &'a Option<String>,
    options: &'a Option<BTreeMap<String, String>>,
    permission_mode: &'a Option<String>,
    session_id: &'a str,
}

fn parse<T: DeserializeOwned>(args: &Value) -> Result<T, Response> {
    serde_json::from_value(args.clone())
        .map_err(|e| Response::err(format!("the arguments are not readable: {e}")))
}

fn ok_json<T: Serialize>(value: &T) -> Response {
    Response::ok(serde_json::to_value(value).unwrap_or(Value::Null))
}

fn answer_to_port(answer: &wire::ChatAnswerArg) -> ChatAnswer {
    match answer {
        wire::ChatAnswerArg::Allow => ChatAnswer::Allow,
        wire::ChatAnswerArg::Deny { message } => ChatAnswer::Deny {
            message: message.clone(),
        },
        wire::ChatAnswerArg::Answers(a) => ChatAnswer::Answers(a.clone()),
    }
}

/// The numeric part of a session ID, for creation order.
fn session_number(id: &str) -> u64 {
    id.rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(u64::MAX)
}

impl Controller {
    /// Does the store know this command ID already? Then the verb is a replay or a conflict, and the
    /// store decides which.
    fn command_known(&self, command_id: &str) -> Result<bool, String> {
        Ok(self
            .store
            .admission(CONTROLLER_SCOPE, command_id)
            .map_err(|e| e.to_string())?
            .is_some())
    }

    fn session_row(&self, id: &str) -> Result<Option<ChatSessionRow>, String> {
        self.store.chat_session(id).map_err(|e| e.to_string())
    }

    /// The launch check of an open, before the admission: the runtime is on this server and its
    /// settings pass the declared options. A note of the degrade is printed.
    pub(super) fn launch_ready(
        &self,
        runtime: &str,
        model: &str,
        effort: &str,
        permission_mode: &str,
        options: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let port = self.chat_port(runtime)?;
        let notes = check_launch(port.as_ref(), model, effort, permission_mode, options)?;
        for note in notes {
            eprintln!("{note}");
        }
        Ok(())
    }

    /// The checks of an open of a session that has no process: the launch check, the live worktree
    /// of a resume, and the cap.
    fn ready_to_open(&self, row: &ChatSessionRow) -> Result<(), String> {
        let agent = self
            .agent(&row.agent_id)?
            .ok_or_else(|| format!("no agent {}", row.agent_id))?;
        self.launch_ready(
            &row.runtime,
            &row.model,
            &row.effort,
            &row.permission_mode,
            &effective_options(row, &agent),
        )?;
        if row.runtime_session_ref.is_some() {
            let project = self
                .store
                .project(&row.project_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no project {}", row.project_id))?;
            let worktree = self
                .store
                .worktree(&row.worktree_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no worktree {}", row.worktree_id))?;
            if let WorktreeLive::Gone(why) =
                self.check_worktree(&project, &worktree.path, Some(&worktree))?
            {
                return Err(format!("the worktree {} is gone: {why}", worktree.path));
            }
        }
        match self.cap_refusal() {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    /// An open failed after its admission: the notice, the "context lost" mark when it applies,
    /// and the text the verb answers with.
    fn open_failed(&self, session_id: &str, failure: OpenFailure) -> String {
        let text = match &failure {
            OpenFailure::ContextLost(why) => {
                lock(&self.chats.lost).insert(session_id.to_string(), why.clone());
                let text = format!("context lost: {why}; end this session and start a new one");
                self.notice(session_id, "context_lost", &text);
                text
            }
            OpenFailure::Refused(why) | OpenFailure::Failed(why) => {
                self.notice(
                    session_id,
                    "open_failed",
                    &format!("the chat process did not open: {why}"),
                );
                why.clone()
            }
        };
        self.emit_session(session_id);
        text
    }

    /// Run `f` on the live process of a session, holding the session's write lock. The process is
    /// opened first when there is none: the next message of the operator resumes a released one.
    fn with_live<R>(
        &self,
        row: &ChatSessionRow,
        f: impl FnOnce(&std::sync::Arc<LiveChat>) -> R,
    ) -> Result<R, OpenFailure> {
        for _ in 0..3 {
            let live = match self.live_chat(&row.id) {
                Some(l) => l,
                None => self.open_chat(row)?,
            };
            let _io = lock(&live.io);
            if live.is_stopping() {
                continue;
            }
            return Ok(f(&live));
        }
        Err(OpenFailure::Failed(
            "the chat process kept stopping".to_string(),
        ))
    }

    /// The record a changing verb answers with.
    fn changed(&self, session_id: &str, replayed: bool, note: Option<String>) -> Response {
        let row = match self.session_row(session_id) {
            Ok(Some(r)) => r,
            Ok(None) => return Response::err(format!("no chat session {session_id}")),
            Err(e) => return Response::err(e),
        };
        match self.session_record(&row, true) {
            Ok(session) => ok_json(&wire::SessionChanged {
                schema_version: SCHEMA_VERSION,
                replayed,
                session,
                note,
            }),
            Err(why) => Response::err(why),
        }
    }

    // ------------------------------------------------------------------ start

    pub(super) fn verb_session_start(&self, args: &Value) -> Response {
        let args: wire::SessionStartArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        let started = match self.start_chat_session_checked(
            &ChatSessionStartArgs {
                project_id: args.project_id,
                agent_id: args.agent_id,
                command_id: args.command_id,
            },
            true,
        ) {
            Ok(s) => s,
            Err(why) => return Response::err(why),
        };
        if started.replayed {
            return ok_json(&started);
        }
        // The process opens at the start, so the runtime's session ID is stored from the first
        // moment and the composer's live controls work before the first message.
        let id = started.session.id.clone();
        match self.open_chat(&started.session) {
            Ok(_) => {
                let _ = self
                    .store
                    .chat_session(&id)
                    .ok()
                    .flatten()
                    .map(|row| self.session_record(&row, true));
                let session = self
                    .store
                    .chat_session(&id)
                    .ok()
                    .flatten()
                    .unwrap_or(started.session);
                ok_json(&super::sessions::ChatSessionStarted { session, ..started })
            }
            Err(failure) => {
                let why = self.open_failed(&id, failure);
                Response::err(format!(
                    "session {id} started, but its chat process did not open: {why}"
                ))
            }
        }
    }

    // ------------------------------------------------------------------ send

    pub(super) fn verb_session_send(&self, args: &Value) -> Response {
        let args: wire::SessionSendArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        if let Err(why) = check_session_id(&args.session_id) {
            return Response::err(why);
        }
        let bytes = args.text.len();
        if bytes == 0 || bytes > SEND_MAX_BYTES {
            return Response::err(format!(
                "a sent text is 1 byte to {SEND_MAX_BYTES} bytes, and this one has {bytes}"
            ));
        }
        let sha = sha256_hex(args.text.as_bytes());
        let content = serde_json::to_string(&SendContent {
            session_id: &args.session_id,
            text_sha256: &sha,
        })
        .unwrap_or_default();
        let plan = match self.command_known(&args.command_id) {
            Ok(true) => Plan::Refuse("a replay".to_string()),
            Ok(false) => match self.plan_send(&args.session_id, bytes, &args.command_id) {
                Ok(p) => p,
                Err(why) => return Response::err(why),
            },
            Err(why) => return Response::err(why),
        };
        let replayed = match self.run_command("chat_send", &args.command_id, content, plan) {
            Ok((_, replayed)) => replayed,
            Err(why) => return Response::err(why),
        };
        if replayed {
            return self.changed(&args.session_id, true, None);
        }
        self.do_send(&args.session_id, &args.text)
    }

    fn plan_send(&self, id: &str, bytes: usize, command_id: &str) -> Result<Plan, String> {
        let Some(row) = self.session_row(id)? else {
            return Ok(Plan::Refuse(format!("no chat session {id}")));
        };
        if row.ended_at.is_some() {
            return Ok(Plan::Refuse(format!("chat session {id} has ended")));
        }
        if let Some(why) = lock(&self.chats.lost).get(id) {
            return Ok(Plan::Refuse(format!(
                "chat session {id}: context lost: {why}; end it and start a new one"
            )));
        }
        if self.live_chat(id).is_none()
            && let Err(why) = self.ready_to_open(&row)
        {
            return Ok(Plan::Refuse(why));
        }
        Ok(Plan::Admit {
            effects: Vec::new(),
            subject: id.to_string(),
            line: format!(
                "chat_send {} admitted bytes={bytes} command={command_id}",
                field(id)
            ),
        })
    }

    /// The act after an admitted send: open the process when there is none, write the operator's
    /// message to the log, and send it. A failure here is the answer's note and a notice.
    fn do_send(&self, session_id: &str, text: &str) -> Response {
        let row = match self.session_row(session_id) {
            Ok(Some(r)) => r,
            Ok(None) => return Response::err(format!("no chat session {session_id}")),
            Err(e) => return Response::err(e),
        };
        self.restart_for_pending_options(&row);
        let sent = self.with_live(&row, |live| {
            let body = wire::ChatItemBody::OperatorMessage {
                text: text.to_string(),
            };
            let appended = self.append_item(super::chat_view::new_item(session_id, &body));
            if let Err(why) = appended {
                return Err(format!("the message is not stored: {why}"));
            }
            lock(&live.inner).idle_since = std::time::SystemTime::now();
            live.port
                .send(&live.chat, InputKind::Operator, text)
                .map_err(|e| e.to_string())
        });
        match sent {
            Err(failure) => Response::err(self.open_failed(session_id, failure)),
            Ok(Err(why)) => {
                self.notice(
                    session_id,
                    "open_failed",
                    &format!("the message was not delivered: {why}"),
                );
                self.changed(session_id, false, Some(why))
            }
            Ok(Ok(())) => {
                self.emit_session(session_id);
                self.changed(session_id, false, None)
            }
        }
    }

    /// A runtime option that is not live applies at the next process. At a send to an idle process
    /// the controller stops it, and the send reopens it with `--resume` and the new value (one cache
    /// write). While a holder holds the process, the change waits for the next open.
    fn restart_for_pending_options(&self, row: &ChatSessionRow) {
        let Some(live) = self.live_chat(&row.id) else {
            return;
        };
        let Ok(Some(agent)) = self.agent(&row.agent_id) else {
            return;
        };
        let wanted = effective_options(row, &agent);
        let differs = {
            let inner = lock(&live.inner);
            wanted.iter().any(|(k, v)| inner.applied.get(k) != Some(v))
        };
        if !differs || !live.is_idle() {
            return;
        }
        let _io = lock(&live.io);
        if live.is_stopping() || !live.is_idle() {
            return;
        }
        self.stop_chat(&live, "option");
        self.notice(
            &row.id,
            "option_restart",
            "the chat process restarted to apply a changed option",
        );
    }

    // ------------------------------------------------------------------ answer

    pub(super) fn verb_session_answer(&self, args: &Value) -> Response {
        let args: wire::SessionAnswerArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        if let Err(why) = check_session_id(&args.session_id) {
            return Response::err(why);
        }
        let content = serde_json::to_string(&AnswerContent {
            answer: &args.answer,
            seq: args.seq,
            session_id: &args.session_id,
        })
        .unwrap_or_default();
        let plan = match self.command_known(&args.command_id) {
            Ok(true) => Plan::Refuse("a replay".to_string()),
            Ok(false) => match self.plan_answer(&args) {
                Ok(p) => p,
                Err(why) => return Response::err(why),
            },
            Err(why) => return Response::err(why),
        };
        let replayed = match self.run_command("chat_answer", &args.command_id, content, plan) {
            Ok((_, replayed)) => replayed,
            Err(why) => return Response::err(why),
        };
        if replayed {
            return self.changed(&args.session_id, true, None);
        }
        self.do_answer(&args)
    }

    fn plan_answer(&self, args: &wire::SessionAnswerArgs) -> Result<Plan, String> {
        let (id, seq) = (args.session_id.as_str(), args.seq);
        let refuse = |why: String| Ok(Plan::Refuse(why));
        let Some(row) = self.session_row(id)? else {
            return refuse(format!("no chat session {id}"));
        };
        if row.ended_at.is_some() {
            return refuse(format!("chat session {id} has ended"));
        }
        if self.live_chat(id).is_none() {
            return refuse(format!(
                "chat session {id} has no process; its requests were closed as lost"
            ));
        }
        let Some(item) = self.store.chat_item(id, seq).map_err(|e| e.to_string())? else {
            return refuse(format!("chat session {id} has no item {seq}"));
        };
        let Some(kind) = ChatItemKind::parse(&item.kind).filter(|k| k.is_request()) else {
            return refuse(format!(
                "item {seq} of {id} is not an approval or a question"
            ));
        };
        let pending = self.store.pending_requests(id).map_err(|e| e.to_string())?;
        if !pending.iter().any(|p| p.seq == seq) {
            return refuse(format!(
                "request {seq} of {id} is already answered, closed or lost"
            ));
        }
        let label = match (&args.answer, kind) {
            (wire::ChatAnswerArg::Allow, ChatItemKind::ApprovalRequest) => "allow",
            (wire::ChatAnswerArg::Deny { message }, ChatItemKind::ApprovalRequest) => {
                if message.contains(['\n', '\r']) || message.chars().count() > DENY_MAX_CHARS {
                    return refuse(format!(
                        "a deny message has no line break and at most {DENY_MAX_CHARS} characters"
                    ));
                }
                "deny"
            }
            (wire::ChatAnswerArg::Answers(a), ChatItemKind::QuestionRequest) => {
                if a.is_empty() {
                    return refuse("a question answer needs at least one answer".to_string());
                }
                "answers"
            }
            (wire::ChatAnswerArg::Answers(_), _) => {
                return refuse(format!(
                    "request {seq} is an approval: answer it with --allow or --deny"
                ));
            }
            (_, _) => {
                return refuse(format!(
                    "request {seq} is a question: answer it with --answer"
                ));
            }
        };
        Ok(Plan::Admit {
            effects: Vec::new(),
            subject: id.to_string(),
            line: format!(
                "chat_answer {} seq={seq} admitted answer={label} command={}",
                field(id),
                args.command_id
            ),
        })
    }

    fn do_answer(&self, args: &wire::SessionAnswerArgs) -> Response {
        let (id, seq) = (args.session_id.as_str(), args.seq);
        let Some(live) = self.live_chat(id) else {
            return self.changed(
                id,
                false,
                Some("the chat process is gone; the request was closed as lost".to_string()),
            );
        };
        let _io = lock(&live.io);
        let request_id = lock(&live.inner)
            .requests
            .get(&seq)
            .map(|r| r.request_id.clone());
        let Some(request_id) = request_id else {
            return self.changed(
                id,
                false,
                Some("the request is not pending in the process any more".to_string()),
            );
        };
        if let Err(e) = live
            .port
            .answer(&live.chat, &request_id, &answer_to_port(&args.answer))
        {
            let why = e.to_string();
            self.notice(
                id,
                "open_failed",
                &format!("the answer was not delivered: {why}"),
            );
            return self.changed(id, false, Some(why));
        }
        let body = match &args.answer {
            wire::ChatAnswerArg::Allow => wire::ChatItemBody::ApprovalAnswer {
                outcome: wire::AnswerOutcome::Answered,
                reason: None,
                decision: Some("allow".to_string()),
                message: None,
            },
            wire::ChatAnswerArg::Deny { message } => wire::ChatItemBody::ApprovalAnswer {
                outcome: wire::AnswerOutcome::Answered,
                reason: None,
                decision: Some("deny".to_string()),
                message: Some(message.clone()),
            },
            wire::ChatAnswerArg::Answers(a) => wire::ChatItemBody::QuestionAnswer {
                outcome: wire::AnswerOutcome::Answered,
                reason: None,
                answers: Some(a.clone()),
            },
        };
        let mut item = super::chat_view::new_item(id, &body);
        item.answers_seq = Some(seq);
        let appended = self.append_item(item);
        {
            let mut inner = lock(&live.inner);
            inner.requests.remove(&seq);
            if inner.requests.is_empty() && inner.turn_since.is_none() {
                inner.idle_since = std::time::SystemTime::now();
            }
        }
        self.sync_state(&live);
        self.emit_session(id);
        match appended {
            Ok(_) => self.changed(id, false, None),
            Err(why) => self.changed(
                id,
                false,
                Some(format!("the answer item is not stored: {why}")),
            ),
        }
    }

    // ------------------------------------------------------------------ interrupt

    pub(super) fn verb_session_interrupt(&self, args: &Value) -> Response {
        let args: wire::SessionInterruptArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        if let Err(why) = check_session_id(&args.session_id) {
            return Response::err(why);
        }
        let content = serde_json::to_string(&InterruptContent {
            session_id: &args.session_id,
        })
        .unwrap_or_default();
        let plan = match self.command_known(&args.command_id) {
            Ok(true) => Plan::Refuse("a replay".to_string()),
            Ok(false) => match self.session_row(&args.session_id) {
                Ok(None) => Plan::Refuse(format!("no chat session {}", args.session_id)),
                Ok(Some(r)) if r.ended_at.is_some() => {
                    Plan::Refuse(format!("chat session {} has ended", args.session_id))
                }
                Ok(Some(_)) => Plan::Admit {
                    effects: Vec::new(),
                    subject: args.session_id.clone(),
                    line: format!(
                        "chat_interrupt {} admitted command={}",
                        field(&args.session_id),
                        args.command_id
                    ),
                },
                Err(why) => return Response::err(why),
            },
            Err(why) => return Response::err(why),
        };
        let replayed = match self.run_command("chat_interrupt", &args.command_id, content, plan) {
            Ok((_, replayed)) => replayed,
            Err(why) => return Response::err(why),
        };
        if replayed {
            return self.changed(&args.session_id, true, None);
        }
        // It ends a running turn and does nothing else.
        let mut note = None;
        if let Some(live) = self.live_chat(&args.session_id)
            && lock(&live.inner).turn_since.is_some()
            && let Err(e) = live.port.interrupt(&live.chat)
        {
            note = Some(e.to_string());
        }
        self.changed(&args.session_id, false, note)
    }

    // ------------------------------------------------------------------ set

    pub(super) fn verb_session_set(&self, args: &Value) -> Response {
        let args: wire::SessionSetArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        if let Err(why) = check_session_id(&args.session_id) {
            return Response::err(why);
        }
        let content = serde_json::to_string(&SetContent {
            effort: &args.effort,
            model: &args.model,
            options: &args.options,
            permission_mode: &args.permission_mode,
            session_id: &args.session_id,
        })
        .unwrap_or_default();
        let plan = match self.command_known(&args.command_id) {
            Ok(true) => Plan::Refuse("a replay".to_string()),
            Ok(false) => match self.plan_set(&args) {
                Ok(p) => p,
                Err(why) => return Response::err(why),
            },
            Err(why) => return Response::err(why),
        };
        let replayed = match self.run_command("chat_session_set", &args.command_id, content, plan) {
            Ok((_, replayed)) => replayed,
            Err(why) => return Response::err(why),
        };
        if replayed {
            return self.changed(&args.session_id, true, None);
        }
        let note = self.apply_set(&args);
        self.emit_session(&args.session_id);
        self.changed(&args.session_id, false, note)
    }

    fn plan_set(&self, args: &wire::SessionSetArgs) -> Result<Plan, String> {
        let id = args.session_id.as_str();
        let refuse = |why: String| Ok(Plan::Refuse(why));
        let Some(row) = self.session_row(id)? else {
            return refuse(format!("no chat session {id}"));
        };
        if row.ended_at.is_some() {
            return refuse(format!("chat session {id} has ended"));
        }
        if args.model.is_none()
            && args.effort.is_none()
            && args.permission_mode.is_none()
            && args.options.as_ref().is_none_or(BTreeMap::is_empty)
        {
            return refuse(
                "nothing to set: give at least one of model, effort, mode or option".to_string(),
            );
        }
        let port = match self.chat_port(&row.runtime) {
            Ok(p) => p,
            Err(why) => return refuse(why),
        };
        let declared = port.options();
        for (option_id, value) in [
            ("model", &args.model),
            ("effort", &args.effort),
            ("permission_mode", &args.permission_mode),
        ] {
            let Some(value) = value else { continue };
            let Some(option) = declared.iter().find(|o| o.id == option_id) else {
                return refuse(format!("this runtime declares no option {option_id}"));
            };
            if let Err(why) = check_option_value(option, value) {
                return refuse(why);
            }
        }
        let mut option_changes = None;
        if let Some(changes) = args.options.as_ref().filter(|o| !o.is_empty()) {
            for (option_id, value) in changes {
                if matches!(option_id.as_str(), "model" | "effort" | "permission_mode") {
                    return refuse(format!(
                        "{option_id} has its own flag; --option is for the runtime's other options"
                    ));
                }
                let Some(option) = declared.iter().find(|o| &o.id == option_id) else {
                    let ids: Vec<&str> = declared.iter().map(|o| o.id.as_str()).collect();
                    return refuse(format!(
                        "the option {option_id} is not declared by this runtime; it declares: {}",
                        ids.join(", ")
                    ));
                };
                if let Err(why) = check_option_value(option, value) {
                    return refuse(why);
                }
            }
            // The store merges these into the session's map at the commit.
            option_changes = Some(changes.clone());
        }
        let mut parts = String::new();
        for (label, value) in [
            ("model", &args.model),
            ("effort", &args.effort),
            ("mode", &args.permission_mode),
        ] {
            if let Some(v) = value {
                parts.push_str(&format!(" {label}={}", field(v)));
            }
        }
        if let Some(changes) = args.options.as_ref().filter(|o| !o.is_empty()) {
            let pairs: Vec<String> = changes
                .iter()
                .map(|(k, v)| format!("{}={}", field(k), field(v)))
                .collect();
            parts.push_str(&format!(" options={}", field(&pairs.join(","))));
        }
        Ok(Plan::Admit {
            effects: vec![Effect::SetChatSessionRuntime {
                id: id.to_string(),
                model: args.model.clone(),
                effort: args.effort.clone(),
                permission_mode: args.permission_mode.clone(),
                option_changes,
                updated_at: now(),
            }],
            subject: id.to_string(),
            line: format!(
                "chat_session_set {} admitted{parts} command={}",
                field(id),
                args.command_id
            ),
        })
    }

    /// The live part of an admitted set: a changed model, effort or mode reaches a live process, and
    /// so does a changed option that the port declares live. Any other option waits for the next
    /// open. A live change that fails stops the process: the next send reopens it with the new
    /// flags.
    fn apply_set(&self, args: &wire::SessionSetArgs) -> Option<String> {
        let live = self.live_chat(&args.session_id)?;
        let declared = live.port.options();
        let _io = lock(&live.io);
        if live.is_stopping() {
            return None;
        }
        let mut live_changes: Vec<(String, String)> = Vec::new();
        for (id, value) in [
            ("model", &args.model),
            ("effort", &args.effort),
            ("permission_mode", &args.permission_mode),
        ] {
            if let Some(v) = value {
                live_changes.push((id.to_string(), v.clone()));
            }
        }
        for (id, value) in args.options.iter().flatten() {
            if declared.iter().any(|o| &o.id == id && o.live) {
                live_changes.push((id.clone(), value.clone()));
            }
        }
        for (id, value) in live_changes {
            match live.port.set_option(&live.chat, &id, &value) {
                Ok(()) => {
                    lock(&live.inner).applied.insert(id, value);
                }
                Err(e) => {
                    let why = e.to_string();
                    self.stop_chat(&live, "option");
                    self.close_pending(&live, "the chat process stopped");
                    self.notice(
                        &args.session_id,
                        "option_restart",
                        &format!(
                            "the change of {id} did not reach the chat process ({why}); the next message restarts it with the new setting"
                        ),
                    );
                    self.sync_state(&live);
                    return Some(why);
                }
            }
        }
        None
    }

    // ------------------------------------------------------------------ end

    pub(super) fn verb_session_end(&self, args: &Value) -> Response {
        let args: wire::SessionEndArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        if let Err(why) = check_session_id(&args.session_id) {
            return Response::err(why);
        }
        let known = match self.command_known(&args.command_id) {
            Ok(k) => k,
            Err(why) => return Response::err(why),
        };
        let mut note = None;
        // The process stops first. A replay stops nothing.
        if !known && let Some(live) = self.live_chat(&args.session_id) {
            let _io = lock(&live.io);
            self.stop_chat(&live, "end");
            self.close_pending(&live, "the session ended");
        }
        let ended = match self.end_chat_session(&ChatSessionEndArgs {
            session_id: args.session_id.clone(),
            command_id: args.command_id,
        }) {
            Ok(e) => e,
            Err(why) => return Response::err(why),
        };
        if !ended.replayed {
            lock(&self.chats.lost).remove(&args.session_id);
            self.emit_session(&args.session_id);
        }
        if ended.replayed && self.live_chat(&args.session_id).is_some() {
            note = Some("the session is ended, but a process is live".to_string());
        }
        ok_json(&super::sessions::ChatSessionEnded { note, ..ended })
    }

    // ------------------------------------------------------------------ redact

    pub(super) fn verb_session_redact(&self, args: &Value) -> Response {
        let args: wire::SessionRedactArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        let done = match self.redact_chat_item(&ChatItemRedactArgs {
            session_id: args.session_id,
            seq: args.seq,
            command_id: args.command_id,
        }) {
            Ok(d) => d,
            Err(why) => return Response::err(why),
        };
        // A reader that follows the session sees the marks. A device that read the item before
        // keeps its copy (ADR-0015's known limit).
        if !done.replayed {
            let _one = lock(&self.chats.emit_lock);
            self.emit(wire::ChatFrame::Item {
                item: super::chat_view::item_record(&done.item),
            });
        }
        ok_json(&done)
    }

    // ------------------------------------------------------------------ the reads

    pub(super) fn verb_session_list(&self, args: &Value) -> Response {
        let args: wire::SessionListArgs = match parse(args) {
            Ok(a) => a,
            Err(r) => return r,
        };
        let all = args.all.unwrap_or(false);
        let projects = match &args.project_id {
            Some(id) => match self.store.project(id) {
                Ok(Some(p)) => vec![p],
                Ok(None) => return Response::err(format!("no project {id}")),
                Err(e) => return Response::err(e.to_string()),
            },
            None => match self.store.projects() {
                Ok(p) => p,
                Err(e) => return Response::err(e.to_string()),
            },
        };
        let mut rows = Vec::new();
        for project in &projects {
            match self.store.chat_sessions(&project.id) {
                Ok(r) => rows.extend(r),
                Err(e) => return Response::err(e.to_string()),
            }
        }
        rows.retain(|r| all || r.ended_at.is_none());
        rows.sort_by_key(|r| session_number(&r.id));
        let mut sessions = Vec::new();
        for row in &rows {
            match self.session_record(row, true) {
                Ok(r) => sessions.push(r),
                Err(why) => return Response::err(why),
            }
        }
        let agents = match self.agent_records() {
            Ok(a) => a,
            Err(why) => return Response::err(why),
        };
        ok_json(&wire::SessionList {
            schema_version: SCHEMA_VERSION,
            server: self.server_ref(),
            runtimes: self.runtime_records(),
            agents,
            sessions,
        })
    }

    pub(super) fn verb_session_items(&self, args: &Value) -> Response {
        let Some(id) = args.get("session_id").and_then(Value::as_str) else {
            return Response::err("the arguments are not readable: session_id is missing");
        };
        let after = args.get("after").and_then(Value::as_u64).unwrap_or(0);
        if let Err(why) = check_session_id(id) {
            return Response::err(why);
        }
        match self.session_row(id) {
            Ok(Some(_)) => {}
            Ok(None) => return Response::err(format!("no chat session {id}")),
            Err(e) => return Response::err(e),
        }
        match self.store.chat_items_after(id, after, ITEMS_MAX) {
            Ok(rows) => ok_json(&wire::SessionItems {
                schema_version: SCHEMA_VERSION,
                session_id: id.to_string(),
                items: rows.iter().map(super::chat_view::item_record).collect(),
            }),
            Err(e) => Response::err(e.to_string()),
        }
    }

    /// `halt` over the chats: stop every live process first. The answer names the sessions.
    pub(super) fn halt_chat_json(&self) -> Vec<String> {
        self.halt_chats()
    }
}
