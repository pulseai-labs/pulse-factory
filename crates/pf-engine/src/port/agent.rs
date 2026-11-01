//! The agent adapter port: the nine duties of ADR-0011, one method each, plus a liveness check.
//!
//! Each coding agent gets one adapter. The engine knows no agent.

use serde::{Deserialize, Serialize};

use crate::admission::PushOutcome;

/// A handle the adapter gives for one running session. The engine stores it and hands it back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef(pub String);

/// What the adapter needs to start one session. Duties 7 and 8 edit it before the launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPlan {
    pub run_id: String,
    pub unit_id: String,
    pub stage: String,
    pub role: String,
    pub model: String,
    /// The agent runtime of the role. A router picks the adapter by this name.
    pub runtime: String,
    pub attempt: u32,
    /// The stage's launch settings, rendered (`docs/contracts/launch-settings.md`), and the
    /// settings the adapter adds, such as flags.
    pub settings: std::collections::BTreeMap<String, String>,
}

/// The role rule set a stage declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleSet {
    pub name: String,
    pub prompts_off: bool,
    pub deny: Vec<String>,
}

/// What a session is doing now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Working,
    Idle,
    Ended,
}

/// Duty 2: status and turn end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    pub state: SessionState,
    /// True once a turn has ended and no other turn follows.
    pub turn_ended: bool,
    /// True once the session has started its first turn. A launch that never reaches a first
    /// turn fails (see `docs/contracts/agent-adapter.md`).
    pub first_turn_started: bool,
}

/// A plugin the adapter found, at a version that meets the minimum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPlugin {
    pub name: String,
    pub version: String,
    /// The directory of the plugin.
    pub path: String,
}

/// A neutral event from a session. A controller drains them with [`AgentPort::drain_events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The session did something: a turn, a tool call or a message.
    Activity,
    /// The session showed no activity within its stall limit. `screen_tail` is the end of the
    /// session's screen, redacted, as data for a reason text. The controller decides, with
    /// `unit::stall_decision`, whether the stall is real.
    Stalled { screen_tail: String },
    /// A seat pushed. `id` is fresh for each event. `repo` is the plan's declared repo.
    Push {
        id: String,
        repo: String,
        branch: String,
        sha: String,
        outcome: PushOutcome,
    },
    /// A push left a target the adapter could not read, and the default branch of the remote
    /// moved since the launch. The controller parks the unit.
    DefaultBranchMoved {
        remote: String,
        branch: String,
        before: String,
        after: String,
    },
}

/// Duty 5: token usage by kind, and cost in millionths of a dollar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_micros: u64,
}

/// Duty 6: a tool dialog the session shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialog {
    pub kind: String,
    pub text: String,
}

/// Duty 6: the adapter's answer to a known dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogAnswer {
    Answered(String),
    /// The adapter does not know this dialog.
    Unknown,
}

/// Why an adapter call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentError {
    /// The adapter refused the call, for example an unqualified agent version.
    Refused(String),
    /// The call was allowed and failed.
    Failed(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Refused(why) => write!(f, "launch refused by the agent adapter: {why}"),
            AgentError::Failed(why) => write!(f, "agent adapter failed: {why}"),
        }
    }
}

impl std::error::Error for AgentError {}

pub trait AgentPort: Send + Sync {
    /// Duty 1: launch a session.
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError>;
    /// Duty 2: read its status and turn end.
    fn status(&self, session: &SessionRef) -> Result<SessionStatus, AgentError>;
    /// Duty 3: deliver a message into an idle session.
    fn deliver(&self, session: &SessionRef, message: &str) -> Result<(), AgentError>;
    /// Duty 4: interrupt it.
    fn interrupt(&self, session: &SessionRef) -> Result<(), AgentError>;
    /// Duty 5: read its token usage.
    fn usage(&self, session: &SessionRef) -> Result<Usage, AgentError>;
    /// Duty 6: answer one of its known tool dialogs.
    fn answer_dialog(
        &self,
        session: &SessionRef,
        dialog: &Dialog,
    ) -> Result<DialogAnswer, AgentError>;
    /// Duty 7: apply prompts-off and the role rule set to the plan.
    fn apply_rules(&self, plan: &mut LaunchPlan, rules: &RuleSet) -> Result<(), AgentError>;
    /// Duty 8: remove the ask tool from the plan.
    fn remove_ask_tool(&self, plan: &mut LaunchPlan) -> Result<(), AgentError>;
    /// Duty 9: hand off to a successor session.
    fn hand_off(
        &self,
        session: &SessionRef,
        successor: &LaunchPlan,
    ) -> Result<SessionRef, AgentError>;
    /// Is this session's process alive?
    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError>;
    /// Find a plugin by name, at a version of at least `min_version`. It makes no prompt of its
    /// own. A plugin it cannot find, or finds at a lower version, is `AgentError::Refused`.
    fn resolve_plugin(&self, name: &str, min_version: &str) -> Result<ResolvedPlugin, AgentError>;
    /// Take the neutral events that happened since the last drain, oldest first.
    fn drain_events(&self, session: &SessionRef) -> Result<Vec<SessionEvent>, AgentError>;
}

