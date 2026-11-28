//! The store port (ADR-0003).
//!
//! The port carries one atomic commit: the state transition, its command and receipt, and its
//! ledger outbox line, together or not at all. It shows no SQL.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::port::agent::Usage;
use crate::status::Status;

/// Whether a run still takes work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Active,
    Ended,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Active => "active",
            RunState::Ended => "ended",
        }
    }

    pub fn parse(text: &str) -> Option<RunState> {
        match text {
            "active" => Some(RunState::Active),
            "ended" => Some(RunState::Ended),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRow {
    pub id: String,
    pub workflow: String,
    pub ledger_file: String,
    pub state: RunState,
    /// The reason of the `halt` command, when the run is halted.
    pub halted_reason: Option<String>,
    /// The project of the chat that proposed the run, when a device armed it from a start card.
    /// `None` for every run armed from the CLI and for every run that existed before migration
    /// 0008. `PutRun` never writes it: only [`Effect::SetRunProject`] does.
    pub project_id: Option<String>,
    /// The run's armed launch count: the most launches `arm` showed for the run's pinned package
    /// (ADR-0003's amendment of 2026-11-16). `None` for every run armed before migration 0014 and for
    /// a plan whose most launches are unknown. `PutRun` never writes it: only
    /// [`Effect::SetRunLaunchBound`] does, once. A grant act that would pass it is refused.
    pub launch_bound: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Unit,
    Stage,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Unit => "unit",
            NodeKind::Stage => "stage",
        }
    }

    pub fn parse(text: &str) -> Option<NodeKind> {
        match text {
            "unit" => Some(NodeKind::Unit),
            "stage" => Some(NodeKind::Stage),
            _ => None,
        }
    }
}

/// One node of a run's tree: the run's units, and under each unit one node per stage attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRow {
    pub run_id: String,
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: NodeKind,
    pub status: Status,
    pub reason: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub attempt: u32,
    /// Engine-owned text. A unit holds its record and progress as JSON. A stage holds its name.
    pub detail: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One model session, with its token usage by kind and its cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub run_id: String,
    pub id: String,
    pub node_id: String,
    pub unit_id: String,
    /// The adapter's handle. `None` until the launch has succeeded.
    pub process_ref: Option<String>,
    /// True once a signal or an end status closed the session's stage.
    pub ended: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_micros: u64,
    pub created_at: String,
    pub updated_at: String,
}

/// Whether the engine admitted a command, or refused it with a recorded reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Admitted,
    Refused,
}

impl AdmissionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AdmissionOutcome::Admitted => "admitted",
            AdmissionOutcome::Refused => "refused",
        }
    }

    pub fn parse(text: &str) -> Option<AdmissionOutcome> {
        match text {
            "admitted" => Some(AdmissionOutcome::Admitted),
            "refused" => Some(AdmissionOutcome::Refused),
            _ => None,
        }
    }
}

/// The receipt of one command. The command ID is unique within a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionRow {
    /// Admission order, assigned by the store.
    pub seq: i64,
    pub run_id: String,
    pub command_id: String,
    pub kind: String,
    /// The command as canonical JSON. The store compares these bytes.
    pub content: String,
    pub outcome: AdmissionOutcome,
    pub reason: Option<String>,
    /// For a launch: the unit, role, model and attempt. For a controller command on a project:
    /// the ID of the project it names (`unit_id` only), so a replay can find that project.
    pub unit_id: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub attempt: Option<u32>,
    pub admitted_at: String,
}

/// The receipt fields the engine decides. The store adds `seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAdmission {
    pub kind: String,
    pub outcome: AdmissionOutcome,
    pub reason: Option<String>,
    pub unit_id: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub attempt: Option<u32>,
    pub admitted_at: String,
}

/// What a request is about: the closed list of the halts of ADR-0006, and the merge ack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// Any park whose reason is not one of the others.
    #[default]
    Park,
    /// An implementer's pre-flight `gaps-surfaced` return.
    Gaps,
    /// A stop file.
    Stop,
    /// A red close gate (a correction packet).
    RedGate,
    /// A stalled seat that already had its one relaunch.
    Stall,
    /// A merge that did not land.
    MergeFailed,
    /// A seat lost across a controller restart.
    SeatLost,
    /// A merge waits on the claim's ack.
    MergeAck,
}

impl RequestKind {
    /// Every kind, in the order the contract lists them.
    pub const ALL: [RequestKind; 8] = [
        RequestKind::Park,
        RequestKind::Gaps,
        RequestKind::Stop,
        RequestKind::RedGate,
        RequestKind::Stall,
        RequestKind::MergeFailed,
        RequestKind::SeatLost,
        RequestKind::MergeAck,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RequestKind::Park => "park",
            RequestKind::Gaps => "gaps",
            RequestKind::Stop => "stop",
            RequestKind::RedGate => "red_gate",
            RequestKind::Stall => "stall",
            RequestKind::MergeFailed => "merge_failed",
            RequestKind::SeatLost => "seat_lost",
            RequestKind::MergeAck => "merge_ack",
        }
    }

    pub fn parse(text: &str) -> Option<RequestKind> {
        RequestKind::ALL.into_iter().find(|k| k.as_str() == text)
    }
}

/// Why a unit parked: the closed list of the causes of a park (`docs/contracts/requests.md`, "The
/// decision block"). Every park site names one in code. Nothing reads the reason text to find it.
/// The cause is the key of a request's options and of its recommendation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The seat wrote its stop file.
    StopFile,
    /// The seat wrote a file the stage lists under `park_on`.
    ParkFile,
    /// The seat's turn ended and no done signal holds.
    NoDoneSignal,
    /// The seat stalled after its one relaunch.
    Stalled,
    /// The agent port refused the launch.
    LaunchRefused,
    /// The agent port failed the launch.
    LaunchFailed,
    /// A stage outcome reached its cap.
    OutcomeCap,
    /// A command hook of a stage failed.
    HookFailed,
    /// The answer hook of a request failed.
    AnswerHookFailed,
    /// A pinned copy of the run changed after arm.
    PinsChanged,
    /// The push of a stage's base branch failed.
    BasePushFailed,
    /// The default branch moved during a push.
    BaseMoved,
    /// A path or a condition of a stage cannot render.
    RenderFailed,
    /// The decisions file is over its bound.
    DecisionsTooLarge,
    /// The unit is ready to merge and no PR is recorded for it.
    NoPr,
    /// The forge reports a merge conflict.
    MergeConflict,
    /// The checks of the PR are red.
    MergeChecksRed,
    /// The forge refused the merge.
    MergeRefused,
    /// A forge error, or an outcome no other cause fits.
    MergeError,
    /// The seat's process is gone.
    SeatLost,
    /// The seat's host did not answer.
    HostUnreachable,
    /// A merge waits on the claim's ack.
    MergeWaitsAck,
}

