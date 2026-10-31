//! The six node statuses of ADR-0006.
//!
//! Every node has one status. Every end status carries a one-line reason.

use serde::{Deserialize, Serialize};

/// The status of one node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Done,
    Failed,
    Stopped,
    WaitingOnYou,
    Parked,
}

impl Status {
    /// The stored name of the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Stopped => "stopped",
            Status::WaitingOnYou => "waiting_on_you",
            Status::Parked => "parked",
        }
    }

    /// Read a stored name back.
    pub fn parse(text: &str) -> Option<Status> {
        Some(match text {
            "running" => Status::Running,
            "done" => Status::Done,
            "failed" => Status::Failed,
            "stopped" => Status::Stopped,
            "waiting_on_you" => Status::WaitingOnYou,
            "parked" => Status::Parked,
            _ => return None,
        })
    }

    /// An end status needs a reason.
    pub fn is_end(self) -> bool {
        matches!(
            self,
            Status::Done | Status::Failed | Status::Stopped | Status::Parked
        )
    }
}

/// The end statuses a caller may record with the `End` command.
/// `Parked` has its own command, [`crate::admission::CommandBody::Park`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndStatus {
    Done,
    Failed,
    Stopped,
}

impl EndStatus {
    pub fn status(self) -> Status {
        match self {
            EndStatus::Done => Status::Done,
            EndStatus::Failed => Status::Failed,
            EndStatus::Stopped => Status::Stopped,
        }
    }
}

/// Why a reason text is not acceptable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReasonError {
    Empty,
    MultiLine,
}

impl std::fmt::Display for ReasonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReasonError::Empty => write!(f, "a reason must not be empty"),
            ReasonError::MultiLine => write!(f, "a reason must be one line"),
        }
    }
}

impl std::error::Error for ReasonError {}

/// Check that a reason is one non-empty line. Returns the trimmed text.
pub fn check_reason(text: &str) -> Result<&str, ReasonError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ReasonError::Empty);
    }
    if trimmed.contains(['\n', '\r']) {
        return Err(ReasonError::MultiLine);
    }
    Ok(trimmed)
}

/// Reason a unit gets when the run is halted and its next step is a launch or a merge.
pub const REASON_RUN_HALTED: &str = "run halted";

/// Reason a unit gets when its seat was lost across a controller restart.
pub const REASON_SEAT_LOST: &str = "seat lost across a controller restart";
