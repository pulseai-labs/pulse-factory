//! The chat sessions: the admission that starts one, the admission that ends one, and the admitted
//! redaction of one chat item (ADR-0001, ADR-0003, ADR-0015). They take the path of `commands.rs`: a
//! command ID, the admission scope `@controller`, one ledger line for each new command, and a refusal
//! that is an admission of its own. They launch nothing and change no run, so `halt` does not apply
//! to them. The verbs that reach them are `r1.s3.w3`'s.
//!
//! The display log itself (`append_chat_item` and the writes beside it) is not a command: those are
//! the store's writes outside admission, and they reach the store directly.

use pf_engine::port::store::{
    CONTROLLER_SCOPE, ChatItemKind, ChatItemRow, ChatSessionRow, Effect, NEW_ID, NewChatItem,
    NewChatSession, ORCHESTRATOR_AGENT, ProjectKind, SessionWorktree, Store, WORKTREE_ID,
    WorktreeCreatedBy, WorktreeRow,
};
use serde::{Deserialize, Serialize};

use super::Controller;
use super::commands::{Plan, check_command_id, field, now};
use super::worktrees::WorktreeLive;

/// A session ID names a stored row, so it is short. A longer text is no ID.
const SESSION_ID_MAX_CHARS: usize = 64;

/// The arguments of a chat session start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSessionStartArgs {
    pub project_id: String,
    pub agent_id: String,
    pub command_id: String,
}

/// The arguments of a chat session end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSessionEndArgs {
    pub session_id: String,
    pub command_id: String,
}

/// The answer of a chat session start. `replayed` is true when the command ID was admitted before
/// with the same content: the session and the worktree are the first record's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSessionStarted {
    pub replayed: bool,
    pub session: ChatSessionRow,
    pub worktree: WorktreeRow,
}

/// The answer of a chat session end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSessionEnded {
    pub replayed: bool,
    pub session: ChatSessionRow,
}

/// The arguments of the redaction of one chat item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatItemRedactArgs {
    pub session_id: String,
    pub seq: u64,
    pub command_id: String,
}

/// The answer of a redaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatItemRedacted {
    pub replayed: bool,
    pub item: ChatItemRow,
}

/// The content of a `chat_session_start` admission. The store compares these bytes, so the field
/// order is fixed. The key refresh and the worktree are not in it: a replay still matches.
#[derive(Serialize)]
struct StartContent<'a> {
    agent_id: &'a str,
    project_id: &'a str,
}

/// The content of a `chat_session_end` admission.
#[derive(Serialize)]
struct EndContent<'a> {
    session_id: &'a str,
}

/// The content of a `chat_item_redact` admission.
#[derive(Serialize)]
struct RedactContent<'a> {
    seq: u64,
    session_id: &'a str,
}

fn check_session_id(id: &str) -> Result<(), String> {
    let n = id.chars().count();
    if n == 0 || n > SESSION_ID_MAX_CHARS {
        return Err(format!(
            "a chat session ID is 1 to {SESSION_ID_MAX_CHARS} characters"
        ));
    }
    Ok(())
}

impl Controller {
    /// The project's live Orchestrator session, if any.
    pub fn live_orchestrator(&self, project_id: &str) -> Result<Option<ChatSessionRow>, String> {
        Ok(self
            .store
            .chat_sessions(project_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|s| s.agent_id == ORCHESTRATOR_AGENT && s.ended_at.is_none()))
    }