impl Cause {
    /// Every cause, in the order the contract lists them.
    pub const ALL: [Cause; 22] = [
        Cause::StopFile,
        Cause::ParkFile,
        Cause::NoDoneSignal,
        Cause::Stalled,
        Cause::LaunchRefused,
        Cause::LaunchFailed,
        Cause::OutcomeCap,
        Cause::HookFailed,
        Cause::AnswerHookFailed,
        Cause::PinsChanged,
        Cause::BasePushFailed,
        Cause::BaseMoved,
        Cause::RenderFailed,
        Cause::DecisionsTooLarge,
        Cause::NoPr,
        Cause::MergeConflict,
        Cause::MergeChecksRed,
        Cause::MergeRefused,
        Cause::MergeError,
        Cause::SeatLost,
        Cause::HostUnreachable,
        Cause::MergeWaitsAck,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Cause::StopFile => "stop_file",
            Cause::ParkFile => "park_file",
            Cause::NoDoneSignal => "no_done_signal",
            Cause::Stalled => "stalled",
            Cause::LaunchRefused => "launch_refused",
            Cause::LaunchFailed => "launch_failed",
            Cause::OutcomeCap => "outcome_cap",
            Cause::HookFailed => "hook_failed",
            Cause::AnswerHookFailed => "answer_hook_failed",
            Cause::PinsChanged => "pins_changed",
            Cause::BasePushFailed => "base_push_failed",
            Cause::BaseMoved => "base_moved",
            Cause::RenderFailed => "render_failed",
            Cause::DecisionsTooLarge => "decisions_too_large",
            Cause::NoPr => "no_pr",
            Cause::MergeConflict => "merge_conflict",
            Cause::MergeChecksRed => "merge_checks_red",
            Cause::MergeRefused => "merge_refused",
            Cause::MergeError => "merge_error",
            Cause::SeatLost => "seat_lost",
            Cause::HostUnreachable => "host_unreachable",
            Cause::MergeWaitsAck => "merge_waits_ack",
        }
    }

    pub fn parse(text: &str) -> Option<Cause> {
        Cause::ALL.into_iter().find(|c| c.as_str() == text)
    }
}

/// Where a request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Open,
    Answered,
    Withdrawn,
}

impl RequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RequestStatus::Open => "open",
            RequestStatus::Answered => "answered",
            RequestStatus::Withdrawn => "withdrawn",
        }
    }

    pub fn parse(text: &str) -> Option<RequestStatus> {
        match text {
            "open" => Some(RequestStatus::Open),
            "answered" => Some(RequestStatus::Answered),
            "withdrawn" => Some(RequestStatus::Withdrawn),
            _ => None,
        }
    }
}

/// One request: something an operator can see and answer (`docs/contracts/requests.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestRow {
    /// `R-<n>`, from one sequence for the whole store.
    pub id: String,
    /// From 1. A revision rises when the request's facts change while it stays open.
    pub revision: u32,
    pub run_id: String,
    /// `None` for a run-level request.
    pub unit_id: Option<String>,
    /// The stage the unit parked in, `merge` for a unit parked at its merge, or empty.
    pub stage: String,
    pub kind: RequestKind,
    /// One line.
    pub reason: String,
    /// JSON text.
    pub body: String,
    pub status: RequestStatus,
    pub opened_at: String,
    /// When the request left `open`: answered or withdrawn.
    pub answered_at: Option<String>,
    /// JSON text of the answer: `{"verb":"answer",...}`, `{"verb":"clear",...}` or
    /// `{"verb":"withdraw",...}`.
    pub answer: Option<String>,
    /// The claim generation that answered.
    pub answer_generation: Option<u64>,
}

/// A request before the store gives it an ID. The store sets the revision to 1 and the status to
/// `open`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRequest {
    pub run_id: String,
    pub unit_id: Option<String>,
    pub stage: String,
    pub kind: RequestKind,
    pub reason: String,
    pub body: String,
    pub opened_at: String,
}

/// One claim. The current claim is the highest generation with no `fenced_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimRow {
    /// From 1, strictly rising.
    pub generation: u64,
    /// SHA-256 of the token, as 64 hex digits. The token itself is never stored.
    pub token_hash: String,
    /// One line the client sends: user, host and session label.
    pub holder: String,
    pub claimed_at: String,
    /// Set when a newer claim replaces this one.
    pub fenced_at: Option<String>,
}

/// The admission scope of the controller's own commands (projects and devices). A run's
/// commands commit under the run's ID. The store refuses a run ID that holds `@`, so no run can
/// share this scope.
pub const CONTROLLER_SCOPE: &str = "@controller";

/// A stand-in for the ID that a commit's own effect assigns. The caller cannot know that ID
/// before the commit. The store replaces the first copy of this text in the commit's ledger line
/// with the ID, and the whole `unit_id` of its admission when that is this text, inside the
/// commit's transaction. A commit has one effect that assigns an ID. A chat session start assigns
/// `S-<n>` through `NEW_ID`, and it may also record a worktree: [`WORKTREE_ID`] stands for that
/// second ID (ADR-0003, ADR-0015).
pub const NEW_ID: &str = "{new-id}";

/// The stand-in for the ID of the worktree row that a chat session start used. The store replaces
/// the first copy of this text in the commit's ledger line with that ID: the new `W-<n>` of a
/// recorded worktree, or the ID of the existing worktree. A commit without a session start leaves
/// the text as it is.
pub const WORKTREE_ID: &str = "{worktree-id}";

/// The ID of the built-in Orchestrator agent.
pub const ORCHESTRATOR_AGENT: &str = "orchestrator";

/// The cap on a tool call's stored input and its stored output, in bytes (the display log only). A
/// text the store cuts is cut by [`crate::port::chat::cap`], so the store, the adapter and the stream
/// share one cut and one mark (`…[truncated: <bytes> bytes in all]`).
pub const TOOL_TEXT_MAX_BYTES: usize = 16384;

/// The payload of a redacted chat item (C12). The store's trigger holds the same text.
pub const REDACTED_PAYLOAD: &str = "{\"redacted\":true}";

/// The text of a redacted tool input or output (C12). The store's trigger holds the same text.
pub const REDACTED_TEXT: &str = "[redacted]";

/// The `outcome` an answer item's payload names: the operator answered, the process that asked is
/// gone (a controller restart), or the runtime itself cancelled the request.
pub const ANSWER_OUTCOMES: [&str; 3] = ["answered", "lost", "withdrawn"];

/// What a project is: a git repository's main worktree, or a plain folder (ADR-0015).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    Git,
    Folder,
}

impl ProjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Git => "git",
            ProjectKind::Folder => "folder",
        }
    }

    pub fn parse(text: &str) -> Option<ProjectKind> {
        match text {
            "git" => Some(ProjectKind::Git),
            "folder" => Some(ProjectKind::Folder),
            _ => None,
        }
    }
}

/// One project: a controller entity. The engine core never reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRow {
    /// `P-<n>`, from one sequence for the whole store. A removed row keeps its ID.
    pub id: String,
    pub name: String,
    /// An absolute, canonical path.
    pub root: String,
    pub created_at: String,
    /// Set when the project is removed. The row stays.
    pub removed_at: Option<String>,
    /// `None` only for a project that was added before the kind existed and that the controller
    /// could not detect yet. A kind, once set, never changes.
    pub kind: Option<ProjectKind>,
    /// `host/path` of the git remote, for a `git` project with one. Never a URL.
    pub remote_key: Option<String>,
}

/// A project before the store gives it an ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewProject {
    pub name: String,
    pub root: String,
    pub created_at: String,
    pub kind: ProjectKind,
    pub remote_key: Option<String>,
}

/// Who made a worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeCreatedBy {
    /// The project's root: its main worktree.
    #[serde(rename = "root")]
    Root,
    #[serde(rename = "pulse-factory")]
    PulseFactory,
    #[serde(rename = "external")]
    External,
}

impl WorktreeCreatedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            WorktreeCreatedBy::Root => "root",
            WorktreeCreatedBy::PulseFactory => "pulse-factory",
            WorktreeCreatedBy::External => "external",
        }
    }

    pub fn parse(text: &str) -> Option<WorktreeCreatedBy> {
        match text {
            "root" => Some(WorktreeCreatedBy::Root),
            "pulse-factory" => Some(WorktreeCreatedBy::PulseFactory),
            "external" => Some(WorktreeCreatedBy::External),
            _ => None,
        }
    }
}

