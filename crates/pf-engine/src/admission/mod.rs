//! Admission: every state change is a command with an ID and its content as canonical JSON.
//!
//! The store keeps the content bytes and compares them, so no hash crate is needed. The same ID
//! with the same content returns the stored record. The same ID with changed content is a
//! conflict. Admissions serialize at commit (ADR-0009).

mod decide;

pub use decide::{Decision, decide};

use serde::{Deserialize, Serialize};

use crate::port::agent::Usage;
use crate::port::forge::PrRef;
use crate::port::store::{AdmissionRow, NodeKind, NodeRow, RunRow, SessionRow};
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
    /// Admit one unit from a unit record.
    AdmitUnit { unit: UnitRecord },
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
    /// Park a unit that is `running`, with its reason.
    Park { unit_id: String, reason: String },
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
    /// End the run.
    EndRun { reason: String },
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
            CommandBody::EndRun { .. } => "end_run",
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
