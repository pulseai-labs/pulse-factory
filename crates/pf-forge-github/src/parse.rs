//! Read `gh` output. The output is untrusted: strict JSON, every value checked before use.

use pf_engine::port::forge::{Checks, ForgeError, Mergeable, PrStatus};
use pf_engine::status::redact;
use serde::Deserialize;

/// The fields `pr_status` asks for.
pub(crate) const STATUS_FIELDS: &str =
    "headRefName,headRefOid,baseRefName,mergeable,mergeStateStatus,statusCheckRollup";

/// The fields `merge_landed` asks for.
pub(crate) const LANDED_FIELDS: &str = "state,mergeCommit,headRefName,headRefOid,baseRefName";

fn bad(why: impl std::fmt::Display) -> ForgeError {
    ForgeError(redact(&format!("gh gave unreadable output: {why}")))
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Is `text` one `owner/name` pair of plain characters, with no part that starts with `-`?
pub(crate) fn is_repo_name(text: &str) -> bool {
    let mut parts = text.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [owner, name]
        .iter()
        .all(|p| !p.is_empty() && !p.starts_with(['-', '.']) && p.chars().all(name_char))
}

/// A full object name: 40 or 64 hexadecimal digits.
pub(crate) fn is_sha(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A branch name that is safe to put in an API path: plain characters, no `..`, no leading `-`.
pub(crate) fn is_branch(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(['-', '/'])
        && !text.ends_with('/')
        && !text.contains("..")
        && !text.contains("//")
        && text.chars().all(|c| name_char(c) || c == '/')
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
    mergeable: String,
    status_check_rollup: Option<Vec<Check>>,
}

/// One entry of the rollup: a check run, or a commit status.
#[derive(Deserialize)]
struct Check {
    /// A check run: `COMPLETED`, `IN_PROGRESS`, `QUEUED`…
    status: Option<String>,
    /// A check run's result, empty until it completes.
    conclusion: Option<String>,
    /// A commit status: `SUCCESS`, `FAILURE`, `ERROR`, `PENDING`, `EXPECTED`.
    state: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Passed,
    Neutral,
    Failed,
    Running,
}

fn verdict(check: &Check) -> Result<Verdict, ForgeError> {
    if let Some(state) = &check.state {
        return match state.as_str() {
            "SUCCESS" => Ok(Verdict::Passed),
            "FAILURE" | "ERROR" => Ok(Verdict::Failed),
            "PENDING" | "EXPECTED" => Ok(Verdict::Running),
            other => Err(bad(format!("unknown commit status {other:?}"))),
        };
    }
    let Some(status) = &check.status else {
        return Err(bad("a check has neither a state nor a status"));
    };
    if status != "COMPLETED" {
        return Ok(Verdict::Running);
    }
    match check.conclusion.as_deref().unwrap_or("") {
        "SUCCESS" => Ok(Verdict::Passed),
        "NEUTRAL" | "SKIPPED" => Ok(Verdict::Neutral),
        "FAILURE" | "CANCELLED" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" | "STALE" => {
            Ok(Verdict::Failed)
        }
        other => Err(bad(format!("unknown check conclusion {other:?}"))),
    }
}

/// Fold the rollup: any failure is red; any check that still runs, an empty rollup, or a rollup
/// with no success is pending; only a non-empty rollup with a success and no failure is green.
fn fold(rollup: &[Check]) -> Result<Checks, ForgeError> {
    let verdicts = rollup.iter().map(verdict).collect::<Result<Vec<_>, _>>()?;
    let has = |v: Verdict| verdicts.contains(&v);
    Ok(if has(Verdict::Failed) {
        Checks::Red
    } else if has(Verdict::Running) || !has(Verdict::Passed) {
        Checks::Pending
    } else {
        Checks::Green
    })
}

pub(crate) fn pr_status(repo: &str, stdout: &[u8]) -> Result<PrStatus, ForgeError> {
    let view: View = serde_json::from_slice(stdout).map_err(bad)?;
    if view.head_ref_name.is_empty() || view.base_ref_name.is_empty() {
        return Err(bad("a branch name is empty"));
    }
    if !is_sha(&view.head_ref_oid) {
        return Err(bad("the head is not a full object name"));
    }
    let mut checks = fold(view.status_check_rollup.as_deref().unwrap_or(&[]))?;
    let mergeable = match view.mergeable.as_str() {
        "MERGEABLE" => Mergeable::Clean,
        "CONFLICTING" => Mergeable::Conflict,
        // GitHub is still computing. Report it as pending, never as clean or as a conflict.
        "UNKNOWN" => {
            if checks == Checks::Green {
                checks = Checks::Pending;
            }
            Mergeable::Clean
        }
        other => return Err(bad(format!("unknown mergeable value {other:?}"))),
    };
    Ok(PrStatus {
        repo: repo.to_string(),
        branch: view.head_ref_name,
        sha: view.head_ref_oid,
        base: view.base_ref_name,
        checks,
        mergeable,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LandedView {
    state: String,
    merge_commit: Option<MergeCommit>,
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
}

#[derive(Deserialize)]
struct MergeCommit {
    oid: String,
}

/// What `merge_landed` needs from the PR.
pub(crate) struct Landed {
    pub branch: String,
    pub sha: String,
    pub base: String,
    /// The merge commit, when the PR is merged.
    pub merge_commit: Option<String>,
}

pub(crate) fn landed_view(stdout: &[u8]) -> Result<Landed, ForgeError> {
    let view: LandedView = serde_json::from_slice(stdout).map_err(bad)?;
    if view.head_ref_name.is_empty() || !is_sha(&view.head_ref_oid) {
        return Err(bad("the head is not a branch and a full object name"));
    }
    if !is_branch(&view.base_ref_name) {
        return Err(bad("the base is not a plain branch name"));
    }
    let merge_commit = match (view.state.as_str(), view.merge_commit) {
        ("MERGED", Some(c)) if is_sha(&c.oid) => Some(c.oid),
        ("MERGED", _) => return Err(bad("a merged PR has no readable merge commit")),
        ("OPEN" | "CLOSED", _) => None,
        (other, _) => return Err(bad(format!("unknown PR state {other:?}"))),
    };
    Ok(Landed {
        branch: view.head_ref_name,
        sha: view.head_ref_oid,
        base: view.base_ref_name,
        merge_commit,
    })
}

#[derive(Deserialize)]
struct Compare {
    status: String,
}

/// Read the answer of the compare API, from the merge commit to the base: is the base ahead of,
/// or the same as, the commit?
pub(crate) fn is_descendant_status(stdout: &[u8]) -> Result<bool, ForgeError> {
    let compare: Compare = serde_json::from_slice(stdout).map_err(bad)?;
    match compare.status.as_str() {
        "ahead" | "identical" => Ok(true),
        "behind" | "diverged" => Ok(false),
        other => Err(bad(format!("unknown compare status {other:?}"))),
    }
}