/// A worktree the store records: one where a chat session started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRow {
    /// `W-<n>`, never reused.
    pub id: String,
    pub project_id: String,
    /// An absolute, canonical path.
    pub path: String,
    pub created_by: WorktreeCreatedBy,
    pub created_at: String,
    /// Set by the first read that saw the worktree gone, for display. The row stays.
    pub gone_at: Option<String>,
    /// Set with the project's remove.
    pub removed_at: Option<String>,
}

/// One agent: a persona with its runtime settings. A built-in agent is a seeded row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub title: String,
    pub built_in: bool,
    pub colour: String,
    pub head: String,
    /// `None` for a built-in agent: the binary holds its text.
    pub instructions: Option<String>,
    /// A chat runtime's ID. The store treats it as opaque text.
    pub runtime: String,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    pub skills: Vec<String>,
    /// The runtime's options, as opaque text pairs.
    pub runtime_options: BTreeMap<String, String>,
    /// Names and values that are not secrets. They reach the store and the paired devices.
    pub launch_env: BTreeMap<String, String>,
    pub created_at: String,
}

/// What a live chat session is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatSessionState {
    Working,
    NeedsYou,
    Idle,
}

impl ChatSessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatSessionState::Working => "working",
            ChatSessionState::NeedsYou => "needs_you",
            ChatSessionState::Idle => "idle",
        }
    }

    pub fn parse(text: &str) -> Option<ChatSessionState> {
        match text {
            "working" => Some(ChatSessionState::Working),
            "needs_you" => Some(ChatSessionState::NeedsYou),
            "idle" => Some(ChatSessionState::Idle),
            _ => None,
        }
    }
}

/// One chat session: an agent in a worktree. Live means `ended_at` is empty, with or without a
/// process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSessionRow {
    /// `S-<n>`, never reused.
    pub id: String,
    pub project_id: String,
    pub worktree_id: String,
    pub agent_id: String,
    pub runtime: String,
    /// The runtime's own session ID. `None` until it reports one.
    pub runtime_session_ref: Option<String>,
    pub state: ChatSessionState,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    /// The session's own runtime options, as opaque text pairs. They start as the agent's, change
    /// per chat and never change the agent. `{}` for a session that was live before migration 0006:
    /// the controller then launches it with its agent's options.
    pub runtime_options: BTreeMap<String, String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_micros: u64,
    pub created_at: String,
    pub updated_at: String,
    pub ended_at: Option<String>,
    /// Where the session's output stands in its host. `None` for a session with no host output.
    pub host_cursor: Option<ChatHostCursor>,
}

/// Where a chat session's output stands in the host it came from (migration 0011).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHostCursor {
    /// The host the output came from.
    pub unit: String,
    pub incarnation: u32,
    /// The cursor after the last output line the store consumed.
    pub committed: u64,
    /// The cursor the controller last acknowledged to the host.
    pub acked: u64,
    /// The adapter's opaque state at `acked`. `None` before the first quiet point.
    pub checkpoint: Option<String>,
    /// True once the store holds this host's exit: the exit's own write set it. The exit comes at
    /// the cursor of the last output line, so a cursor that equals `committed` does not say that
    /// the exit is stored; only this flag does. A cursor written before the flag reads `false`.
    #[serde(default)]
    pub exited: bool,
    /// When the turn that runs in this host started, as UTC text: the marks of the turn's output
    /// lines set it. `None` when no turn runs: a quiet point and the exit clear it. A restart
    /// reads it, so the turn keeps its start. A cursor written before the field reads `None`.
    #[serde(default)]
    pub turn_since: Option<String>,
}

/// A position that comes with a write: the host, and the cursor after the output that gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatHostMark {
    pub unit: String,
    pub incarnation: u32,
    /// The cursor after the output line.
    pub at: u64,
    /// The adapter's state at this quiet point. With it, the store also sets `acked`, and clears
    /// `turn_since`: no turn runs at a quiet point.
    pub checkpoint: Option<String>,
    /// The output is the host's exit: the store sets `exited` with it, and clears `turn_since`.
    pub exited: bool,
    /// The output line belongs to a turn that started at this time (UTC text): the store sets
    /// `turn_since` with it. The mark of a turn's first line names it, and each later mark of the
    /// turn names it again, so a start whose own write failed is stored with the turn's next
    /// item. `None` for a line outside a turn, which leaves `turn_since` as it is.
    pub turn_since: Option<String>,
}

/// The kind of one chat item: the closed list of the display log. The store keeps it as text with
/// no `CHECK`, so a later kind needs a change here and no table rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatItemKind {
    OperatorMessage,
    AssistantText,
    ToolCall,
    ApprovalRequest,
    ApprovalAnswer,
    QuestionRequest,
    QuestionAnswer,
    BackgroundTask,
    Notice,
    TurnResult,
    /// A proposal to start a run, posted by the controller. It is no request: it holds no process.
    StartCard,
    /// The end of a start card: armed, withdrawn or failed.
    StartResult,
}

impl ChatItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatItemKind::OperatorMessage => "operator_message",
            ChatItemKind::AssistantText => "assistant_text",
            ChatItemKind::ToolCall => "tool_call",
            ChatItemKind::ApprovalRequest => "approval_request",
            ChatItemKind::ApprovalAnswer => "approval_answer",
            ChatItemKind::QuestionRequest => "question_request",
            ChatItemKind::QuestionAnswer => "question_answer",
            ChatItemKind::BackgroundTask => "background_task",
            ChatItemKind::Notice => "notice",
            ChatItemKind::TurnResult => "turn_result",
            ChatItemKind::StartCard => "start_card",
            ChatItemKind::StartResult => "start_result",
        }
    }

    pub fn parse(text: &str) -> Option<ChatItemKind> {
        match text {
            "operator_message" => Some(ChatItemKind::OperatorMessage),
            "assistant_text" => Some(ChatItemKind::AssistantText),
            "tool_call" => Some(ChatItemKind::ToolCall),
            "approval_request" => Some(ChatItemKind::ApprovalRequest),
            "approval_answer" => Some(ChatItemKind::ApprovalAnswer),
            "question_request" => Some(ChatItemKind::QuestionRequest),
            "question_answer" => Some(ChatItemKind::QuestionAnswer),
            "background_task" => Some(ChatItemKind::BackgroundTask),
            "notice" => Some(ChatItemKind::Notice),
            "turn_result" => Some(ChatItemKind::TurnResult),
            "start_card" => Some(ChatItemKind::StartCard),
            "start_result" => Some(ChatItemKind::StartResult),
            _ => None,
        }
    }

    /// For an answer kind, the request kind it answers. `None` for every other kind.
    pub fn answers(self) -> Option<ChatItemKind> {
        match self {
            ChatItemKind::ApprovalAnswer => Some(ChatItemKind::ApprovalRequest),
            ChatItemKind::QuestionAnswer => Some(ChatItemKind::QuestionRequest),
            ChatItemKind::StartResult => Some(ChatItemKind::StartCard),
            _ => None,
        }
    }

    /// For a request kind, the answer kind that closes it. `None` for every other kind.
    pub fn answered_by(self) -> Option<ChatItemKind> {
        match self {
            ChatItemKind::ApprovalRequest => Some(ChatItemKind::ApprovalAnswer),
            ChatItemKind::QuestionRequest => Some(ChatItemKind::QuestionAnswer),
            ChatItemKind::StartCard => Some(ChatItemKind::StartResult),
            _ => None,
        }
    }

    /// True for a request that an answer item closes.
    pub fn is_request(self) -> bool {
        matches!(
            self,
            ChatItemKind::ApprovalRequest | ChatItemKind::QuestionRequest
        )
    }
}

