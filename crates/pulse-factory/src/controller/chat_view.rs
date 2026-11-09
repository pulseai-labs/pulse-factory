//! The records of the chat verbs and of the stream: a stored session or item as the wire types of
//! `pf_api::types` show it (`r1.s3.w3`). The CLI's `--json` and the API print the same shapes.

use std::collections::BTreeMap;

use pf_api::types::{self as wire};
use pf_engine::port::chat::{ChatOption, MidTurn};
use pf_engine::port::store::{
    AgentRow, ChatItemKind, ChatItemRow, ChatSessionRow, ChatSessionState, NewChatItem, Store,
};
use serde_json::Value;

use super::chats::{Hold, effective_options, payload_of, utc};
use super::commands::now;
use super::worktrees::WorktreeLive;
use super::{Controller, lock};

/// The kind a body is stored under.
pub(super) fn kind_of(body: &wire::ChatItemBody) -> ChatItemKind {
    use wire::ChatItemBody as B;
    match body {
        B::OperatorMessage { .. } => ChatItemKind::OperatorMessage,
        B::AssistantText { .. } => ChatItemKind::AssistantText,
        B::ToolCall { .. } => ChatItemKind::ToolCall,
        B::ApprovalRequest { .. } => ChatItemKind::ApprovalRequest,
        B::ApprovalAnswer { .. } => ChatItemKind::ApprovalAnswer,
        B::QuestionRequest { .. } => ChatItemKind::QuestionRequest,
        B::QuestionAnswer { .. } => ChatItemKind::QuestionAnswer,
        B::BackgroundTask { .. } => ChatItemKind::BackgroundTask,
        B::TurnResult { .. } => ChatItemKind::TurnResult,
        B::Notice { .. } | B::Redacted { .. } | B::Unknown { .. } => ChatItemKind::Notice,
    }
}

/// A new item for a body. The caller adds the tool texts, the `answers_seq` and the usage.
pub(super) fn new_item(session_id: &str, body: &wire::ChatItemBody) -> NewChatItem {
    NewChatItem {
        session_id: session_id.to_string(),
        kind: kind_of(body),
        payload: payload_of(body),
        tool_input: None,
        tool_output: None,
        answers_seq: None,
        usage: None,
        created_at: now(),
    }
}

/// The body of an answer item that closes a request without an operator answer.
pub(super) fn closing_body(
    answer: ChatItemKind,
    outcome: wire::AnswerOutcome,
    reason: &str,
) -> wire::ChatItemBody {
    match answer {
        ChatItemKind::QuestionAnswer => wire::ChatItemBody::QuestionAnswer {
            outcome,
            reason: Some(reason.to_string()),
            answers: None,
        },
        _ => wire::ChatItemBody::ApprovalAnswer {
            outcome,
            reason: Some(reason.to_string()),
            decision: None,
            message: None,
        },
    }
}

fn tool_text_record(
    text: &Option<String>,
    bytes: Option<u64>,
    truncated: bool,
) -> Option<wire::ToolTextRecord> {
    text.as_ref().map(|t| wire::ToolTextRecord {
        text: t.clone(),
        bytes: bytes.unwrap_or(t.len() as u64),
        truncated,
    })
}

/// The body of a stored item. `None` for a kind this binary does not know, or a payload that does
/// not parse as its kind.
fn body_of(row: &ChatItemRow) -> Option<wire::ChatItemBody> {
    ChatItemKind::parse(&row.kind)?;
    let mut value: Value = serde_json::from_str(&row.payload).ok()?;
    value
        .as_object_mut()?
        .insert("kind".to_string(), Value::String(row.kind.clone()));
    let mut body: wire::ChatItemBody = serde_json::from_value(value).ok()?;
    match &mut body {
        wire::ChatItemBody::ToolCall { input, output, .. } => {
            *input = tool_text_record(
                &row.tool_input,
                row.tool_input_bytes,
                row.tool_input_truncated,
            );
            *output = tool_text_record(
                &row.tool_output,
                row.tool_output_bytes,
                row.tool_output_truncated,
            );
        }
        wire::ChatItemBody::ApprovalRequest { input, .. } => {
            *input = tool_text_record(
                &row.tool_input,
                row.tool_input_bytes,
                row.tool_input_truncated,
            );
        }
        _ => {}
    }
    Some(body)
}

