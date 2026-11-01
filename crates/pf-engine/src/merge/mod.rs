//! Merge by rule.
//!
//! Given the workflow's rule (green checks, declared order), the engine asks the forge port to
//! merge the next unit's PR, checks that the merge landed, and records the result. A failed
//! check, a conflict or a merge that did not land parks the unit with its reason. Each merge
//! result writes a ledger line with repo, branch, SHA and outcome, from what the forge port
//! reports.
//!
//! Declared order: units rank by the order their group first appeared, then by `order`, then by
//! ID. A unit that is not merged yet holds back every later unit.

use crate::admission::{Command, CommandBody, Decision, EngineError, RunView};
use crate::engine::Engine;
use crate::port::forge::{Checks, ForgeEffect, ForgeOutcome, Mergeable};
use crate::port::store::{AdmissionOutcome, Effect, NodeKind, NodeRow};
use crate::status::{REASON_RUN_HALTED, Status, check_reason, one_line_reason};
use crate::unit::UnitDetail;
use crate::workflow::Workflow;

type Rejected = String;

/// The last push of a unit that succeeded: where it went, and the SHA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRecord {
    pub repo: String,
    pub branch: String,
    pub sha: String,
}

/// Compare a PR head with the last recorded push. A PR head with no recorded push, or with
/// another one, is refused. This catches a push from a script that the rules cannot parse.
///
/// It is a pure function in the engine, because the engine may not call a workflow crate. The
/// forge adapter's merge step calls it when the unit's track sets `head_check`.
pub fn head_check(pr_sha: &str, last_push: Option<&PushRecord>) -> Result<(), String> {
    match last_push {
        Some(push) if !pr_sha.is_empty() && push.sha.eq_ignore_ascii_case(pr_sha) => Ok(()),
        _ => Err(format!("PR head {pr_sha} has no recorded push")),
    }
}

/// A unit that has all its stages done and is ready for the merge rule.
fn merge_ready(
    view: &RunView,
    workflow: &Workflow,
    unit: &NodeRow,
) -> Result<UnitDetail, Rejected> {
    let detail = view.unit_detail(unit)?;
    let track = workflow
        .track(&detail.track)
        .ok_or_else(|| crate::admission::unknown_track(&unit.id, &detail.track))?;
    if track.merge.is_none() {
        return Err(format!("track {:?} has no merge rule", track.name));
    }
    if unit.status != Status::Running {
        return Err(format!(
            "unit {} is {}, not running",
            unit.id,
            unit.status.as_str()
        ));
    }
    if (detail.stages_done as usize) < track.stages.len() {
        return Err(format!("unit {} has stages left", unit.id));
    }
    Ok(detail)
}

pub(crate) fn decide_merge(
    view: &RunView,
    workflow: &Workflow,
    unit_id: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    let unit = view
        .node(unit_id)
        .filter(|n| n.kind == NodeKind::Unit)
        .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
    merge_ready(view, workflow, unit)?;
    let mut d = if view.is_halted() {
        let mut d = Decision::refused(REASON_RUN_HALTED);
        if let Some(u) = crate::launch::with_halt_reason(unit, now) {
            d.effects.push(Effect::PutNode(u));
        }
        d.ledger = Some(format!(
            "merge of unit {unit_id} refused: {REASON_RUN_HALTED}"
        ));
        d
    } else {
        let mut d = Decision::admitted();
        d.ledger = Some(format!("merge of unit {unit_id} admitted"));
        d
    };
    d.unit_id = Some(unit_id.to_string());
    Ok(d)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_merge_result(
    view: &RunView,
    workflow: &Workflow,
    unit_id: &str,
    repo: &str,
    branch: &str,
    sha: &str,
    outcome: &str,
    landed: bool,
    now: &str,
) -> Result<Decision, Rejected> {
    let outcome = check_reason(outcome).map_err(|e| e.to_string())?;
    let unit = view
        .node(unit_id)
        .filter(|n| n.kind == NodeKind::Unit)
        .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
    merge_ready(view, workflow, unit)?;
    let mut u = unit.clone();
    u.updated_at = now.to_string();
    if landed {
        u.status = Status::Done;
        u.reason = Some(if sha.is_empty() {
            "merged".to_string()
        } else {
            format!("merged at {sha}")
        });
    } else {
        u.status = Status::Parked;
        u.reason = Some(outcome.to_string());
    }
    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    d.effects.push(Effect::PutNode(u));
    d.ledger = Some(format!(
        "merge of unit {unit_id}: repo={repo} branch={branch} sha={sha} outcome={outcome}"
    ));
    Ok(d)
}

/// What stands next in the declared order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Every unit is merged, or there is no unit.
    Nothing,
    /// The next unit has all stages done and is ready.
    Ready(String),
    /// The next unit still has stages to run. It holds back the later units.
    NotReady(String),
    /// The next unit is parked, failed or stopped. It holds back the later units.
    Blocked(String),
}