/// One completed item of a chat's display log. The log is append-only, except for one admitted
/// redaction of an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatItemRow {
    pub session_id: String,
    /// From 1 within the session, with no gap.
    pub seq: u64,
    /// The text of the kind. A kind this binary does not know stays readable.
    pub kind: String,
    /// JSON.
    pub payload: String,
    pub tool_input: Option<String>,
    pub tool_input_bytes: Option<u64>,
    pub tool_input_truncated: bool,
    pub tool_output: Option<String>,
    pub tool_output_bytes: Option<u64>,
    pub tool_output_truncated: bool,
    /// An answer item: the `seq` of the request it answers.
    pub answers_seq: Option<u64>,
    pub redacted_at: Option<String>,
    pub created_at: String,
}

/// A tool call's input or output, as the adapter capped it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolText {
    pub text: String,
    /// The size before any cut, in bytes.
    pub bytes: u64,
    pub truncated: bool,
}

impl ToolText {
    /// Raw text that nothing has capped. The store cuts it when it is over
    /// [`TOOL_TEXT_MAX_BYTES`].
    pub fn uncapped(text: impl Into<String>) -> ToolText {
        let text = text.into();
        ToolText {
            bytes: text.len() as u64,
            text,
            truncated: false,
        }
    }
}

/// A chat item before the store gives it its `seq`. A tool call's input and output travel in their
/// own fields, never inside `payload`, so the store can cap them and a redaction can replace them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatItem {
    pub session_id: String,
    pub kind: ChatItemKind,
    /// JSON.
    pub payload: String,
    pub tool_input: Option<ToolText>,
    pub tool_output: Option<ToolText>,
    /// An answer item: the `seq` of the request it answers.
    pub answers_seq: Option<u64>,
    /// A `turn_result`'s usage: the store adds it to the session's totals.
    pub usage: Option<Usage>,
    /// An item written from host output: the store sets the session's `committed` to `at` in the
    /// same transaction. A mark of another unit or incarnation replaces the cursor, and a
    /// `committed` never moves back within one unit. A mark of a line in a turn sets
    /// `turn_since`, and a quiet point or the exit clears it.
    pub host: Option<ChatHostMark>,
    pub created_at: String,
}

/// Where a chat session starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionWorktree {
    /// A live worktree row of the project.
    Existing { id: String },
    /// A new row, made inside the commit. The path must hold no live row.
    Record {
        path: String,
        created_by: WorktreeCreatedBy,
    },
}

/// A chat session before the store gives it an ID. It starts `idle`, with no runtime ref, and takes
/// its runtime, model, effort and permission mode from the agent row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChatSession {
    pub project_id: String,
    pub agent_id: String,
    pub worktree: SessionWorktree,
    pub created_at: String,
}

/// One device: a client paired with a one-use code. It holds a revocable token, and the row never
/// holds the token or its hash. A controller entity: the engine core never reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRow {
    /// `D-<n>`, from one sequence for the whole store. A revoked row keeps its ID.
    pub id: String,
    pub name: String,
    pub paired_at: String,
    /// Telemetry, written by [`Store::touch_device`] outside admission.
    pub last_seen_at: Option<String>,
    pub last_peer: Option<String>,
    /// Set when the device is revoked. The row stays.
    pub revoked_at: Option<String>,
}

/// A device before the store gives it an ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewDevice {
    pub name: String,
    /// SHA-256 of the token, as 64 hex digits. The token itself is never stored.
    pub token_sha256: String,
    pub paired_at: String,
}

/// Which of the four scopes a setting row sits at (ADR-0016).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingScopeKind {
    /// Every project and every agent.
    Server,
    /// One project, every agent in it.
    Project,
    /// One agent, every project.
    Agent,
    /// One agent in one project.
    ProjectAgent,
}

impl SettingScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingScopeKind::Server => "server",
            SettingScopeKind::Project => "project",
            SettingScopeKind::Agent => "agent",
            SettingScopeKind::ProjectAgent => "project_agent",
        }
    }
}

/// The scope of one setting row. `None` means "every": no project, or no agent. The store keeps
/// `None` as `''` and the port maps it both ways.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettingScope {
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
}

impl SettingScope {
    /// The server default: every project and every agent.
    pub fn server() -> SettingScope {
        SettingScope {
            project_id: None,
            agent_id: None,
        }
    }

    pub fn new(project_id: Option<&str>, agent_id: Option<&str>) -> SettingScope {
        SettingScope {
            project_id: project_id.map(str::to_string),
            agent_id: agent_id.map(str::to_string),
        }
    }

    pub fn kind(&self) -> SettingScopeKind {
        match (&self.project_id, &self.agent_id) {
            (None, None) => SettingScopeKind::Server,
            (Some(_), None) => SettingScopeKind::Project,
            (None, Some(_)) => SettingScopeKind::Agent,
            (Some(_), Some(_)) => SettingScopeKind::ProjectAgent,
        }
    }

    /// The text form, for ledger lines and admission subjects: `server`, `project:P-1`,
    /// `agent:orchestrator`, `project:P-1/agent:orchestrator`.
    pub fn text(&self) -> String {
        match (&self.project_id, &self.agent_id) {
            (None, None) => "server".to_string(),
            (Some(p), None) => format!("project:{p}"),
            (None, Some(a)) => format!("agent:{a}"),
            (Some(p), Some(a)) => format!("project:{p}/agent:{a}"),
        }
    }

    /// The scopes a context reads, most specific first: (project, agent), (project, every),
    /// (every, agent), server. A context with no project skips the two project scopes; one with no
    /// agent skips the two agent scopes.
    pub fn chain(project: Option<&str>, agent: Option<&str>) -> Vec<SettingScope> {
        let mut scopes = Vec::with_capacity(4);
        if project.is_some() && agent.is_some() {
            scopes.push(SettingScope::new(project, agent));
        }
        if project.is_some() {
            scopes.push(SettingScope::new(project, None));
        }
        if agent.is_some() {
            scopes.push(SettingScope::new(None, agent));
        }
        scopes.push(SettingScope::server());
        scopes
    }
}

/// One value of one key at one scope. The engine core never reads it (ADR-0002): the key is opaque
/// text and the value is canonical JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingRow {
    pub scope: SettingScope,
    pub key: String,
    /// Canonical JSON.
    pub value: String,
    /// `cli`, or the ID of the device that set it.
    pub set_by: String,
    pub set_at: String,
}

/// The kinds of session that a launch record and a host can name. The store keeps the kind as
/// text with no `CHECK`: the port validates it, so a later kind needs no table rebuild (ADR-0003's
/// amendment of 2026-11-16, the precedent of ADR-0016's key list).
pub const SESSION_KINDS: [&str; 3] = ["seat", "chat", "terminal"];

/// The launchers that start a host: a transient service unit, or a detached process. The store
/// keeps the launcher as text with no `CHECK`.
pub const HOST_LAUNCHERS: [&str; 2] = ["systemd", "detached"];

