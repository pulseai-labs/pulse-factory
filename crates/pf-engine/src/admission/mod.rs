//! Admission: every state change is a command with an ID and its content as canonical JSON.
//!
//! The store keeps the content bytes and compares them, so no hash crate is needed. The same ID
//! with the same content returns the stored record. The same ID with changed content is a
//! conflict. Admissions serialize at commit (ADR-0009).

mod decide;

pub(crate) use decide::unknown_track;
pub use decide::{Decision, decide};

use serde::{Deserialize, Serialize};

use crate::port::agent::Usage;
use crate::port::forge::PrRef;
use crate::port::store::{
    AdmissionRow, ClaimRow, NodeKind, NodeRow, RequestKind, RequestRow, RequestStatus, RunRow,
    SessionRow,
};
use crate::port::unit_source::UnitRecord;
use crate::status::{EndStatus, Status};
use crate::unit::{Signal, UnitDetail};

/// One command. The ID is chosen by the caller and is unique within a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub body: CommandBody,
}

/// What a command asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandBody {
    /// Open the run. The engine sends this once at start-up.
    OpenRun {
        workflow: String,
        ledger_file: String,
    },
    /// Admit one unit from a unit record, onto a track.
    AdmitUnit { unit: UnitRecord, track: String },
    /// Launch the unit's next stage. One admitted launch writes one launch record.
    Launch {
        unit_id: String,
        stage: String,
        role: String,
        model: String,
        attempt: u32,
    },
    /// Record the adapter's handle for a launched session.
    Attach {
        session_id: String,
        process_ref: String,
    },
    /// A signal ends a stage. `pr` names the unit's PR, when the signal knows it.
    Signal {
        unit_id: String,
        stage: String,
        signal: Signal,
        pr: Option<PrRef>,
    },
    /// Add token usage and cost to a session.
    Usage { session_id: String, usage: Usage },
    /// End a node with `done`, `failed` or `stopped`.
    End {
        node_id: String,
        status: EndStatus,
        reason: String,
    },
    /// Park a unit that is `running`, with its reason. The same commit opens the unit's request,
    /// of the kind the park's source names.
    Park {
        unit_id: String,
        reason: String,
        #[serde(default)]
        request_kind: RequestKind,
    },
    /// The operator's kill switch: the run admits no further launch and no further merge.
    Halt { reason: String },
    /// Ask to merge a unit's PR.
    Merge { unit_id: String },
    /// Record what a merge did. `landed` ends the unit `done`. Any other result parks it.
    MergeResult {
        unit_id: String,
        repo: String,
        branch: String,
        sha: String,
        outcome: String,
        landed: bool,
    },
    /// A seat's push, reported at the push boundary. It is recorded in every run state, even
    /// after `halt`: the push happened, and the audit trail must show it.
    Push {
        unit_id: String,
        /// The adapter's ID for this push. A repeat of one ID is a replay.
        event_id: String,
        repo: String,
        branch: String,
        sha: String,
        outcome: PushOutcome,
    },
    /// A line a seat wrote to its decisions file, reported by the controller. It is recorded in
    /// every run state and for a unit in any state, as a push is. `text` is one clean line.
    Decision {
        unit_id: String,
        /// The 1-based number of the line in the file. Together with the unit it names the line.
        line_number: u64,
        text: String,
    },
    /// End the run. Its open requests are withdrawn in the same commit.
    EndRun { reason: String },
    /// Open a request. A park opens its own, so this is for a request that no park carries.
    OpenRequest { request: OpenRequestBody },
    /// Change the facts of an open request. `revision` is the one the caller read; the request
    /// moves to the next one.
    ReviseRequest {
        id: String,
        revision: u32,
        /// JSON text.
        body: String,
    },
    /// Answer a request, bound to its exact ID and revision and to the claim generation.
    AnswerRequest {
        id: String,
        revision: u32,
        generation: u64,
        /// `{"verb":"answer","text":…}`.
        answer: serde_json::Value,
    },
    /// Close an open request that nothing will answer: the run ended, or the unit moved on.
    WithdrawRequest { id: String, reason: String },
    /// Take the claim. `generation` is the next one; the old claim is fenced out.
    Claim {
        generation: u64,
        holder: String,
        /// SHA-256 of the token, as 64 hex digits.
        token_hash: String,
    },
    /// Resume a parked unit at a stage, in the claim's name (G9).
    ClearUnit {
        unit_id: String,
        /// `None` is the stage the unit parked in.
        to_stage: Option<String>,
        generation: u64,
        request_id: String,
    },
}

/// What an `OpenRequest` command asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRequestBody {
    /// `None` for a run-level request.
    pub unit_id: Option<String>,
    pub stage: String,
    pub kind: RequestKind,
    pub reason: String,
    /// JSON text.
    pub body: String,
}

/// How a push ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushOutcome {
    Pushed,
    /// The push failed, with a one-line reason.
    Failed(String),
}

