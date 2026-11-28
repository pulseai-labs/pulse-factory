//! The forge port: read a PR's checks and mergeability, merge a PR, read whether a merge landed, run a
//! PR's failed checks again and read the files of a commit.
//!
//! Each effect returns its repo, branch, SHA and outcome.
//!
//! A forge is built for one repo. `merge` takes the head SHA the engine read, and the forge
//! merges that SHA or nothing. A PR for another repo is never merged: `merge` answers it with
//! [`ForgeOutcome::Refused`] and the reads answer it with a [`ForgeError`].

use std::collections::BTreeMap;

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
    /// The PR's head branch.
    pub branch: String,
    /// The PR's head SHA.
    pub sha: String,
    /// The branch the PR merges into.
    pub base: String,
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
    /// The forge refused the merge for another reason. A head that differs from the expected
    /// SHA, and a PR of another repo, are refusals.
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

/// One failed check of a PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedCheck {
    pub name: String,
    /// Why this forge cannot run the check again. `None` for a check it can run again.
    pub blocked: Option<String>,
}

/// The failed checks of a PR, with the head they ran on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedChecks {
    /// The PR's head SHA when the checks were read.
    pub head_sha: String,
    pub failed: Vec<FailedCheck>,
}

/// What a call to run the failed checks of a PR again did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RerunEffect {
    pub repo: String,
    pub number: u64,
    /// The head the call ran on, or the head it found when that was not the expected one.
    pub sha: String,
    /// The checks that were run again.
    pub ran: Vec<String>,
    /// The checks that were not run again, each with the reason.
    pub not_ran: Vec<(String, String)>,
    /// The head the call found when it was not the expected SHA. Nothing was run again.
    pub head_moved: Option<String>,
}

fn cannot(what: &str) -> ForgeError {
    ForgeError(format!("this forge cannot {what}"))
}

pub trait ForgePort: Send + Sync {
    /// Read a PR's checks and mergeability.
    fn pr_status(&self, pr: &PrRef) -> Result<PrStatus, ForgeError>;
    /// Merge a PR, but only when its head is `expected_sha`. A head that differs gives
    /// [`ForgeOutcome::Refused`], not an error. The effect may leave `branch` empty when the
    /// call did not read the PR; the engine then records the branch it read in `pr_status`.
    fn merge(&self, pr: &PrRef, expected_sha: &str) -> Result<ForgeEffect, ForgeError>;
    /// Read whether the merge landed.
    fn merge_landed(&self, pr: &PrRef) -> Result<ForgeEffect, ForgeError>;
    /// Read the failed checks of a PR, and whether this forge can run each one again.
    fn failed_checks(&self, _pr: &PrRef) -> Result<FailedChecks, ForgeError> {
        Err(cannot("read the failed checks of a PR"))
    }
    /// Run the failed checks of a PR again, but only when its head is `expected_sha`. A head that
    /// differs runs nothing and is named in [`RerunEffect::head_moved`].
    fn rerun_failed_checks(
        &self,
        _pr: &PrRef,
        _expected_sha: &str,
    ) -> Result<RerunEffect, ForgeError> {
        Err(cannot("run the failed checks of a PR again"))
    }
    /// Every file path of a commit with its blob ID.
    fn tree(&self, _repo: &str, _sha: &str) -> Result<BTreeMap<String, String>, ForgeError> {
        Err(cannot("read the files of a commit"))
    }
    /// The tip of a branch.
    fn branch_tip(&self, _repo: &str, _branch: &str) -> Result<String, ForgeError> {
        Err(cannot("read the tip of a branch"))
    }
}
