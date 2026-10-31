//! The forge port: read a PR's checks and mergeability, merge a PR, read whether a merge landed.
//!
//! Each effect returns its repo, branch, SHA and outcome.

use serde::{Deserialize, Serialize};

/// A pull request on a forge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRef {
    pub repo: String,
    pub number: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    Green,
    Red,
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mergeable {
    Clean,
    Conflict,
}

/// What a PR looks like now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrStatus {
    pub repo: String,
    pub branch: String,
    pub sha: String,
    pub checks: Checks,
    pub mergeable: Mergeable,
}

/// How a forge effect ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeOutcome {
    /// The forge merged the PR.
    Merged,
    /// The forge refused the merge because of a conflict.
    Conflict,
    /// The forge refused the merge for another reason.
    Refused(String),
    /// A read found the merge on the target branch.
    Landed,
    /// A read did not find the merge on the target branch.
    NotLanded,
}

impl std::fmt::Display for ForgeOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeOutcome::Merged => write!(f, "merged"),
            ForgeOutcome::Conflict => write!(f, "conflict"),
            ForgeOutcome::Refused(why) => write!(f, "refused: {why}"),
            ForgeOutcome::Landed => write!(f, "landed"),
            ForgeOutcome::NotLanded => write!(f, "not landed"),
        }
    }
}

/// The result of one forge effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeEffect {
    pub repo: String,
    pub branch: String,
    pub sha: String,
    pub outcome: ForgeOutcome,
}

/// Why a forge call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeError(pub String);

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "forge call failed: {}", self.0)
    }
}

impl std::error::Error for ForgeError {}

pub trait ForgePort: Send + Sync {
    /// Read a PR's checks and mergeability.
    fn pr_status(&self, pr: &PrRef) -> Result<PrStatus, ForgeError>;
    /// Merge a PR.
    fn merge(&self, pr: &PrRef) -> Result<ForgeEffect, ForgeError>;
    /// Read whether the merge landed.
    fn merge_landed(&self, pr: &PrRef) -> Result<ForgeEffect, ForgeError>;
}