impl CommandBody {
    /// The kind name stored in the admission record.
    pub fn kind(&self) -> &'static str {
        match self {
            CommandBody::OpenRun { .. } => "open_run",
            CommandBody::AdmitUnit { .. } => "admit_unit",
            CommandBody::Launch { .. } => "launch",
            CommandBody::Attach { .. } => "attach",
            CommandBody::Signal { .. } => "signal",
            CommandBody::Usage { .. } => "usage",
            CommandBody::End { .. } => "end",
            CommandBody::Park { .. } => "park",
            CommandBody::Halt { .. } => "halt",
            CommandBody::Merge { .. } => "merge",
            CommandBody::MergeResult { .. } => "merge_result",
            CommandBody::Push { .. } => "push",
            CommandBody::Decision { .. } => "decision",
            CommandBody::EndRun { .. } => "end_run",
            CommandBody::OpenRequest { .. } => "open_request",
            CommandBody::ReviseRequest { .. } => "revise_request",
            CommandBody::AnswerRequest { .. } => "answer_request",
            CommandBody::WithdrawRequest { .. } => "withdraw_request",
            CommandBody::Claim { .. } => "claim",
            CommandBody::ClearUnit { .. } => "clear_unit",
        }
    }
}

/// The canonical JSON bytes of a command in a run. Field order is fixed by the types and map
/// keys are sorted, so equal commands give equal bytes.
pub fn canonical_content(run_id: &str, command: &Command) -> String {
    #[derive(Serialize)]
    struct Wire<'a> {
        run: &'a str,
        id: &'a str,
        body: &'a CommandBody,
    }
    serde_json::to_string(&Wire {
        run: run_id,
        id: &command.id,
        body: &command.body,
    })
    .expect("a command always serializes")
}

/// Read the body of a stored command back from its canonical content.
pub fn body_of_content(content: &str) -> Option<CommandBody> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    serde_json::from_value(value.get("body")?.clone()).ok()
}

/// The receipt for one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub row: AdmissionRow,
    /// True when the command was admitted before and this call returned the stored record.
    pub replayed: bool,
}

/// The stored state of one run, as the engine reads it before it decides.
#[derive(Clone, Debug)]
pub struct RunView {
    pub run: Option<RunRow>,
    pub nodes: Vec<NodeRow>,
    pub sessions: Vec<SessionRow>,
    pub admissions: Vec<AdmissionRow>,
    /// The run's requests, in the order they were opened.
    pub requests: Vec<RequestRow>,
    /// Every claim of the store, by generation.
    pub claims: Vec<ClaimRow>,
}

impl RunView {
    pub fn node(&self, id: &str) -> Option<&NodeRow> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn session(&self, id: &str) -> Option<&SessionRow> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// The run's units, in the order they were admitted.
    pub fn units(&self) -> impl Iterator<Item = &NodeRow> {
        self.nodes.iter().filter(|n| n.kind == NodeKind::Unit)
    }

    /// The stage nodes of one unit, in the order they were launched.
    pub fn stages_of(&self, unit_id: &str) -> Vec<&NodeRow> {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Stage && n.parent_id.as_deref() == Some(unit_id))
            .collect()
    }

    /// The stage node of the unit that is still `running`, when there is one.
    pub fn running_stage_of(&self, unit_id: &str) -> Option<&NodeRow> {
        self.stages_of(unit_id)
            .into_iter()
            .find(|n| n.status == Status::Running)
    }

    pub fn request(&self, id: &str) -> Option<&RequestRow> {
        self.requests.iter().find(|r| r.id == id)
    }

    /// The unit's open request, when it has one. A unit has at most one.
    pub fn open_request_of(&self, unit_id: &str) -> Option<&RequestRow> {
        self.requests
            .iter()
            .find(|r| r.status == RequestStatus::Open && r.unit_id.as_deref() == Some(unit_id))
    }

    /// The requests that are still open.
    pub fn open_requests(&self) -> impl Iterator<Item = &RequestRow> {
        self.requests
            .iter()
            .filter(|r| r.status == RequestStatus::Open)
    }

    /// The current claim: the highest generation with no `fenced_at`.
    pub fn current_claim(&self) -> Option<&ClaimRow> {
        self.claims
            .iter()
            .filter(|c| c.fenced_at.is_none())
            .max_by_key(|c| c.generation)
    }

    /// The highest generation any claim has had, fenced or not. 0 before the first claim.
    pub fn last_claim_generation(&self) -> u64 {
        self.claims.iter().map(|c| c.generation).max().unwrap_or(0)
    }

    pub fn is_halted(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.halted_reason.is_some())
    }

    /// Read a unit node's detail.
    pub fn unit_detail(&self, unit: &NodeRow) -> Result<UnitDetail, String> {
        UnitDetail::from_text(&unit.detail)
    }
}

/// Why the engine did not admit or refuse a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    /// The command ID was used before with other content.
    Conflict { command_id: String },
    /// The command is not valid in the run's present state. Nothing was recorded.
    Rejected(String),
    /// The engine stopped admitting work after a store or ledger failure.
    Degraded(String),
    /// A store call failed.
    Store(String),
    /// A ledger file call failed.
    Ledger(String),
    /// An adapter or forge call failed in a way the engine cannot record as a park.
    Port(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Conflict { command_id } => write!(
                f,
                "command {command_id} was admitted before with other content"
            ),
            EngineError::Rejected(why) => write!(f, "command rejected: {why}"),
            EngineError::Degraded(why) => write!(f, "engine stopped admitting work: {why}"),
            EngineError::Store(why) => write!(f, "{why}"),
            EngineError::Ledger(why) => write!(f, "ledger failed: {why}"),
            EngineError::Port(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for EngineError {}