/// A launch record before the store writes it: what a session's next launch must use. A record is
/// written once, before its host starts, and never changes. A live change that must hold after a
/// resume writes the next revision, and the newest revision wins.
///
/// The body, `record`, is JSON with the fields `argv`, `cwd`, `env`, `secret_names`, `transport`,
/// `runtime`, `settings`, `plugins`, `skills`, `rules_sha256`, `run_binding` and `resume`. `resume`
/// says how to resume. It never holds the runtime's own session ID: the runtime mints that ID after
/// the launch, so it stays on the session row. No secret value is in the body: a name in
/// `secret_names` holds `sha256:<64 hex>` or `ref:<name>` in `env`. No attach key is in the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewLaunchRecord {
    /// `seat`, `chat` or `terminal` ([`SESSION_KINDS`]).
    pub session_kind: String,
    pub session_id: String,
    /// From 1. Each launch of a session is the next incarnation.
    pub incarnation: u32,
    /// From 1 within an incarnation. A live change writes the next revision.
    pub revision: u32,
    /// `systemd` or `detached` ([`HOST_LAUNCHERS`]).
    pub launcher: String,
    pub host_unit: String,
    pub host_socket: String,
    /// The JSON body. The store keeps its canonical form ([`canonical_json`]).
    pub record: String,
    /// SHA-256 of the canonical body, as 64 lower-case hex digits.
    pub record_sha256: String,
    pub created_at: String,
}

/// One stored launch record. `record` is the canonical body, and `record_sha256` is its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRecordRow {
    pub session_kind: String,
    pub session_id: String,
    pub incarnation: u32,
    pub revision: u32,
    pub launcher: String,
    pub host_unit: String,
    pub host_socket: String,
    pub record: String,
    pub record_sha256: String,
    /// The command that wrote the record. It is unique in the table.
    pub command_id: String,
    pub created_at: String,
}

/// A host row before the store writes it: one session host, started from one revision of one
/// launch record. `launched_revision` and `launched_record_sha256` name what the host was
/// launched from. A restart compares the host's `hello` with them, never with the newest revision.
/// The row holds no secret: the attach key lives in a key file outside the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewHost {
    /// The unit name. It is the key of the registry.
    pub unit: String,
    pub session_kind: String,
    pub session_id: String,
    pub incarnation: u32,
    /// An existing revision of the same incarnation.
    pub launched_revision: u32,
    /// The hash of that revision's body.
    pub launched_record_sha256: String,
    pub launcher: String,
    pub socket: String,
    pub invocation_id: String,
    pub host_binary: String,
    pub host_sha256: String,
    /// The host protocol version the host speaks.
    pub protocol: u32,
    pub started_at: String,
}

/// One row of the host registry. A host is live while `ended_at` is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRow {
    pub unit: String,
    pub session_kind: String,
    pub session_id: String,
    pub incarnation: u32,
    pub launched_revision: u32,
    pub launched_record_sha256: String,
    pub launcher: String,
    pub socket: String,
    pub invocation_id: String,
    pub host_binary: String,
    pub host_sha256: String,
    pub protocol: u32,
    pub started_at: String,
    /// Set once, with `exit`.
    pub ended_at: Option<String>,
    /// JSON text. Set once, with `ended_at`.
    pub exit: Option<String>,
    /// The command that started the host. It is unique in the table.
    pub command_id: String,
}

/// The position an adapter saved for one host (ADR-0003's amendment of 2026-11-16, ADR-0011's
/// kept-and-resent event test): how far it had read in the output a session writes, and what it
/// derived up to there. The store keeps one row for each unit, and never reads inside `position`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPositionRow {
    pub unit: String,
    /// The saves of this unit so far, from 1.
    pub seq: u64,
    /// The JSON text the adapter saved.
    pub position: String,
    /// The command that wrote this `seq`.
    pub command_id: String,
    pub updated_at: String,
}

// ------------------------------------------------------------------ notices (ADR-0003's amendment of 2026-11-21)

/// The events of a notice: a request opened, was revised, or closed (answered or withdrawn).
pub const NOTICE_EVENTS: [&str; 3] = ["opened", "revised", "closed"];

/// A notice: one record of one request event, in one sequence for the store. The store keeps a
/// channel and a target as opaque text. A row never changes and is never deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeRow {
    /// The store's sequence, from 1.
    pub position: u64,
    pub request_id: String,
    pub revision: u32,
    /// One of [`NOTICE_EVENTS`].
    pub event: String,
    pub run_id: String,
    /// `None` for a run with no project.
    pub project_id: Option<String>,
    pub created_at: String,
    /// The command that added the notice.
    pub command_id: String,
}

/// One delivery of a new notice, for one channel. `why` is `None` for a delivery that waits, and
/// the reason for a delivery that is superseded at birth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewDelivery {
    pub channel: String,
    /// The controller's own text, redacted and cut before it comes here.
    pub text: String,
    pub why: Option<String>,
}

/// A notice to add. The store gives it the next position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewNotice {
    pub request_id: String,
    pub revision: u32,
    /// One of [`NOTICE_EVENTS`].
    pub event: String,
    pub run_id: String,
    pub project_id: Option<String>,
    pub created_at: String,
    /// One delivery for each channel, at least one.
    pub deliveries: Vec<NewDelivery>,
}

/// Whether a delivery still waits for its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    Waiting,
    Superseded,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryState::Waiting => "waiting",
            DeliveryState::Superseded => "superseded",
        }
    }
}

/// One delivery of a notice, for one channel. The one change a row allows is `Waiting` to
/// `Superseded`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryRow {
    pub position: u64,
    pub channel: String,
    pub text: String,
    pub state: DeliveryState,
    /// `None` while the delivery waits.
    pub why: Option<String>,
    pub updated_at: String,
}

/// What one target has acknowledged: every notice of its channel at or below `acked`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeTargetRow {
    pub target: String,
    pub channel: String,
    /// A position. It only rises.
    pub acked: u64,
    /// The outcome of the last acknowledgement.
    pub outcome: String,
    pub updated_at: String,
}

/// How often one notice was sent to one target, and when last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeSendRow {
    pub position: u64,
    pub target: String,
    pub sends: u64,
    pub last_sent_at: String,
}

/// The canonical form of a JSON value: no white space and every object's keys in order, so the
/// same value always gives the same bytes.
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The SHA-256 of a launch record's canonical body, as 64 lower-case hex digits.
pub fn record_sha256(canonical_body: &str) -> String {
    crate::claim::sha256_hex(canonical_body.as_bytes())
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A secret value as a record may hold it: `sha256:<64 hex>` or `ref:<name>`.
fn is_secret_stand_in(value: &str) -> bool {
    if let Some(hex) = value.strip_prefix("sha256:") {
        return is_sha256_hex(hex);
    }
    value.strip_prefix("ref:").is_some_and(|name| {
        !name.is_empty() && name.chars().all(|c| !c.is_whitespace() && !c.is_control())
    })
}

fn check_kind_and_launcher(kind: &str, launcher: &str) -> Result<(), String> {
    if !SESSION_KINDS.contains(&kind) {
        return Err(format!(
            "session kind {kind:?} is not one of {}",
            SESSION_KINDS.join(", ")
        ));
    }
    if !HOST_LAUNCHERS.contains(&launcher) {
        return Err(format!(
            "launcher {launcher:?} is not one of {}",
            HOST_LAUNCHERS.join(", ")
        ));
    }
    Ok(())
}

impl NewLaunchRecord {
    /// Seal a body: the canonical text and its hash, for a caller that builds the record.
    pub fn seal(body: &str) -> Result<(String, String), String> {
        let value: serde_json::Value = serde_json::from_str(body)
            .map_err(|e| format!("the launch record is not JSON: {e}"))?;
        let canonical = canonical_json(&value);
        let hash = record_sha256(&canonical);
        Ok((canonical, hash))
    }

    /// The checks that need no stored state, before any write. They answer the canonical body that
    /// the store keeps, or the one-line reason the record is refused:
    ///
    /// - the kind is `seat`, `chat` or `terminal`, and the launcher is `systemd` or `detached`;
    /// - the body is a JSON object;
    /// - every `env` value of a name in `secret_names` is `sha256:<64 hex>` or `ref:<name>`;
    /// - `record_sha256` is the SHA-256 of the canonical body.
    ///
    /// The next revision and the next incarnation need the store, and the store checks them.
    pub fn check(&self) -> Result<String, String> {
        check_kind_and_launcher(&self.session_kind, &self.launcher)?;
        if self.session_id.is_empty() {
            return Err("the launch record names no session".to_string());
        }
        if self.incarnation == 0 || self.revision == 0 {
            return Err("the incarnation and the revision start at 1".to_string());
        }
        let value: serde_json::Value = serde_json::from_str(&self.record)
            .map_err(|e| format!("the launch record is not JSON: {e}"))?;
        let Some(body) = value.as_object() else {
            return Err("the launch record is not a JSON object".to_string());
        };
        let secret_names: Vec<&str> = match body.get("secret_names") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(names)) => names
                .iter()
                .map(|n| {
                    n.as_str()
                        .ok_or("secret_names holds a name that is not text")
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("secret_names is not a list".to_string()),
        };
        let env = match body.get("env") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Object(env)) => Some(env),
            Some(_) => return Err("env is not an object".to_string()),
        };
        if let Some(env) = env {
            for name in secret_names {
                let Some(value) = env.get(name) else {
                    continue;
                };
                match value.as_str() {
                    Some(text) if is_secret_stand_in(text) => {}
                    _ => {
                        return Err(format!(
                            "the value of the secret {name} is not sha256:<64 hex digits> or ref:<name>"
                        ));
                    }
                }
            }
        }
        let canonical = canonical_json(&value);
        if self.record_sha256 != record_sha256(&canonical) {
            return Err(
                "record_sha256 is not the SHA-256 of the canonical record body".to_string(),
            );
        }
        Ok(canonical)
    }
}

