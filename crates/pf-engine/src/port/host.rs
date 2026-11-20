//! The session-host port (ADR-0001's amendment of 2026-11-16, ADR-0002).
//!
//! A session runs in a host outside the controller. An adapter must not start a host itself: the
//! launch record must be written before the host starts, and the registry row after it, and both
//! are the controller's. So the controller hands each adapter an `Arc<dyn SessionHosts>`.
//!
//! The port names no runtime, no launcher and no host crate. Its words are neutral: a session
//! kind, a unit, an incarnation, a record body, and the events `Out`, `Gap` and `Exit`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

/// How the child's input and output are connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostTransport {
    /// A pipe each for input and output.
    Pipes,
    /// A terminal of this size.
    Terminal { rows: u16, cols: u16 },
}

/// What an adapter asks the controller to start in a host.
#[derive(Clone, Debug)]
pub struct HostLaunch {
    /// The session kind: `seat`, `chat` or `terminal`.
    pub session_kind: String,
    /// The key of the session. A launch with the same kind and key is the next incarnation of the
    /// same session.
    pub session_key: String,
    /// The child's command line. The text `{unit}` in a word is replaced by the host's unit name.
    pub argv: Vec<String>,
    pub cwd: String,
    /// The child's environment, secret values left out. The text `{unit}` in a value is replaced
    /// by the host's unit name, which the adapter cannot know before the launch.
    pub env: BTreeMap<String, String>,
    /// Files the child expects to exist, empty, when it starts. The text `{unit}` in a path is
    /// replaced by the unit name. The service makes each file, and its directory, before the host
    /// starts. A file that exists is left as it is.
    pub files: Vec<String>,
    /// The child's secret variables, by name. The record holds `sha256:<hex>` of each value.
    pub secrets: BTreeMap<String, String>,
    pub transport: HostTransport,
    /// The record body fields `runtime`, `settings`, `run_binding` and `resume`, as JSON.
    pub runtime: Value,
    pub settings: Value,
    pub run_binding: Value,
    pub resume: Value,
}

/// Why a host call failed, in words an operator can read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

/// A signal for the child's process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSignal {
    Interrupt,
    Terminate,
    Kill,
}

/// How the child ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// The output cursor after the last byte.
    pub at: u64,
}

/// What a host streams after a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    /// Output bytes that start at cursor `at`.
    Out { at: u64, data: Vec<u8> },
    /// Output from `from` to `to` was lost to an overflow.
    Gap { from: u64, to: u64 },
    /// The child ended, and every output byte was sent.
    Exit(HostExit),
}

/// One attached host. The controller implements it over its connection.
pub trait HostSession: Send + Sync {
    /// Start streaming output from this cursor.
    fn read(&self, from: u64) -> Result<(), HostError>;
    /// The events that arrive within `wait`, oldest first. Empty when none came.
    fn events(&self, wait: Duration) -> Result<Vec<HostEvent>, HostError>;
    /// Say the output below `to` is consumed. Call it after the commit that consumed it.
    fn ack(&self, to: u64) -> Result<(), HostError>;
    fn input(&self, data: &[u8]) -> Result<(), HostError>;
    fn signal(&self, signal: HostSignal) -> Result<(), HostError>;
    /// Say the exit was consumed. The service ends the host's row first.
    fn exit_ack(&self) -> Result<(), HostError>;
    /// Stop the host and its child. It is safe to repeat.
    fn stop(&self) -> Result<(), HostError>;
}

/// A host the controller attached to.
#[derive(Clone)]
pub struct HostHandle {
    pub unit: String,
    pub incarnation: u32,
    /// The launched record body, as the store holds it.
    pub record: Value,
    /// The child's process ID, from the host's `hello`.
    pub child_pid: u32,
    /// The exit, when the child had ended already at the attach.
    pub exit: Option<HostExit>,
    session: Arc<dyn HostSession>,
}

impl std::fmt::Debug for HostHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostHandle")
            .field("unit", &self.unit)
            .field("incarnation", &self.incarnation)
            .field("child_pid", &self.child_pid)
            .field("exit", &self.exit)
            .finish()
    }
}

impl HostHandle {
    pub fn new(
        unit: String,
        incarnation: u32,
        record: Value,
        child_pid: u32,
        exit: Option<HostExit>,
        session: Arc<dyn HostSession>,
    ) -> HostHandle {
        HostHandle {
            unit,
            incarnation,
            record,
            child_pid,
            exit,
            session,
        }
    }

    pub fn read(&self, from: u64) -> Result<(), HostError> {
        self.session.read(from)
    }

    pub fn events(&self, wait: Duration) -> Result<Vec<HostEvent>, HostError> {
        self.session.events(wait)
    }

    pub fn ack(&self, to: u64) -> Result<(), HostError> {
        self.session.ack(to)
    }

    pub fn input(&self, data: &[u8]) -> Result<(), HostError> {
        self.session.input(data)
    }

    pub fn signal(&self, signal: HostSignal) -> Result<(), HostError> {
        self.session.signal(signal)
    }

    pub fn exit_ack(&self) -> Result<(), HostError> {
        self.session.exit_ack()
    }

    pub fn stop(&self) -> Result<(), HostError> {
        self.session.stop()
    }
}

/// What `rebind` found for one unit after a controller restart.
#[derive(Debug)]
pub enum Rebind {
    /// The host runs, and the controller attached with every check passed.
    Survived(HostHandle),
    /// The unit is gone. `exit` is what it left: the exit file's body, or the text "ended, cause
    /// unknown" when the unit is gone and left no exit file.
    Lost { exit: Value },
    /// The unit may run, and no valid `hello` came, or a check refused it.
    Unreachable { why: String },
}

/// The controller's host service, as an adapter sees it.
pub trait SessionHosts: Send + Sync {
    /// Write the launch record, start the host, attach with the checks, and register the host.
    /// An older incarnation of the same session ends first.
    fn launch(&self, launch: HostLaunch) -> Result<HostHandle, HostError>;
    /// Attach to the host of `unit` again after a restart. It commits the host's end when the
    /// unit is gone.
    fn rebind(&self, unit: &str) -> Rebind;
    /// The body of the launch record the host of `unit` was launched from, or `None` when the
    /// registry names no such host. It attaches to nothing.
    fn launched_record(&self, unit: &str) -> Result<Option<Value>, HostError>;
    /// The last position an adapter saved for the host of `unit` (a JSON value the adapter owns),
    /// or `None` when it saved none. After a restart, a rebound adapter reads on from it. The
    /// default answers `None`: a host service that keeps no positions.
    fn position(&self, _unit: &str) -> Result<Option<Value>, HostError> {
        Ok(None)
    }
    /// Keep a position for the host of `unit`. An adapter that reads a file its session writes saves
    /// how far it read, and what it took from there, before it hands any of it to the engine. The
    /// default refuses: a host service that keeps no positions cannot keep one.
    fn save_position(&self, _unit: &str, _position: Value) -> Result<(), HostError> {
        Err(HostError(
            "this host service keeps no positions".to_string(),
        ))
    }
}