/// One stored item as the wire shows it. A redacted row shows the marks only, and a kind this
/// binary does not know shows as `unknown`.
pub fn item_record(row: &ChatItemRow) -> wire::ChatItemRecord {
    let body = if row.redacted_at.is_some() {
        wire::ChatItemBody::Redacted {
            item_kind: row.kind.clone(),
        }
    } else {
        body_of(row).unwrap_or_else(|| wire::ChatItemBody::Unknown {
            item_kind: row.kind.clone(),
        })
    };
    wire::ChatItemRecord {
        session_id: row.session_id.clone(),
        seq: row.seq,
        answers_seq: row.answers_seq,
        created_at: row.created_at.clone(),
        redacted_at: row.redacted_at.clone(),
        body,
    }
}

fn option_records(options: Vec<ChatOption>) -> Vec<wire::ChatOptionRecord> {
    options
        .into_iter()
        .map(|o| wire::ChatOptionRecord {
            id: o.id,
            label: o.label,
            values: o.values,
            default: o.default,
            free: o.free,
            live: o.live,
        })
        .collect()
}

fn wire_state(state: ChatSessionState) -> wire::ChatState {
    match state {
        ChatSessionState::Working => wire::ChatState::Working,
        ChatSessionState::NeedsYou => wire::ChatState::NeedsYou,
        ChatSessionState::Idle => wire::ChatState::Idle,
    }
}

/// An agent as every view shows it: no environment value.
pub(super) fn agent_record(a: AgentRow) -> wire::AgentRecord {
    wire::AgentRecord {
        id: a.id,
        name: a.name,
        title: a.title,
        built_in: a.built_in,
        colour: a.colour,
        head: a.head,
        runtime: a.runtime,
        model: a.model,
        effort: a.effort,
        permission_mode: a.permission_mode,
        skills: a.skills,
        runtime_options: a.runtime_options,
    }
}

