//! Units, stages, sessions and signals.
//!
//! A **unit** is one record of work. It moves through the workflow's **stages** in order.
//! A **session** runs one stage. A **signal** ends a stage: a done-file, a PR state, a session's
//! turn end or a command's exit code. The engine records each move as an admitted command.

use serde::{Deserialize, Serialize};

use crate::port::forge::PrRef;
use crate::port::unit_source::UnitRecord;

/// What ends a stage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Signal {
    /// A done-file appeared.
    DoneFile { path: String },
    /// A PR reached a state, such as `open` or `merged`.
    PrState { state: String },
    /// The session's turn ended.
    TurnEnd,
    /// A command exited. Code 0 ends the stage `done`. Any other code ends it `failed`.
    ExitCode { code: i32 },
}

impl Signal {
    /// A short text for ledger lines and stage reasons.
    pub fn describe(&self) -> String {
        match self {
            Signal::DoneFile { path } => format!("done-file {path}"),
            Signal::PrState { state } => format!("PR state {state}"),
            Signal::TurnEnd => "turn end".to_string(),
            Signal::ExitCode { code } => format!("exit code {code}"),
        }
    }

    /// Does this signal end the stage as a success?
    pub fn is_success(&self) -> bool {
        !matches!(self, Signal::ExitCode { code } if *code != 0)
    }
}

/// What the engine keeps in a unit node's `detail` text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitDetail {
    pub record: UnitRecord,
    /// The track whose stages the unit runs. It comes from the source or seed that admitted it.
    #[serde(default)]
    pub track: String,
    /// Where this unit came in, counting from 1. Merge order uses it to rank groups.
    pub ingest_seq: u32,
    /// How many stages have ended `done`.
    pub stages_done: u32,
    /// The unit's PR, once a signal has named it.
    pub pr: Option<PrRef>,
}

impl UnitDetail {
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).expect("a unit detail always serializes")
    }

    pub fn from_text(text: &str) -> Result<UnitDetail, String> {
        serde_json::from_str(text).map_err(|e| format!("unit detail is not readable: {e}"))
    }
}

/// The node ID of one stage attempt. It is also the ID of the attempt's session.
pub fn stage_node_id(unit_id: &str, stage: &str, attempt: u32) -> String {
    format!("{unit_id}/{stage}#{attempt}")
}
