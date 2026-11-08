//! The store port (ADR-0003).
//!
//! The port carries one atomic commit: the state transition, its command and receipt, and its
//! ledger outbox line, together or not at all. It shows no SQL.

use serde::{Deserialize, Serialize};

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
/// commit's transaction. A commit has one effect that assigns an ID.
pub const NEW_ID: &str = "{new-id}";

/// One project: a controller entity. The engine core never reads it. Only the root is decided; the
/// project's other fields wait for the project hierarchy.
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
}

/// A project before the store gives it an ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewProject {
    pub name: String,
    pub root: String,
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
}
