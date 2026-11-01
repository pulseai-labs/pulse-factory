//! The store port (ADR-0003).
//!
//! The port carries one atomic commit: the state transition, its command and receipt, and its
//! ledger outbox line, together or not at all. It shows no SQL.

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
    /// For a launch: the unit, role, model and attempt.
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

/// A state write inside one commit. A row replaces the row with the same key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    PutRun(RunRow),
    PutNode(NodeRow),
    PutSession(SessionRow),
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
}