impl Controller {
    /// One session as the wire shows it. With `refresh`, a live session's branch is read from git
    /// now. Without it the last read is used, so a frame costs no git call.
    pub fn session_record(
        &self,
        row: &ChatSessionRow,
        refresh: bool,
    ) -> Result<wire::ChatSessionRecord, String> {
        let project = self
            .store
            .project(&row.project_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("project {} is not in the store", row.project_id))?;
        let worktree = self
            .store
            .worktree(&row.worktree_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("worktree {} is not in the store", row.worktree_id))?;
        let agent = self.agent(&row.agent_id)?;
        let (branch, detached) = if refresh && row.ended_at.is_none() {
            match self.check_worktree(&project, &worktree.path, Some(&worktree)) {
                Ok(WorktreeLive::Live { branch, detached }) => {
                    lock(&self.chats.branches).insert(row.id.clone(), (branch.clone(), detached));
                    (branch, detached)
                }
                Ok(WorktreeLive::Gone(_)) => (None, false),
                Err(_) => lock(&self.chats.branches)
                    .get(&row.id)
                    .cloned()
                    .unwrap_or((None, false)),
            }
        } else {
            lock(&self.chats.branches)
                .get(&row.id)
                .cloned()
                .unwrap_or((None, false))
        };
        let runtime_options = match &agent {
            Some(a) => effective_options(row, a),
            None => row.runtime_options.clone(),
        };
        let live = self.live_chat(&row.id);
        let process = live.as_ref().map(|l| {
            let holds = l.holds();
            let inner = lock(&l.inner);
            let turn_since = holds.iter().find_map(|h| match h {
                Hold::Turn(t) => Some(utc(*t)),
                _ => None,
            });
            let waiting_since = holds.iter().find_map(|h| match h {
                Hold::Answer(t) => Some(utc(*t)),
                _ => None,
            });
            let tasks = inner
                .tasks
                .values()
                .filter(|(t, _)| !t.ambient)
                .map(|(t, since)| wire::ChatTaskRecord {
                    id: t.id.clone(),
                    kind: t.kind.clone(),
                    description: t.description.clone(),
                    since: utc(*since),
                })
                .collect();
            wire::ChatProcessRecord {
                since: utc(l.opened_at),
                runtime_version: l.version.clone(),
                qualified: l.qualified,
                turn_since,
                waiting_since,
                tasks,
                releases_at: holds
                    .is_empty()
                    .then(|| utc(inner.idle_since + self.cfg.chat_idle)),
            }
        });
        let pending_options: BTreeMap<String, String> = match &live {
            Some(l) => {
                let inner = lock(&l.inner);
                runtime_options
                    .iter()
                    .filter(|(k, v)| inner.applied.get(*k) != Some(*v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            }
            None => BTreeMap::new(),
        };
        let context = live.as_ref().and_then(|l| {
            lock(&l.inner).context.map(|c| wire::ChatContext {
                used_tokens: c.used_tokens,
                max_tokens: c.max_tokens,
            })
        });
        let meta = lock(&self.chats.meta)
            .get(&row.id)
            .cloned()
            .unwrap_or_default();
        Ok(wire::ChatSessionRecord {
            id: row.id.clone(),
            project_id: row.project_id.clone(),
            project_name: project.name,
            worktree_id: row.worktree_id.clone(),
            worktree_dir: worktree.path,
            branch,
            detached,
            agent_id: row.agent_id.clone(),
            runtime: row.runtime.clone(),
            runtime_session_ref: row.runtime_session_ref.clone(),
            state: wire_state(row.state),
            context_lost: lock(&self.chats.lost).get(&row.id).cloned(),
            process,
            model: row.model.clone(),
            effort: row.effort.clone(),
            permission_mode: row.permission_mode.clone(),
            runtime_options,
            pending_options,
            usage: wire::ChatUsage {
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cache_read_tokens: row.cache_read_tokens,
                cache_write_tokens: row.cache_write_tokens,
                cost_micros: row.cost_micros,
            },
            last_turn_at: meta.last_turn_at,
            cache_ttl_seconds: meta.cache_ttl,
            context,
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
            ended_at: row.ended_at.clone(),
        })
    }

    /// The chat runtimes of this server, with their declared options and whether each can open a
    /// chat now. A readiness check reads the size of the managed copy and never runs it.
    pub(super) fn runtime_records(&self) -> Vec<wire::ChatRuntimeRecord> {
        self.cfg
            .chat_runtimes
            .iter()
            .map(|(id, port)| {
                let error = self.runtime_unready(id);
                wire::ChatRuntimeRecord {
                    id: id.clone(),
                    options: option_records(port.options()),
                    mid_turn: match port.mid_turn() {
                        MidTurn::Steer => wire::MidTurnMode::Steer,
                        MidTurn::NextTurn => wire::MidTurnMode::NextTurn,
                    },
                    ready: error.is_none(),
                    error,
                }
            })
            .collect()
    }

    /// Why a runtime cannot open a chat now, or `None`. Only the managed Claude Code copy has a
    /// check: a missing file or a size that differs from the pin.
    fn runtime_unready(&self, runtime: &str) -> Option<String> {
        if !self.cfg.chat_managed || runtime != pf_adapter_claude::CLAUDE_CODE_RUNTIME {
            return None;
        }
        super::chat_install::copy_problem(&self.cfg.chat_share_dir, &self.cfg.chat_pin)
    }

    /// The agents, for `session_list`.
    pub(super) fn agent_records(&self) -> Result<Vec<wire::AgentRecord>, String> {
        Ok(self.agents()?.into_iter().map(agent_record).collect())
    }
}