/// Find the first unit in the declared order that has not merged.
pub fn next_in_order(view: &RunView, workflow: &Workflow) -> Next {
    let mut ranked: Vec<(u32, u32, &str, &NodeRow, UnitDetail)> = Vec::new();
    let mut group_rank: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    // Only units on a track with a merge rule take part. A unit on another track never waits on
    // a merge, and never holds one back.
    let mut units: Vec<(&NodeRow, UnitDetail)> = view
        .units()
        .filter_map(|n| UnitDetail::from_text(&n.detail).ok().map(|d| (n, d)))
        .filter(|(_, d)| workflow.track(&d.track).is_some_and(|t| t.merge.is_some()))
        .collect();
    units.sort_by_key(|(_, d)| d.ingest_seq);
    for (_, d) in &units {
        let next = u32::try_from(group_rank.len()).unwrap_or(u32::MAX);
        group_rank.entry(d.record.group.clone()).or_insert(next);
    }
    for (n, d) in units {
        let rank = group_rank[&d.record.group];
        let order = d.record.order;
        ranked.push((rank, order, n.id.as_str(), n, d));
    }
    ranked.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    for (_, _, id, node, detail) in ranked {
        match node.status {
            Status::Done => continue,
            Status::Running => {
                let stages = workflow.track(&detail.track).map_or(0, |t| t.stages.len());
                return if detail.stages_done as usize >= stages {
                    Next::Ready(id.to_string())
                } else {
                    Next::NotReady(id.to_string())
                };
            }
            _ => return Next::Blocked(id.to_string()),
        }
    }
    Next::Nothing
}

/// What `merge_next` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeStep {
    /// The forge merged the PR, the merge landed, and the unit is `done`.
    Merged { unit: String, effect: ForgeEffect },
    /// The unit is parked with the reason. Later units wait.
    Parked { unit: String, reason: String },
    /// The run is halted. The unit keeps its status and gains the reason "run halted".
    Held { unit: String, reason: String },
    /// Nothing can merge yet. `unit` is the one that holds the order.
    Waiting { unit: String, why: String },
    /// No unit is left to merge, or the workflow has no merge rule.
    Idle,
}

impl Engine {
    /// Merge the next unit in the declared order, by the workflow's rule.
    pub fn merge_next(&self) -> Result<MergeStep, EngineError> {
        if !self.cfg.workflow.has_merge() {
            return Ok(MergeStep::Idle);
        }
        let view = self.view()?;
        match next_in_order(&view, &self.cfg.workflow) {
            Next::Nothing => Ok(MergeStep::Idle),
            Next::NotReady(unit) => Ok(MergeStep::Waiting {
                why: format!("unit {unit} has stages left"),
                unit,
            }),
            Next::Blocked(unit) => {
                let node = view.node(&unit);
                let status = node.map_or("unknown", |n| n.status.as_str());
                let reason = node.and_then(|n| n.reason.clone()).unwrap_or_default();
                Ok(MergeStep::Waiting {
                    why: format!("unit {unit} is {status}: {reason}"),
                    unit,
                })
            }
            Next::Ready(unit) => self.merge_unit(&view, &unit),
        }
    }