impl NewHost {
    /// The checks that need no stored state, before any write. The launched revision and its hash
    /// need the store, and the store checks them.
    pub fn check(&self) -> Result<(), String> {
        check_kind_and_launcher(&self.session_kind, &self.launcher)?;
        if self.unit.is_empty() {
            return Err("the host row names no unit".to_string());
        }
        if self.session_id.is_empty() {
            return Err("the host row names no session".to_string());
        }
        if self.incarnation == 0 || self.launched_revision == 0 {
            return Err("the incarnation and the launched revision start at 1".to_string());
        }
        if !is_sha256_hex(&self.launched_record_sha256) {
            return Err("launched_record_sha256 is not 64 hex digits".to_string());
        }
        Ok(())
    }
}

/// One row of the clear grant (ADR-0005 and ADR-0009, amended): the grant of one project at one
/// generation. A project has at most one row with no revoke, and its generations run 1 to n with no
/// gap. A revoke is set once on its row, and a row is never deleted. A controller entity: the
/// engine core never writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClearGrantRow {
    /// A project's ID, `P-<n>`.
    pub project_id: String,
    /// From 1. The project's highest generation plus 1 for a new row.
    pub generation: u64,
    /// The agent that holds the grant. The store keeps it as opaque text.
    pub agent_id: String,
    pub granted_by: String,
    pub granted_at: String,
    /// Set together with `revoked_at`, once.
    pub revoked_by: Option<String>,
    pub revoked_at: Option<String>,
    /// The command that gave the grant. It is unique.
    pub command_id: String,
}

/// A clear grant before the store writes it. The command ID is the commit's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewClearGrant {
    pub project_id: String,
    /// The project's highest generation plus 1, or 1 when it has none.
    pub generation: u64,
    pub agent_id: String,
    pub granted_by: String,
    pub granted_at: String,
}