/// Routes each call to the adapter of the right runtime.
///
/// A router holds one adapter per runtime name. It sends a launch to the plan's `runtime` and
/// tags the [`SessionRef`] it returns as `<runtime>:<inner>`, so every later call for that session
/// reaches the same adapter, which sees its own reference without the tag. A runtime name holds
/// no `:`. A call for a runtime with no adapter is [`AgentError::Refused`], so the launch parks
/// with a reason that names the runtime.
#[derive(Default)]
pub struct AgentRouter {
    adapters: std::collections::BTreeMap<String, std::sync::Arc<dyn AgentPort>>,
}

impl AgentRouter {
    pub fn new() -> AgentRouter {
        AgentRouter::default()
    }

    /// Add the adapter of a runtime. A name that is empty, holds a `:` or is taken is refused.
    pub fn register(
        &mut self,
        runtime: &str,
        adapter: std::sync::Arc<dyn AgentPort>,
    ) -> Result<(), AgentError> {
        if runtime.is_empty() || runtime.contains(':') {
            return Err(AgentError::Refused(format!(
                "runtime name \"{runtime}\" must have text and no \":\""
            )));
        }
        if self.adapters.contains_key(runtime) {
            return Err(AgentError::Refused(format!(
                "runtime \"{runtime}\" already has an adapter"
            )));
        }
        self.adapters.insert(runtime.to_string(), adapter);
        Ok(())
    }

    fn adapter(&self, runtime: &str) -> Result<&dyn AgentPort, AgentError> {
        self.adapters
            .get(runtime)
            .map(|a| a.as_ref())
            .ok_or_else(|| AgentError::Refused(format!("no adapter for runtime \"{runtime}\"")))
    }

    /// The adapter and the inner reference of a tagged session.
    fn route<'a>(
        &'a self,
        session: &'a SessionRef,
    ) -> Result<(&'a str, &'a dyn AgentPort, SessionRef), AgentError> {
        let Some((runtime, inner)) = session.0.split_once(':') else {
            return Err(AgentError::Refused(format!(
                "session reference \"{}\" has no runtime tag",
                session.0
            )));
        };
        let adapter = self.adapter(runtime)?;
        Ok((runtime, adapter, SessionRef(inner.to_string())))
    }

    fn tag(runtime: &str, inner: SessionRef) -> SessionRef {
        SessionRef(format!("{runtime}:{}", inner.0))
    }
}

impl AgentPort for AgentRouter {
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        let inner = self.adapter(&plan.runtime)?.launch(plan)?;
        Ok(AgentRouter::tag(&plan.runtime, inner))
    }

    fn status(&self, session: &SessionRef) -> Result<SessionStatus, AgentError> {
        let (_, adapter, inner) = self.route(session)?;
        adapter.status(&inner)
    }

    fn deliver(&self, session: &SessionRef, message: &str) -> Result<(), AgentError> {
        let (_, adapter, inner) = self.route(session)?;
        adapter.deliver(&inner, message)
    }

    fn interrupt(&self, session: &SessionRef) -> Result<(), AgentError> {
        let (_, adapter, inner) = self.route(session)?;
        adapter.interrupt(&inner)
    }

    fn usage(&self, session: &SessionRef) -> Result<Usage, AgentError> {
        let (_, adapter, inner) = self.route(session)?;
        adapter.usage(&inner)
    }

    fn answer_dialog(
        &self,
        session: &SessionRef,
        dialog: &Dialog,
    ) -> Result<DialogAnswer, AgentError> {
        let (_, adapter, inner) = self.route(session)?;
        adapter.answer_dialog(&inner, dialog)
    }

    fn apply_rules(&self, plan: &mut LaunchPlan, rules: &RuleSet) -> Result<(), AgentError> {
        let runtime = plan.runtime.clone();
        self.adapter(&runtime)?.apply_rules(plan, rules)
    }

    fn remove_ask_tool(&self, plan: &mut LaunchPlan) -> Result<(), AgentError> {
        let runtime = plan.runtime.clone();
        self.adapter(&runtime)?.remove_ask_tool(plan)
    }

    fn hand_off(
        &self,
        session: &SessionRef,
        successor: &LaunchPlan,
    ) -> Result<SessionRef, AgentError> {
        let (runtime, adapter, inner) = self.route(session)?;
        if successor.runtime != runtime {
            return Err(AgentError::Refused(format!(
                "a hand-off cannot change the runtime: the session runs on \"{runtime}\" and the \
                 successor asks for \"{}\"",
                successor.runtime
            )));
        }
        let next = adapter.hand_off(&inner, successor)?;
        Ok(AgentRouter::tag(runtime, next))
    }

    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError> {
        let (_, adapter, inner) = self.route(session)?;
        adapter.is_alive(&inner)
    }

    /// A plugin name has no runtime, so the adapters are tried in name order. The first one that
    /// resolves it wins. If none does, the call is refused and the reason holds each answer.
    fn resolve_plugin(&self, name: &str, min_version: &str) -> Result<ResolvedPlugin, AgentError> {
        let mut answers = Vec::new();
        for (runtime, adapter) in &self.adapters {
            match adapter.resolve_plugin(name, min_version) {
                Ok(found) => return Ok(found),
                Err(e) => answers.push(format!("{runtime}: {e}")),
            }
        }
        if answers.is_empty() {
            return Err(AgentError::Refused(format!(
                "no adapter can resolve plugin \"{name}\": the router has no adapter"
            )));
        }
        Err(AgentError::Refused(format!(
            "no adapter can resolve plugin \"{name}\" at version {min_version}: {}",
            answers.join("; ")
        )))
    }

    fn drain_events(&self, session: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        let (_, adapter, inner) = self.route(session)?;
        adapter.drain_events(&inner)
    }
}
