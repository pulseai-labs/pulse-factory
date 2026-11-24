//! The line class of a request (ADR-0006, amended; `docs/contracts/requests.md`, "The line").
//!
//! A request is `below` or `above` the line. `below` means that the operator's clear grant may
//! settle it, and `above` means that the claim alone may. The engine computes the class from the
//! request's cause and from its track's merge, and never stores it: no request body, command or
//! block holds a class key, so a reader that wants the class asks [`Engine::request_line`].
//!
//! The class fails closed. Every read that fails gives `above`, and the engine names no repo and
//! no branch of its own: the caller passes the forge's repo and its default branch.

use serde_json::Value;

use crate::admission::RunView;
use crate::claim::{DECISION_VERSION, KEY_DECISION};
use crate::engine::Engine;
use crate::port::store::{Cause, RequestRow};
use crate::unit::UnitDetail;
use crate::workflow::MergeRule;

/// Which side of the line a request is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Line {
    /// The clear grant may settle it, or the claim.
    Below,
    /// The claim alone settles it.
    Above,
}

impl Line {
    pub fn as_str(self) -> &'static str {
        match self {
            Line::Below => "below",
            Line::Above => "above",
        }
    }
}

/// The class of a cause. One exhaustive `match`, so a new cause does not compile until it has a
/// row here.
///
/// `free_merge` says that the unit's track merges without the claim's ack and into a branch that
/// is not the forge's default branch: see [`Engine::request_line`]. It matters to two causes only.
/// A request with no cause is `above`.
pub fn line_of(cause: Option<Cause>, free_merge: bool) -> Line {
    let Some(cause) = cause else {
        return Line::Above;
    };
    match cause {
        Cause::Stalled | Cause::SeatLost | Cause::HostUnreachable => Line::Below,
        Cause::MergeChecksRed | Cause::MergeConflict => {
            if free_merge {
                Line::Below
            } else {
                Line::Above
            }
        }
        // A failed launch is below the line only once an adapter marks its cause as transient, and
        // none does today.
        Cause::LaunchFailed
        | Cause::StopFile
        | Cause::ParkFile
        | Cause::NoDoneSignal
        | Cause::LaunchRefused
        | Cause::OutcomeCap
        | Cause::HookFailed
        | Cause::AnswerHookFailed
        | Cause::PinsChanged
        | Cause::BasePushFailed
        | Cause::BaseMoved
        | Cause::RenderFailed
        | Cause::DecisionsTooLarge
        | Cause::NoPr
        | Cause::MergeRefused
        | Cause::MergeError
        | Cause::MergeWaitsAck => Line::Above,
    }
}

/// A branch name without one leading `refs/heads/`. Nothing else is folded.
fn branch_name(name: &str) -> &str {
    name.strip_prefix("refs/heads/").unwrap_or(name)
}

/// The cause a request's stored block names. A body that is not JSON, a block of another version
/// and a cause that this binary does not know give none.
fn stored_cause(request: &RequestRow) -> Option<Cause> {
    let body: Value = serde_json::from_str(&request.body).ok()?;
    let block = body.get(KEY_DECISION)?;
    if block.get("v").and_then(Value::as_u64) != Some(DECISION_VERSION) {
        return None;
    }
    Cause::parse(block.get("cause").and_then(Value::as_str)?)
}

impl Engine {
    /// The class of one request of this run.
    ///
    /// It reads the cause from the request's stored block (version 1 only), the unit's track from
    /// its detail in `view`, and the track's merge from the engine's own workflow. That workflow is
    /// the one the run's own copy of the package gave, so a change to the source package after the
    /// run began changes the class of no request. The merge's repo and base are rendered by
    /// [`Engine::declared`], the merge step's own function, from the engine's own parameters.
    ///
    /// `forge` is the forge's repo and its default branch, as the caller holds them. A track has a
    /// free merge only when all of these hold: it has a merge; its rule is not `ClaimAck`; `forge`
    /// is given; its `repo`, rendered for the unit, is the forge's repo; and its `base`, rendered
    /// for the unit, is another branch than the default branch. A unit whose merge renders another
    /// repo has no known default branch, so its merge causes are `above`.
    ///
    /// Every read that fails gives `above`.
    pub fn request_line(
        &self,
        view: &RunView,
        request: &RequestRow,
        forge: Option<(&str, &str)>,
    ) -> Line {
        let Some(unit_id) = request.unit_id.as_deref() else {
            return Line::Above;
        };
        // A unit that the run does not hold, or whose detail does not read, fails closed for every
        // cause.
        let Some(node) = view.node(unit_id) else {
            return Line::Above;
        };
        let Ok(detail) = view.unit_detail(node) else {
            return Line::Above;
        };
        let cause = stored_cause(request);
        // Only a red check and a conflict ask the merge anything.
        let free_merge = matches!(cause, Some(Cause::MergeChecksRed | Cause::MergeConflict))
            && self.has_free_merge(unit_id, &detail, forge);
        line_of(cause, free_merge)
    }

    fn has_free_merge(
        &self,
        unit_id: &str,
        detail: &UnitDetail,
        forge: Option<(&str, &str)>,
    ) -> bool {
        let Some((forge_repo, default_branch)) = forge else {
            return false;
        };
        let default_branch = branch_name(default_branch);
        if forge_repo.is_empty() || default_branch.is_empty() {
            return false;
        }
        let Some(def) = self
            .cfg
            .workflow
            .track(&detail.track)
            .and_then(|t| t.merge.as_ref())
        else {
            return false;
        };
        if def.rule == MergeRule::ClaimAck {
            return false;
        }
        let Ok(declared) = self.declared(unit_id, detail, def) else {
            return false;
        };
        let base = branch_name(&declared.base);
        !declared.repo.is_empty()
            && declared.repo == forge_repo
            && !base.is_empty()
            && base != default_branch
    }
}