/// A state write inside one commit. A row replaces the row with the same key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    PutRun(RunRow),
    PutNode(NodeRow),
    PutSession(SessionRow),
    /// Open a request. The store gives it the next `R-<n>`.
    OpenRequest(NewRequest),
    /// Replace the request with this ID. It must exist.
    PutRequest(RequestRow),
    /// Insert the claim of this generation, or replace it.
    PutClaim(ClaimRow),
    /// Add a project. The store gives it the next `P-<n>`. A live project with the same root or
    /// the same name makes the commit [`Committed::Rejected`].
    AddProject(NewProject),
    /// Mark a live project removed. An unknown or removed ID makes the commit
    /// [`Committed::Rejected`].
    RemoveProject {
        id: String,
        removed_at: String,
    },
    /// Add a device. The store gives it the next `D-<n>`. A token hash that a device holds already
    /// makes the commit [`Committed::Rejected`].
    AddDevice(NewDevice),
    /// Mark a live device revoked. An unknown or revoked ID makes the commit
    /// [`Committed::Rejected`].
    RevokeDevice {
        id: String,
        revoked_at: String,
    },
    /// Set the kind of a live project that has none (the backfill at controller start). An
    /// unknown or removed project, or one whose kind is set, makes the commit
    /// [`Committed::Rejected`].
    SetProjectKind {
        id: String,
        kind: ProjectKind,
        remote_key: Option<String>,
    },
    /// Replace the remote key of a live `git` project (inside a chat session start). An unknown or
    /// removed project, or a project that is not `git`, makes the commit [`Committed::Rejected`].
    SetProjectRemoteKey {
        id: String,
        remote_key: Option<String>,
    },
    /// Start a chat session. The store gives it the next `S-<n>` (as [`NEW_ID`]) and, for
    /// [`SessionWorktree::Record`], the next `W-<n>` (as [`WORKTREE_ID`]).
    StartChatSession(NewChatSession),
    /// End a live chat session. An unknown or ended session makes the commit
    /// [`Committed::Rejected`].
    EndChatSession {
        id: String,
        ended_at: String,
    },
    /// Change the model, effort, permission mode or runtime options of a live chat session. A `None`
    /// field stays as it was. `option_changes` merge key by key into the session's map inside the
    /// commit, so two sets that overlap each keep their own keys (when the session's map is empty,
    /// the agent's options are the base). An unknown or ended session makes the commit
    /// [`Committed::Rejected`].
    SetChatSessionRuntime {
        id: String,
        model: Option<String>,
        effort: Option<String>,
        permission_mode: Option<String>,
        option_changes: Option<BTreeMap<String, String>>,
        updated_at: String,
    },
    /// Redact one chat item (C12): the payload becomes [`REDACTED_PAYLOAD`], each tool text that
    /// the item holds becomes [`REDACTED_TEXT`], and every other column stays. An unknown item, or
    /// one that is redacted already, makes the commit [`Committed::Rejected`].
    RedactChatItem {
        session_id: String,
        seq: u64,
        redacted_at: String,
    },
    /// Insert a setting row, or replace the row at the same scope and key. An unknown or removed
    /// project, or an unknown agent, makes the commit [`Committed::Rejected`] (ADR-0016).
    SetSetting(SettingRow),
    /// Delete the setting row at this scope and key. When no row is there, the commit is
    /// [`Committed::Rejected`] (`no value of <key> is set at <scope>`).
    UnsetSetting {
        scope: SettingScope,
        key: String,
    },
    /// Link a run to a live project (a run armed from a start card). Allowed only in an admission
    /// under [`CONTROLLER_SCOPE`]. An unknown run, an unknown or removed project, a run that
    /// names another project already, or a scope other than the controller's makes the commit
    /// [`Committed::Rejected`]. The same pair a second time is a no-op that admits.
    SetRunProject {
        run_id: String,
        project_id: String,
    },
    /// Set a run's armed launch count, once (`arm` writes it). Allowed only in an admission under
    /// [`CONTROLLER_SCOPE`]. An unknown run, a run that holds another value already, or a scope other
    /// than the controller's makes the commit [`Committed::Rejected`]. The same value a second time
    /// is a no-op that admits. A database guard backs the rule: an update that changes a set value
    /// fails.
    SetRunLaunchBound {
        run_id: String,
        bound: u64,
    },
    /// Write one revision of a launch record (ADR-0003's amendment of 2026-11-16). Allowed only in
    /// an admission under [`CONTROLLER_SCOPE`]. A kind or launcher that is not listed, a secret
    /// value in plain text, a revision or an incarnation that is not the next one, or a
    /// `record_sha256` that is not the hash of the body makes the commit [`Committed::Rejected`].
    LaunchRecordWrite(NewLaunchRecord),
    /// Register a host that started from one revision of a launch record. Allowed only in an
    /// admission under [`CONTROLLER_SCOPE`]. A launched revision that does not exist, a hash that
    /// is not that revision's, or a unit or an incarnation that has a host already makes the
    /// commit [`Committed::Rejected`].
    HostStarted(NewHost),
    /// End a live host with its exit (JSON). It sets the end and the exit once, together. Allowed
    /// only in an admission under [`CONTROLLER_SCOPE`]. An unknown or ended host makes the commit
    /// [`Committed::Rejected`].
    HostEnded {
        unit: String,
        ended_at: String,
        exit: String,
    },
    /// Save the position an adapter has read to for the host of `unit` (JSON text). Allowed only
    /// in an admission under [`CONTROLLER_SCOPE`]. `seq` must be the stored one plus 1, or 1 when
    /// the unit has none. A unit with no host row (live or ended), a `seq` that is not the next
    /// one, a position that is not JSON, or a scope other than the controller's makes the commit
    /// [`Committed::Rejected`].
    HostPositionWrite {
        unit: String,
        seq: u64,
        position: String,
        updated_at: String,
    },
    /// Add a notice with its deliveries (ADR-0003's amendment of 2026-11-21). The store gives it the
    /// next position (as [`NEW_ID`]). Allowed only in an admission under [`CONTROLLER_SCOPE`]. A
    /// second notice of one `(request, revision, event)`, an event that is not one of
    /// [`NOTICE_EVENTS`], a revision of 0, no delivery, a channel twice or an empty channel, or a
    /// scope other than the controller's makes the commit [`Committed::Rejected`].
    NoticeAdd(NewNotice),
    /// Supersede the delivery of `position` on `channel`, with a reason. A delivery that is not
    /// there, one that is superseded already, an empty reason, or a scope other than the
    /// controller's makes the commit [`Committed::Rejected`].
    NoticeSupersede {
        position: u64,
        channel: String,
        why: String,
        updated_at: String,
    },
    /// Record one send of a notice to a target. `sends` must be the stored count plus 1 (1 when
    /// there is none). A notice that is not there, a `sends` that is not the next one, or a scope
    /// other than the controller's makes the commit [`Committed::Rejected`].
    NoticeSend {
        position: u64,
        target: String,
        sends: u64,
        sent_at: String,
    },
    /// Make a target's row, or raise its `acked`. A position above the newest notice, a position at
    /// or below the stored one, a channel that is not the row's, an empty target or channel, or a
    /// scope other than the controller's makes the commit [`Committed::Rejected`]. A new row may
    /// take any position from 0 to the newest notice.
    NoticeAck {
        target: String,
        channel: String,
        position: u64,
        outcome: String,
        updated_at: String,
    },
    /// Give a live project its clear grant at the next generation (ADR-0005, amended). Allowed only
    /// in an admission under [`CONTROLLER_SCOPE`]. An unknown or removed project, a project whose
    /// grant is live, a generation that is not the project's highest plus 1, or a scope other than
    /// the controller's makes the commit [`Committed::Rejected`].
    GrantClear(NewClearGrant),
    /// End a project's grant: set the revoke on the row of `generation`, once. Allowed only in an
    /// admission under [`CONTROLLER_SCOPE`]. A row that is not there, a row that is revoked already,
    /// or a scope other than the controller's makes the commit [`Committed::Rejected`]. A removed
    /// project's row can be revoked: the operator can always end what the store still holds.
    RevokeClearGrant {
        project_id: String,
        generation: u64,
        revoked_by: String,
        revoked_at: String,
    },
    /// The precondition "this project's grant is live at this generation". It writes nothing. The
    /// store checks it inside the commit's transaction, and rejects the commit, with nothing written
    /// (no admission, no outbox line and no other effect of the commit), when the project has no row
    /// with no revoke, when that row has another generation, or when the project is removed or is not
    /// in the store. It is admitted in every scope, a run's included.
    RequireLiveGrant {
        project_id: String,
        generation: u64,
    },
}

/// What the engine wants on the ledger. The store assigns the sequence number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub ledger_file: String,
    /// One line, without the sequence number and without a newline.
    pub body: String,
}

/// One row of the ledger outbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerLine {
    pub ledger_file: String,
    /// Assigned per ledger file, rising across runs.
    pub seq: u64,
    pub run_id: String,
    pub body: String,
    pub written: bool,
}

/// One atomic commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub run_id: String,
    pub command_id: String,
    pub content: String,
    pub admission: NewAdmission,
    pub effects: Vec<Effect>,
    pub ledger: Option<LedgerEntry>,
}

/// What the store did with a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Committed {
    /// The commit is stored. `ledger` is the outbox row, when the commit had a ledger entry.
    Applied {
        admission: AdmissionRow,
        ledger: Option<LedgerLine>,
    },
    /// The same ID with the same content was stored before. Nothing changed.
    Replayed(AdmissionRow),
    /// The same ID with other content was stored before. Nothing changed.
    Conflict(AdmissionRow),
    /// An effect cannot apply (a duplicate live project root or name, an unknown or removed
    /// project, a token hash in use, an unknown or revoked device). The reason is one line. Nothing was written: no admission, no outbox line. The
    /// caller may commit a refused admission for the same command.
    Rejected(String),
}

/// Why a store call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "store failed: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

pub trait Store: Send + Sync {
    /// Commit one admission atomically. Admissions serialize here (ADR-0009): of several
    /// commits with one command ID, one is `Applied` and the others are `Replayed` or `Conflict`.
    fn commit(&self, commit: Commit) -> Result<Committed, StoreError>;
    /// Read one run.
    fn run(&self, run_id: &str) -> Result<Option<RunRow>, StoreError>;
    /// Read a run's nodes, in creation order.
    fn nodes(&self, run_id: &str) -> Result<Vec<NodeRow>, StoreError>;
    /// Read a run's sessions, in creation order.
    fn sessions(&self, run_id: &str) -> Result<Vec<SessionRow>, StoreError>;
    /// Read a run's requests, in the order they were opened.
    fn requests(&self, run_id: &str) -> Result<Vec<RequestRow>, StoreError>;
    /// Read one request by ID, whatever run it belongs to.
    fn request(&self, id: &str) -> Result<Option<RequestRow>, StoreError>;
    /// Read every claim of the store, by generation.
    fn claims(&self) -> Result<Vec<ClaimRow>, StoreError>;
    /// Read the live projects, in the order they were added.
    fn projects(&self) -> Result<Vec<ProjectRow>, StoreError>;
    /// Read one project by ID, live or removed.
    fn project(&self, id: &str) -> Result<Option<ProjectRow>, StoreError>;
    /// Read every device, live ones first and each group in pairing order.
    fn devices(&self) -> Result<Vec<DeviceRow>, StoreError>;
    /// Read the live device whose token has this SHA-256. A revoked device is not returned.
    fn device_by_token(&self, token_sha256: &str) -> Result<Option<DeviceRow>, StoreError>;
    /// Write a live device's last-seen time and peer. This is telemetry, not a transition: it makes
    /// no admission and no ledger line (ADR-0003). It writes at most once a minute per device and
    /// answers whether it wrote. A revoked or unknown device is never written.
    fn touch_device(&self, id: &str, at: &str, peer: &str) -> Result<bool, StoreError>;
    /// Read a run's admissions, in admission order.
    fn admissions(&self, run_id: &str) -> Result<Vec<AdmissionRow>, StoreError>;
    /// Read one admission by command ID.
    fn admission(&self, run_id: &str, command_id: &str)
    -> Result<Option<AdmissionRow>, StoreError>;
    /// List the outbox lines of one ledger file that are not marked written, in sequence order.
    /// The list covers every run that used the file.
    fn unwritten_ledger_lines(&self, ledger_file: &str) -> Result<Vec<LedgerLine>, StoreError>;
    /// Mark one outbox line written.
    fn mark_ledger_written(&self, ledger_file: &str, seq: u64) -> Result<(), StoreError>;