    fn merge_unit(&self, view: &RunView, unit_id: &str) -> Result<MergeStep, EngineError> {
        let node = view
            .node(unit_id)
            .ok_or_else(|| EngineError::Rejected(format!("unit {unit_id} is unknown")))?;
        let detail = view.unit_detail(node).map_err(EngineError::Rejected)?;
        let Some(pr) = detail.pr else {
            let reason = "no PR is recorded for the unit".to_string();
            self.park(unit_id, &reason)?;
            return Ok(MergeStep::Parked {
                unit: unit_id.to_string(),
                reason,
            });
        };
        let forge_error = |e: crate::port::forge::ForgeError| format!("forge error: {e}");

        let status = match self.forge.pr_status(&pr) {
            Ok(s) => s,
            Err(e) => return self.finish_merge(unit_id, &pr.repo, "", "", &forge_error(e), false),
        };
        let done = |outcome: &str| {
            self.finish_merge(
                unit_id,
                &status.repo,
                &status.branch,
                &status.sha,
                outcome,
                false,
            )
        };
        match (status.checks, status.mergeable) {
            (Checks::Pending, _) => {
                return Ok(MergeStep::Waiting {
                    unit: unit_id.to_string(),
                    why: "checks are pending".to_string(),
                });
            }
            (Checks::Red, _) => return done("checks red"),
            (_, Mergeable::Conflict) => return done("merge conflict"),
            (Checks::Green, Mergeable::Clean) => {}
        }

        let admission = self.submit(Command {
            id: format!("merge:{unit_id}"),
            body: CommandBody::Merge {
                unit_id: unit_id.to_string(),
            },
        })?;
        if admission.row.outcome == AdmissionOutcome::Refused {
            return Ok(MergeStep::Held {
                unit: unit_id.to_string(),
                reason: admission.row.reason.unwrap_or_default(),
            });
        }

        let merged = match self.forge.merge(&pr) {
            Ok(e) => e,
            Err(e) => {
                return self.finish_merge(
                    unit_id,
                    &status.repo,
                    &status.branch,
                    &status.sha,
                    &forge_error(e),
                    false,
                );
            }
        };
        let fail = |outcome: &str| {
            self.finish_merge(
                unit_id,
                &merged.repo,
                &merged.branch,
                &merged.sha,
                outcome,
                false,
            )
        };
        match &merged.outcome {
            ForgeOutcome::Merged => {}
            ForgeOutcome::Conflict => return fail("merge conflict"),
            ForgeOutcome::Refused(why) => return fail(&format!("merge refused: {why}")),
            other => return fail(&format!("merge returned {other}")),
        }

        match self.forge.merge_landed(&pr) {
            Ok(e) if e.outcome == ForgeOutcome::Landed => {
                self.finish_merge(unit_id, &e.repo, &e.branch, &e.sha, "landed", true)
            }
            Ok(e) => self.finish_merge(
                unit_id,
                &e.repo,
                &e.branch,
                &e.sha,
                "merge did not land",
                false,
            ),
            Err(e) => self.finish_merge(
                unit_id,
                &merged.repo,
                &merged.branch,
                &merged.sha,
                &forge_error(e),
                false,
            ),
        }
    }

    /// Record the merge result. It writes the ledger line and ends or parks the unit.
    fn finish_merge(
        &self,
        unit_id: &str,
        repo: &str,
        branch: &str,
        sha: &str,
        outcome: &str,
        landed: bool,
    ) -> Result<MergeStep, EngineError> {
        // The outcome can hold text from the forge. It is one clean line before it is recorded.
        let outcome = one_line_reason(outcome);
        self.submit(Command {
            id: format!("merge-result:{unit_id}"),
            body: CommandBody::MergeResult {
                unit_id: unit_id.to_string(),
                repo: repo.to_string(),
                branch: branch.to_string(),
                sha: sha.to_string(),
                outcome: outcome.clone(),
                landed,
            },
        })?;
        if landed {
            Ok(MergeStep::Merged {
                unit: unit_id.to_string(),
                effect: ForgeEffect {
                    repo: repo.to_string(),
                    branch: branch.to_string(),
                    sha: sha.to_string(),
                    outcome: ForgeOutcome::Landed,
                },
            })
        } else {
            Ok(MergeStep::Parked {
                unit: unit_id.to_string(),
                reason: outcome,
            })
        }
    }
}