    /// The admission that starts a chat session (Approach 6). `Err` is the one-line refusal or
    /// error, as `run_command` gives it. A bad command ID stores nothing.
    pub fn start_chat_session(
        &self,
        args: &ChatSessionStartArgs,
    ) -> Result<ChatSessionStarted, String> {
        check_command_id(&args.command_id)?;
        let content = serde_json::to_string(&StartContent {
            agent_id: &args.agent_id,
            project_id: &args.project_id,
        })
        .unwrap_or_default();
        // A replay answers the first record. It reads nothing and marks nothing: the store finds
        // the command ID first, whatever the plan is.
        let plan = if self
            .store
            .admission(CONTROLLER_SCOPE, &args.command_id)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            Plan::Refuse("a replay".to_string())
        } else {
            self.plan_start(args)?
        };
        let (admission, replayed) =
            self.run_command("chat_session_start", &args.command_id, content, plan)?;
        let id = admission
            .unit_id
            .ok_or_else(|| "the admission names no chat session".to_string())?;
        let session = self
            .store
            .chat_session(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("chat session {id} is not in the store"))?;
        let worktree = self
            .store
            .worktree(&session.worktree_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("worktree {} is not in the store", session.worktree_id))?;
        Ok(ChatSessionStarted {
            replayed,
            session,
            worktree,
        })
    }

    /// Judge a start: the refusals the controller knows, then the effects.
    fn plan_start(&self, args: &ChatSessionStartArgs) -> Result<Plan, String> {
        let refuse = |why: String| Ok(Plan::Refuse(why));
        let id = &args.project_id;
        let project = match self.store.project(id).map_err(|e| e.to_string())? {
            None => return refuse(format!("no project {id}")),
            Some(p) if p.removed_at.is_some() => return refuse(format!("project {id} is removed")),
            Some(p) => p,
        };
        let Some(kind) = project.kind else {
            let cause = self.kind_cause(&project.root);
            return refuse(format!(
                "project {id} has no kind ({cause}); remove it and add it again"
            ));
        };
        match self.agent(&args.agent_id)? {
            None => return refuse(format!("no agent {}", args.agent_id)),
            Some(a) if a.id != ORCHESTRATOR_AGENT => {
                return refuse(
                    "only the Orchestrator has chat sessions in this release".to_string(),
                );
            }
            Some(_) => {}
        }

        // The worktree is the main worktree: the project root. Its row is recorded at the first
        // start, never at `project add`.
        let root = project.root.clone();
        let row = self
            .store
            .live_worktree_at(&root)
            .map_err(|e| e.to_string())?;
        if let Some(r) = &row
            && r.project_id != project.id
        {
            return refuse(format!(
                "the path {root} is held by worktree {} of project {}",
                r.id, r.project_id
            ));
        }
        let live = match self.check_worktree(&project, &root, row.as_ref()) {
            Ok(live) => live,
            // A git failure is no gone mark. It is a refusal of this start, and it stores no mark.
            Err(why) => return refuse(why),
        };
        if let WorktreeLive::Gone(why) = live {
            return refuse(format!("the worktree {root} is gone: {why}"));
        }
        let worktree = match &row {
            Some(r) => SessionWorktree::Existing { id: r.id.clone() },
            None => SessionWorktree::Record {
                path: root.clone(),
                created_by: WorktreeCreatedBy::Root,
            },
        };

        let mut effects = Vec::new();
        // The stored remote key is refreshed here, in the same commit, when the live read differs.
        // A failed read leaves it as it is. The key is not in the content, so a replay matches.
        let mut key_note = String::new();
        if kind == ProjectKind::Git
            && let Ok(live_key) = self.read_remote_key(&root)
            && live_key != project.remote_key
        {
            key_note = format!(
                " remote_key={}",
                live_key
                    .as_deref()
                    .map_or_else(|| "none".to_string(), field)
            );
            effects.push(Effect::SetProjectRemoteKey {
                id: project.id.clone(),
                remote_key: live_key,
            });
        }
        effects.push(Effect::StartChatSession(NewChatSession {
            project_id: project.id.clone(),
            agent_id: args.agent_id.clone(),
            worktree,
            created_at: now(),
        }));
        Ok(Plan::Admit {
            effects,
            subject: NEW_ID.to_string(),
            line: format!(
                "chat_session_start {NEW_ID} admitted project={} worktree={WORKTREE_ID} path={} agent={}{key_note} command={}",
                field(&project.id),
                field(&root),
                field(&args.agent_id),
                args.command_id
            ),
        })
    }

    /// The admission that ends a chat session. w3 stops the process first, then calls this.
    pub fn end_chat_session(&self, args: &ChatSessionEndArgs) -> Result<ChatSessionEnded, String> {
        check_command_id(&args.command_id)?;
        check_session_id(&args.session_id)?;
        let content = serde_json::to_string(&EndContent {
            session_id: &args.session_id,
        })
        .unwrap_or_default();
        // The store knows whether the ID names a live session, and its answer decides.
        let plan = Plan::Admit {
            effects: vec![Effect::EndChatSession {
                id: args.session_id.clone(),
                ended_at: now(),
            }],
            subject: args.session_id.clone(),
            line: format!(
                "chat_session_end {} admitted command={}",
                field(&args.session_id),
                args.command_id
            ),
        };
        let (_, replayed) =
            self.run_command("chat_session_end", &args.command_id, content, plan)?;
        let session = self
            .store
            .chat_session(&args.session_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("chat session {} is not in the store", args.session_id))?;
        Ok(ChatSessionEnded { replayed, session })
    }

    /// The admitted redaction of one chat item (C12): a command with an ID and one ledger line that
    /// never holds the item's text. It covers the store's display log only. The CLI's own transcript
    /// is the operator's to clean, and a paired device keeps what it already received.
    pub fn redact_chat_item(&self, args: &ChatItemRedactArgs) -> Result<ChatItemRedacted, String> {
        check_command_id(&args.command_id)?;
        check_session_id(&args.session_id)?;
        let content = serde_json::to_string(&RedactContent {
            seq: args.seq,
            session_id: &args.session_id,
        })
        .unwrap_or_default();
        let plan = Plan::Admit {
            effects: vec![Effect::RedactChatItem {
                session_id: args.session_id.clone(),
                seq: args.seq,
                redacted_at: now(),
            }],
            subject: args.session_id.clone(),
            line: format!(
                "chat_item_redact {} seq={} admitted command={}",
                field(&args.session_id),
                args.seq,
                args.command_id
            ),
        };
        let (_, replayed) =
            self.run_command("chat_item_redact", &args.command_id, content, plan)?;
        let item = self
            .store
            .chat_item(&args.session_id, args.seq)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!(
                    "chat item {} of session {} is not in the store",
                    args.seq, args.session_id
                )
            })?;
        Ok(ChatItemRedacted { replayed, item })
    }

    /// At controller start (Q2 rule 4, C3). A restart has ended every chat process (ADR-0007: no
    /// restart survival), so a live chat session is a session that waits for `--resume`: its state
    /// goes to `idle`, and `ended_at`, the runtime ref, the model, the effort and the permission
    /// mode stay. Each pending request gets an answer item with the outcome `lost`, so the app shows
    /// no card that can never be answered. None of this is a command: the state and the closing
    /// answers are writes outside admission (ADR-0003).
    pub(super) fn settle_live_chat_sessions(&self) {
        let sessions = match self.store.live_chat_sessions() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("pulse-factory: the live chat sessions are not read: {e}");
                return;
            }
        };
        for s in sessions {
            if let Err(e) = self.store.set_chat_session_state(
                &s.id,
                pf_engine::port::store::ChatSessionState::Idle,
                &now(),
            ) {
                eprintln!(
                    "pulse-factory: chat session {} is not set to idle: {e}",
                    s.id
                );
            }
            let pending = match self.store.pending_requests(&s.id) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "pulse-factory: the requests of chat session {} are not read: {e}",
                        s.id
                    );
                    continue;
                }
            };
            for request in pending {
                let Some(answer) =
                    ChatItemKind::parse(&request.kind).and_then(ChatItemKind::answered_by)
                else {
                    continue;
                };
                let lost = NewChatItem {
                    session_id: s.id.clone(),
                    kind: answer,
                    payload: "{\"outcome\":\"lost\",\"reason\":\"the controller restarted\"}"
                        .to_string(),
                    tool_input: None,
                    tool_output: None,
                    answers_seq: Some(request.seq),
                    usage: None,
                    created_at: now(),
                };
                if let Err(e) = self.store.append_chat_item(lost) {
                    eprintln!(
                        "pulse-factory: request {} of chat session {} is not closed: {e}",
                        request.seq, s.id
                    );
                }
            }
        }
    }
}