    /// Read one worktree row by ID, whatever its state.
    fn worktree(&self, id: &str) -> Result<Option<WorktreeRow>, StoreError>;
    /// Read every worktree row of a project, in creation order.
    fn worktrees(&self, project_id: &str) -> Result<Vec<WorktreeRow>, StoreError>;
    /// Read the live worktree row (no `gone_at`, no `removed_at`) at a path.
    fn live_worktree_at(&self, path: &str) -> Result<Option<WorktreeRow>, StoreError>;
    /// Read every agent, in creation order.
    fn agents(&self) -> Result<Vec<AgentRow>, StoreError>;
    /// Read one agent by ID.
    fn agent(&self, id: &str) -> Result<Option<AgentRow>, StoreError>;
    /// Read one chat session by ID, live or ended.
    fn chat_session(&self, id: &str) -> Result<Option<ChatSessionRow>, StoreError>;
    /// Read every chat session of a project, in creation order.
    fn chat_sessions(&self, project_id: &str) -> Result<Vec<ChatSessionRow>, StoreError>;
    /// Read every live chat session (`ended_at` empty), in creation order.
    fn live_chat_sessions(&self) -> Result<Vec<ChatSessionRow>, StoreError>;
    /// Read up to `limit` items of a session with a `seq` above `after_seq`, in ascending `seq`.
    fn chat_items_after(
        &self,
        session_id: &str,
        after_seq: u64,
        limit: u32,
    ) -> Result<Vec<ChatItemRow>, StoreError>;
    /// Read one chat item.
    fn chat_item(&self, session_id: &str, seq: u64) -> Result<Option<ChatItemRow>, StoreError>;
    /// Read the approval and question requests of a session that have no answer item yet, in
    /// `seq` order.
    fn pending_requests(&self, session_id: &str) -> Result<Vec<ChatItemRow>, StoreError>;
    /// Read every setting row, by key then scope text (ADR-0016). The name is not `settings`:
    /// `SqliteStore::settings` reads the `PRAGMA` values.
    fn setting_rows(&self) -> Result<Vec<SettingRow>, StoreError>;
    /// Read the rows of one key on the scope chain of a context, most specific first
    /// ([`SettingScope::chain`]).
    fn setting_chain(
        &self,
        key: &str,
        project_id: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Vec<SettingRow>, StoreError>;

    /// Mark a worktree gone, for display: what the file system or git showed. It writes only a
    /// row that is not gone and not removed, and answers whether it wrote. It makes no admission
    /// and no ledger line (ADR-0003): the admission checks the worktree live and never decides on
    /// the mark.
    fn mark_worktree_gone(&self, id: &str, at: &str) -> Result<bool, StoreError>;
    /// Write a live chat session's state, which is the runtime's turn state and changes on every
    /// turn. It writes `updated_at` too, answers whether it wrote, and makes no admission and no
    /// ledger line (ADR-0003). An ended or unknown session is never written.
    fn set_chat_session_state(
        &self,
        id: &str,
        state: ChatSessionState,
        at: &str,
    ) -> Result<bool, StoreError>;
    /// Write the ID that the runtime reports for its own session. A later ref replaces an earlier
    /// one. It is a fact of the runtime, not a command: no admission and no ledger line. An ended
    /// or unknown session is never written.
    fn set_chat_runtime_ref(
        &self,
        id: &str,
        runtime_session_ref: &str,
        at: &str,
    ) -> Result<bool, StoreError>;
    /// Append one item to a session's display log, live or ended. It gives the item the session's
    /// next `seq`, caps a tool text over [`TOOL_TEXT_MAX_BYTES`], and adds a `turn_result`'s usage
    /// to the session's totals, in one transaction. It makes no admission and no ledger line
    /// (ADR-0003). An error writes nothing.
    fn append_chat_item(&self, item: NewChatItem) -> Result<ChatItemRow, StoreError>;
    /// Write a position that comes with no item (the adapter's state between turns, the start of
    /// a turn, or the exit), by the rules of [`NewChatItem::host`]. It makes no admission and no ledger line: the display log and its
    /// cursor are written outside admission (ADR-0003). An unknown session is an error.
    fn set_chat_host_mark(&self, session_id: &str, mark: ChatHostMark) -> Result<(), StoreError>;

    /// Read one revision of a session's launch record.
    fn launch_record(
        &self,
        session_kind: &str,
        session_id: &str,
        incarnation: u32,
        revision: u32,
    ) -> Result<Option<LaunchRecordRow>, StoreError>;
    /// Read the newest revision of a session's incarnation: the one that wins.
    fn newest_launch_record(
        &self,
        session_kind: &str,
        session_id: &str,
        incarnation: u32,
    ) -> Result<Option<LaunchRecordRow>, StoreError>;
    /// Read the host rows with no end, in the order they started. A row reads live until the
    /// controller records the exit: after a crash, every row can read live, so a caller checks
    /// the unit before it trusts the row.
    fn live_hosts(&self) -> Result<Vec<HostRow>, StoreError>;
    /// Read one host row by unit, live or ended.
    fn host(&self, unit: &str) -> Result<Option<HostRow>, StoreError>;
    /// Read the last position saved for the host of `unit`, or `None` when none was saved.
    fn host_position(&self, unit: &str) -> Result<Option<HostPositionRow>, StoreError>;

    /// Read one notice by position.
    fn notice(&self, position: u64) -> Result<Option<NoticeRow>, StoreError>;
    /// Read up to `limit` notices above `position`, in ascending position.
    fn notices_after(&self, position: u64, limit: u32) -> Result<Vec<NoticeRow>, StoreError>;
    /// The position of the newest notice, or 0 when the store holds none.
    fn newest_notice(&self) -> Result<u64, StoreError>;
    /// Read the deliveries of one notice, by channel.
    fn notice_deliveries(&self, position: u64) -> Result<Vec<DeliveryRow>, StoreError>;
    /// Read the deliveries of one channel that wait, in ascending position.
    fn waiting_deliveries(&self, channel: &str) -> Result<Vec<DeliveryRow>, StoreError>;
    /// Read every target, by target.
    fn notice_targets(&self) -> Result<Vec<NoticeTargetRow>, StoreError>;
    /// Read the sends of one notice, by target.
    fn notice_sends(&self, position: u64) -> Result<Vec<NoticeSendRow>, StoreError>;
    /// Every clear-grant row, by project (in the order of the project numbers) and generation.
    fn clear_grants(&self) -> Result<Vec<ClearGrantRow>, StoreError>;
}
