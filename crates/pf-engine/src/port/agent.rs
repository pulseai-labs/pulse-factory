//! The agent adapter port: the nine duties of ADR-0011, one method each, plus a liveness check.
//!
//! Each coding agent gets one adapter. The engine knows no agent.

use serde::{Deserialize, Serialize};

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
}
