//! The acts of the clear grant and the engine's settle decision (ADR-0005, amended;
//! `docs/contracts/requests.md`, "Settle").
//!
//! A settle names a request at the revision the caller read, one act and a reason. The controller
//! checks who calls and what the grant allows, then hands the engine a [`SettleAsk`]. The engine
//! decides under its own lock, so what it reads is what the commit closes:
//!
//! 1. a degraded engine refuses;
//! 2. the replay first: a settle that the store holds under the command ID is answered by the
//!    stored record (or is a conflict), though its request is closed now;
//! 3. the request must be open at the named revision;
//! 4. the request must be `below` the line;
//! 5. the cause must admit the act ([`acts_of`]);
//! 6. the line record is made, and one `submit` follows. [`decide_settle`] checks the revision, the
//!    halt, `--to`, the launch bound and the grant's precondition again, and one commit closes the
//!    request, resumes the unit and carries [`Effect::RequireLiveGrant`].
//!
//! A settle grants no new budget: it resets no outcome counter, and a launching act that would pass
//! the run's armed launch count is refused.
//!
//! **An act with an effect outside the store commits first and runs its effect after.** `rerun_check`
//! asks the forge to run a PR's failed checks again, and `resume` on `host_unreachable` attaches to
//! the recorded host again. Each such act commits as a settle, then [`Engine::settle_effect`] runs the
//! effect and stores what it did as one `settle-result` command (`settle-result:<R-n>:<revision>`).
//! Until that result is stored, the settle is *pending*: no launch and no merge of its unit is
//! admitted. The start-up reconcile closes a pending settle with the result `unknown` and parks the
//! unit again, so an effect never runs twice.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::line::{Line, stored_cause};
use super::{STAGE_MERGE, park_effects, resume_effects};
use crate::admission::{
    Admission, Command, CommandBody, Decision, EngineError, RunView, body_of_content,
};
use crate::engine::Engine;
use crate::port::agent::{NO_SESSION_HOST, RebindOutcome, SessionRef};
use crate::port::store::{
    AdmissionOutcome, Cause, Effect, NodeKind, NodeRow, RequestKind, RequestRow, RequestStatus,
    SessionRow,
};
use crate::status::{Status, check_reason, one_line_reason};
use crate::unit::stage_node_id;
use crate::workflow::Workflow;

type Rejected = String;

/// The verb of a settled request's stored answer.
pub const VERB_SETTLE: &str = "settle";

/// One act of the clear grant, in the order of ADR-0005.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Act {
    /// Resume a stalled unit at the stage it parked in.
    Clear,
    /// Resume a unit whose seat is lost.
    Resume,
    /// Read the PR's checks again.
    RerunCheck,
    /// Resume a unit at an earlier stage of its track.
    Redispatch,
    /// Run the conflict stage.
    ResolveConflict,
    /// Close a stale request. No request is stale: a revision replaces its request in the same row.
    CloseStale,
}

impl Act {
    /// Every act, in ADR-0005's order.
    pub const ALL: [Act; 6] = [
        Act::Clear,
        Act::Resume,
        Act::RerunCheck,
        Act::Redispatch,
        Act::ResolveConflict,
        Act::CloseStale,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Act::Clear => "clear",
            Act::Resume => "resume",
            Act::RerunCheck => "rerun_check",
            Act::Redispatch => "redispatch",
            Act::ResolveConflict => "resolve_conflict",
            Act::CloseStale => "close_stale",
        }
    }

    pub fn parse(text: &str) -> Option<Act> {
        Act::ALL.into_iter().find(|a| a.as_str() == text)
    }

    /// True for the acts that start a unit's seat again. Each one counts against the run's armed
    /// launch count.
    pub fn launches(self) -> bool {
        matches!(
            self,
            Act::Clear | Act::Resume | Act::Redispatch | Act::ResolveConflict
        )
    }
}

/// The effect outside the store that an act has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectKind {
    /// The forge runs the PR's failed checks again.
    Rerun,
    /// The host service attaches to the recorded host again.
    Reattach,
}

/// The effect of an act on a request with this cause, when it has one. `resolve_conflict` has none
/// of its own: its effect is the normal launch of the conflict stage.
pub fn effect_of(act: Act, cause: Option<Cause>) -> Option<EffectKind> {
    match (act, cause) {
        (Act::RerunCheck, _) => Some(EffectKind::Rerun),
        (Act::Resume, Some(Cause::HostUnreachable)) => Some(EffectKind::Reattach),
        _ => None,
    }
}

/// What an effect did. Each effect admits only the outcomes that fit it: [`SettleOutcome::fits`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettleOutcome {
    /// The forge ran every failed check again.
    Ran,
    /// The host answered: the unit's stage runs on in it.
    Attached,
    /// The host is gone: the stage launches again.
    Gone,
    /// The effect failed: the unit parks again.
    Failed,
    /// The effect's result was never stored, because the controller stopped: the unit parks again and
    /// the effect is not run again.
    Unknown,
}

impl SettleOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            SettleOutcome::Ran => "ran",
            SettleOutcome::Attached => "attached",
            SettleOutcome::Gone => "gone",
            SettleOutcome::Failed => "failed",
            SettleOutcome::Unknown => "unknown",
        }
    }

    /// Does this outcome fit the effect?
    pub fn fits(self, effect: EffectKind) -> bool {
        matches!(
            (self, effect),
            (SettleOutcome::Unknown | SettleOutcome::Failed, _)
                | (SettleOutcome::Ran, EffectKind::Rerun)
                | (
                    SettleOutcome::Attached | SettleOutcome::Gone,
                    EffectKind::Reattach
                )
        )
    }
}

/// The most characters of a result's detail line.
pub const RESULT_DETAIL_MAX: usize = 300;

/// The command ID of a settle's result.
pub fn result_command_id(request_id: &str, revision: u32) -> String {
    format!("settle-result:{request_id}:{revision}")
}

/// A settle whose effect has not stored its result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSettle {
    pub request_id: String,
    pub revision: u32,
    pub act: Act,
    pub effect: EffectKind,
}

impl PendingSettle {
    /// `<R-n>@<revision>`, as the ledger and the park reasons name the settle.
    pub fn name(&self) -> String {
        format!("{}@{}", self.request_id, self.revision)
    }
}

/// The unit's pending settle: its newest admitted `settle` whose act has an effect, when no admitted
/// `settle-result` names the same request and revision. The mark is not a field of any row: it is
/// read from the admissions.
pub fn pending_settle(view: &RunView, unit_id: &str) -> Option<PendingSettle> {
    let (request_id, revision, act, effect) = view
        .admissions
        .iter()
        .rev()
        .filter(|a| {
            a.kind == "settle"
                && a.outcome == AdmissionOutcome::Admitted
                && a.unit_id.as_deref() == Some(unit_id)
        })
        .find_map(|a| {
            let Some(CommandBody::Settle {
                request_id,
                revision,
                act,
                ..
            }) = body_of_content(&a.content)
            else {
                return None;
            };
            let cause = view.request(&request_id).and_then(stored_cause);
            effect_of(act, cause).map(|effect| (request_id, revision, act, effect))
        })?;
    let result_id = result_command_id(&request_id, revision);
    let stored = view
        .admissions
        .iter()
        .any(|a| a.kind == "settle_result" && a.command_id == result_id);
    (!stored).then_some(PendingSettle {
        request_id,
        revision,
        act,
        effect,
    })
}

/// The stored result of a settle, when one was admitted.
pub fn stored_result(view: &RunView, request_id: &str, revision: u32) -> Option<SettleOutcome> {
    let result_id = result_command_id(request_id, revision);
    let row = view
        .admissions
        .iter()
        .find(|a| a.kind == "settle_result" && a.command_id == result_id)?;
    match body_of_content(&row.content)? {
        CommandBody::SettleResult { outcome, .. } => Some(outcome),
        _ => None,
    }
}

/// The acts a cause admits (ADR-0005). One exhaustive `match`, so a new cause does not compile until
/// it has a row here. A cause that no act settles gives none, and so does a request with no cause.
pub fn acts_of(cause: Cause) -> &'static [Act] {
    match cause {
        Cause::Stalled => &[Act::Clear, Act::Redispatch],
        Cause::SeatLost | Cause::HostUnreachable => &[Act::Resume],
        Cause::MergeChecksRed => &[Act::RerunCheck, Act::Redispatch],
        Cause::MergeConflict => &[Act::ResolveConflict],
        Cause::StopFile
        | Cause::ParkFile
        | Cause::NoDoneSignal
        | Cause::LaunchRefused
        | Cause::LaunchFailed
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
        | Cause::ResolutionChangedGuarded
        | Cause::MergeWaitsAck => &[],
    }
}

/// What a settle records beyond its content: who settled, under which grant, and why. None of it is
/// part of the command's canonical content, so a retry with another reason, from another session or
/// at another generation is a replay. A body read back from stored content has empty facts, and
/// nothing reads them from there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettleFacts {
    /// The reason, already redacted and bounded by the controller.
    pub reason: String,
    /// The chat session that settled, `S-<n>`.
    pub session: String,
    /// The project whose grant the settle acts under, `P-<n>`.
    pub project: String,
    /// The generation of that grant.
    pub generation: u64,
    /// The agent that holds the grant.
    pub agent: String,
    /// The line record, made by the engine: [`LineRecord::to_json`].
    pub line: Value,
    /// The PR's head SHA, read by the engine before the commit for `rerun_check` and
    /// `resolve_conflict`. What the controller passes here is dropped.
    pub head_sha: Option<String>,
    /// `authority.rerun_checks_per_head` for the project and the Orchestrator, read by the
    /// controller. Only `rerun_check` reads it.
    pub rerun_limit: Option<u32>,
}

/// What the controller asks of [`Engine::settle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettleAsk {
    pub request_id: String,
    /// The revision the caller read.
    pub revision: u32,
    pub act: Act,
    /// The stage a `redispatch` resumes at. No other act takes it.
    pub to_stage: Option<String>,
    /// The engine fills `facts.line`: what the caller passes there is dropped.
    pub facts: SettleFacts,
}

/// Why a settle did not commit. Nothing was written, except by the commit that [`Admission`] names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettleError {
    /// The request is not open any more, or it is at another revision. The text is the
    /// confirm path's.
    Moved(String),
    /// The settle is refused, with the cause.
    Refused(String),
    /// The settle's ID was admitted before with another act or another `--to`.
    Conflict,
    /// The engine failed.
    Engine(EngineError),
}

impl std::fmt::Display for SettleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettleError::Moved(why) | SettleError::Refused(why) => write!(f, "{why}"),
            SettleError::Conflict => write!(f, "the settle was admitted before with another act"),
            SettleError::Engine(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SettleError {}

fn settle_error(e: EngineError) -> SettleError {
    match e {
        EngineError::Rejected(why) => SettleError::Refused(why),
        EngineError::Conflict { .. } => SettleError::Conflict,
        other => SettleError::Engine(other),
    }
}

/// The command ID of a settle.
pub fn command_id(request_id: &str, revision: u32) -> String {
    format!("settle:{request_id}:{revision}")
}

/// The refusal of a halted run, for the decision and for the reads that come before it.
fn halted_text(run_id: &str) -> String {
    format!("run {run_id} is halted: a halted run refuses every settle")
}

fn closed_text(id: &str) -> String {
    format!("request {id} is not open any more")
}

fn revision_text(id: &str, held: u32, named: u32) -> String {
    format!("request {id} is at revision {held}, not {named}")
}

/// The request is open at `revision`. The two refusals are the confirm path's texts.
fn open_for_settle<'a>(
    view: &'a RunView,
    id: &str,
    revision: u32,
) -> Result<&'a RequestRow, Rejected> {
    let request = view
        .request(id)
        .ok_or_else(|| format!("request {id} is unknown in this run"))?;
    if request.status != RequestStatus::Open {
        return Err(closed_text(id));
    }
    if request.revision != revision {
        return Err(revision_text(id, request.revision, revision));
    }
    Ok(request)
}

fn acts_text(acts: &[Act]) -> String {
    if acts.is_empty() {
        "none".to_string()
    } else {
        acts.iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Refuse an act the request's cause does not admit.
fn check_cause_admits(cause: Option<Cause>, act: Act) -> Result<(), Rejected> {
    let admits = cause.map_or(&[][..], acts_of);
    if admits.contains(&act) {
        return Ok(());
    }
    Err(format!(
        "the cause {} admits {}, not {}",
        cause.map_or("none", Cause::as_str),
        acts_text(admits),
        act.as_str()
    ))
}

/// The decision of a settle (step 4 of the contract), a pure function of the stored state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_settle(
    view: &RunView,
    workflow: &Workflow,
    request_id: &str,
    revision: u32,
    act: Act,
    to_stage: Option<&str>,
    facts: &SettleFacts,
    now: &str,
) -> Result<Decision, Rejected> {
    let request = open_for_settle(view, request_id, revision)?;
    let unit_id = request
        .unit_id
        .as_deref()
        .ok_or_else(|| format!("request {request_id} names no unit"))?;
    let unit = view
        .node(unit_id)
        .filter(|n| n.kind == NodeKind::Unit)
        .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
    if unit.status != Status::Parked {
        return Err(format!(
            "unit {unit_id} is {}, not parked",
            unit.status.as_str()
        ));
    }
    match view.open_request_of(unit_id) {
        Some(open) if open.id == request.id => {}
        Some(open) => {
            return Err(format!(
                "unit {unit_id} has request {} open, not {request_id}",
                open.id
            ));
        }
        None => return Err(format!("unit {unit_id} has no open request")),
    }
    if request.kind == RequestKind::MergeAck {
        return Err(format!(
            "request {request_id} is a merge_ack request: only the claim may ack it"
        ));
    }
    if facts.project.is_empty()
        || facts.session.is_empty()
        || facts.agent.is_empty()
        || facts.generation == 0
        || facts.reason.trim().is_empty()
    {
        return Err(
            "the settle names no grant, session or reason: it cannot be recorded".to_string(),
        );
    }
    let run = view
        .run
        .as_ref()
        .ok_or_else(|| "the run is not open".to_string())?;
    // The kill switch: every act, launching or not.
    if view.is_halted() {
        return Err(halted_text(&run.id));
    }
    check_cause_admits(stored_cause(request), act)?;

    // `--to`: only a redispatch takes it, and a redispatch needs it.
    match (act, to_stage) {
        (Act::Redispatch, None) => {
            return Err(
                "redispatch needs --to <stage>: a redispatch with no --to would be a clear"
                    .to_string(),
            );
        }
        (other, Some(_)) if other != Act::Redispatch => {
            return Err(format!(
                "--to belongs to redispatch only: {} resumes at the stage the unit parked in",
                other.as_str()
            ));
        }
        _ => {}
    }
    if let Some(to) = to_stage {
        let detail = view.unit_detail(unit)?;
        let track = workflow
            .track(&detail.track)
            .ok_or_else(|| crate::admission::unknown_track(unit_id, &detail.track))?;
        let names: Vec<&str> = track.stages.iter().map(|s| s.name.as_str()).collect();
        let to_index = track.stage_index(to).ok_or_else(|| {
            format!(
                "{to:?} is not a stage of track {} (its stages: {})",
                track.name,
                names.join(", ")
            )
        })?;
        // The unit's place when it parked: a stage of the track, or past every stage at its merge.
        let parked_index = if request.stage == STAGE_MERGE {
            track.stages.len()
        } else {
            track.stage_index(&request.stage).ok_or_else(|| {
                format!(
                    "the stage the unit parked in ({:?}) is not a stage of track {} (its stages: {})",
                    request.stage,
                    track.name,
                    names.join(", ")
                )
            })?
        };
        if to_index > parked_index {
            return Err(format!(
                "--to {to} is after the stage the unit parked in ({}): a settle never skips a stage",
                request.stage
            ));
        }
    }

    // The new arms: a re-run is counted for the head, and a conflict needs the track's stage.
    let mut conflict_stage: Option<String> = None;
    let head = facts.head_sha.as_deref().filter(|h| !h.trim().is_empty());
    match act {
        Act::RerunCheck => {
            let (Some(head), Some(limit)) = (head, facts.rerun_limit) else {
                return Err(
                    "the settle names no head or no re-run limit: it cannot be recorded"
                        .to_string(),
                );
            };
            let used = reruns_of(view, unit_id, head);
            if used >= u64::from(limit) {
                return Err(format!(
                    "the act rerun_check was admitted {used} times for head {head} of this unit's PR, and authority.rerun_checks_per_head is {limit}; redispatch, or the operator, remains"
                ));
            }
        }
        Act::ResolveConflict => {
            let detail = view.unit_detail(unit)?;
            let track = workflow
                .track(&detail.track)
                .ok_or_else(|| crate::admission::unknown_track(unit_id, &detail.track))?;
            let on = track.on_conflict().ok_or_else(|| {
                format!(
                    "track {} declares no on_conflict stage, so resolve_conflict has no stage to send the unit to",
                    track.name
                )
            })?;
            if head.is_none() {
                return Err("the settle names no head: it cannot be recorded".to_string());
            }
            conflict_stage = Some(on.stage.clone());
        }
        _ => {}
    }

    // The launch bound: a launching act never passes the run's armed launch count.
    if act.launches() {
        let bound = run.launch_bound.ok_or_else(|| {
            format!(
                "run {} holds no armed launch count, so the grant cannot launch in it",
                run.id
            )
        })?;
        let count = crate::launch::launches_so_far(view);
        let so_far = count.so_far();
        if u64::try_from(so_far).unwrap_or(u64::MAX) >= bound {
            return Err(format!(
                "the run's launches so far ({so_far}, {} admitted and {} resumed and waiting to launch) have reached its armed launch count ({bound}); the grant cannot launch more in this run; the operator can still clear",
                count.admitted, count.pending
            ));
        }
    }

    // One commit closes the request, resumes the unit and carries the grant's precondition. The
    // answer is a settle's: it holds no claim generation, and no outcome counter is reset.
    let (mut effects, _resolved) = resume_effects(
        view,
        workflow,
        unit,
        request,
        conflict_stage.as_deref().or(to_stage),
        None,
        false,
        |_| {
            let mut stored = json!({
                "verb": VERB_SETTLE,
                "act": act.as_str(),
                "reason": facts.reason,
                "grant": {
                    "project": facts.project,
                    "generation": facts.generation,
                    "agent": facts.agent,
                    "session": facts.session,
                },
                "line": facts.line,
            });
            if let Some(to) = to_stage {
                stored["to_stage"] = json!(to);
            }
            if matches!(act, Act::RerunCheck | Act::ResolveConflict)
                && let Some(head) = head
            {
                stored["head_sha"] = json!(head);
            }
            stored
        },
        now,
    )?;
    effects.push(Effect::RequireLiveGrant {
        project_id: facts.project.clone(),
        generation: facts.generation,
    });
    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    d.effects = effects;
    let to_tail = to_stage.map(|to| format!(" --to {to}")).unwrap_or_default();
    let head_tail = match (act, head) {
        (Act::RerunCheck | Act::ResolveConflict, Some(head)) => format!(" head={head}"),
        _ => String::new(),
    };
    d.ledger = Some(format!(
        "settle {request_id}@{revision} by grant {}#{} session {}: act={}{head_tail}{to_tail} reason={}",
        facts.project,
        facts.generation,
        facts.session,
        act.as_str(),
        facts.reason
    ));
    Ok(d)
}

/// How many of the unit's requests were answered by a `rerun_check` settle on this head. The count
/// reads the stored answers, so it holds across a restart.
fn reruns_of(view: &RunView, unit_id: &str, head: &str) -> u64 {
    view.requests
        .iter()
        .filter(|r| r.unit_id.as_deref() == Some(unit_id) && r.status == RequestStatus::Answered)
        .filter_map(|r| serde_json::from_str::<Value>(r.answer.as_deref()?).ok())
        .filter(|a| {
            a.get("verb").and_then(Value::as_str) == Some(VERB_SETTLE)
                && a.get("act").and_then(Value::as_str) == Some(Act::RerunCheck.as_str())
                && a.get("head_sha")
                    .and_then(Value::as_str)
                    .is_some_and(|h| h.eq_ignore_ascii_case(head))
        })
        .count() as u64
}

/// The decision of a `settle-result`: what the effect did. The settle must be admitted, the outcome
/// must fit its effect, and the unit is moved on by the outcome (see the module's docs).
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_settle_result(
    view: &RunView,
    workflow: &Workflow,
    request_id: &str,
    revision: u32,
    outcome: SettleOutcome,
    detail: &str,
    process_ref: Option<&str>,
    now: &str,
) -> Result<Decision, Rejected> {
    let name = format!("{request_id}@{revision}");
    let settle_id = command_id(request_id, revision);
    let settle = view
        .admissions
        .iter()
        .find(|a| a.kind == "settle" && a.command_id == settle_id)
        .filter(|a| a.outcome == AdmissionOutcome::Admitted)
        .ok_or_else(|| format!("settle {name} was not admitted"))?;
    let Some(CommandBody::Settle { act, .. }) = body_of_content(&settle.content) else {
        return Err(format!("settle {name} is not readable"));
    };
    let request = view
        .request(request_id)
        .ok_or_else(|| format!("request {request_id} is unknown in this run"))?;
    let effect = effect_of(act, stored_cause(request))
        .ok_or_else(|| format!("settle {name} runs no effect, so it has no result"))?;
    if !outcome.fits(effect) {
        return Err(format!(
            "the result {} does not fit the effect of act {} in settle {name}",
            outcome.as_str(),
            act.as_str()
        ));
    }
    if detail.chars().count() > RESULT_DETAIL_MAX || detail.contains(['\n', '\r']) {
        return Err(format!(
            "the detail of a result is one line of at most {RESULT_DETAIL_MAX} characters"
        ));
    }
    let unit_id = request
        .unit_id
        .as_deref()
        .ok_or_else(|| format!("request {request_id} names no unit"))?;
    let unit = view
        .node(unit_id)
        .filter(|n| n.kind == NodeKind::Unit)
        .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    let act_name = act.as_str();
    let stage = request.stage.as_str();
    match outcome {
        SettleOutcome::Ran => {
            d.ledger = Some(format!("settle-result {name}: {act_name} ran {detail}"));
        }
        SettleOutcome::Gone => {
            d.ledger = Some(format!(
                "settle-result {name}: {act_name} gone host={unit_id}: stage {stage} launches again"
            ));
        }
        SettleOutcome::Attached => {
            let process_ref = process_ref.filter(|p| !p.is_empty()).ok_or_else(|| {
                "an attached result names the host's process reference".to_string()
            })?;
            if unit.status != Status::Running {
                return Err(format!(
                    "unit {unit_id} is {}, not running: it cannot attach to a host",
                    unit.status.as_str()
                ));
            }
            if view.running_stage_of(unit_id).is_some() {
                return Err(format!("unit {unit_id} already has a running stage"));
            }
            let unit_detail = view.unit_detail(unit)?;
            let track = workflow
                .track(&unit_detail.track)
                .ok_or_else(|| crate::admission::unknown_track(unit_id, &unit_detail.track))?;
            let def = track
                .stages
                .iter()
                .find(|s| s.name == stage)
                .ok_or_else(|| format!("track {} has no stage {stage}", track.name))?;
            let earlier = view
                .stages_of(unit_id)
                .into_iter()
                .filter(|n| n.detail == stage)
                .count();
            let attempt = u32::try_from(earlier + 1).unwrap_or(u32::MAX);
            let node_id = stage_node_id(unit_id, stage, attempt);
            d.effects.push(Effect::PutNode(NodeRow {
                run_id: unit.run_id.clone(),
                id: node_id.clone(),
                parent_id: Some(unit_id.to_string()),
                kind: NodeKind::Stage,
                status: Status::Running,
                reason: None,
                role: Some(def.role.clone()),
                model: Some(def.model.clone()),
                attempt,
                detail: stage.to_string(),
                created_at: now.to_string(),
                updated_at: now.to_string(),
            }));
            d.effects.push(Effect::PutSession(SessionRow {
                run_id: unit.run_id.clone(),
                id: node_id.clone(),
                node_id: node_id.clone(),
                unit_id: unit_id.to_string(),
                process_ref: Some(process_ref.to_string()),
                ended: false,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_micros: 0,
                created_at: now.to_string(),
                updated_at: now.to_string(),
            }));
            d.ledger = Some(format!(
                "settle-result {name}: {act_name} attached host={unit_id} session={node_id} stage={stage} attempt={attempt}"
            ));
        }
        SettleOutcome::Failed | SettleOutcome::Unknown => {
            let running = unit.status == Status::Running;
            let tail = if running {
                format!("; unit {unit_id} parked")
            } else {
                String::new()
            };
            d.ledger = Some(match outcome {
                SettleOutcome::Failed => {
                    let sep = if act == Act::Resume { ":" } else { "" };
                    format!("settle-result {name}: {act_name} failed{sep} {detail}{tail}")
                }
                _ => format!("settle-result {name}: unknown: {act_name} was not run again{tail}"),
            });
            if running {
                let (kind, cause, reason) = match (effect, outcome) {
                    (EffectKind::Rerun, SettleOutcome::Failed) => (
                        RequestKind::MergeFailed,
                        Cause::MergeChecksRed,
                        format!("the re-run of settle {name} failed: {detail}"),
                    ),
                    (EffectKind::Rerun, _) => (
                        RequestKind::MergeFailed,
                        Cause::MergeChecksRed,
                        format!(
                            "the result of settle {name} is unknown after a controller restart; its effect is not run again"
                        ),
                    ),
                    (EffectKind::Reattach, SettleOutcome::Failed) => (
                        RequestKind::SeatLost,
                        Cause::HostUnreachable,
                        format!(
                            "host unreachable: the re-attach of settle {name} failed: {detail}"
                        ),
                    ),
                    (EffectKind::Reattach, _) => (
                        RequestKind::SeatLost,
                        Cause::HostUnreachable,
                        format!(
                            "the result of settle {name} is unknown after a controller restart; its effect is not run again"
                        ),
                    ),
                };
                let reason = one_line_reason(&reason);
                let reason = check_reason(&reason).map_err(|e| e.to_string())?;
                let facts = json!({ "settle": name });
                d.effects.extend(park_effects(
                    view,
                    workflow,
                    unit,
                    kind,
                    Some(cause),
                    reason,
                    Some(&facts),
                    now,
                )?);
            }
        }
    }
    Ok(d)
}

/// A pause a test sets inside [`Engine::settle`]. It is off unless a test sets it.
pub type SettlePause = Arc<dyn Fn() + Send + Sync>;

impl Engine {
    /// A test point: call `pause` inside [`Engine::settle`], after the class read and with the
    /// engine's lock held, or `None` to clear it. No product code sets it.
    pub fn set_settle_pause(&self, pause: Option<SettlePause>) {
        *self
            .settle_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pause;
    }

    /// Settle one request under the clear grant. `forge` is the run's forge repo and default branch,
    /// as [`Engine::request_line`] takes them.
    ///
    /// `rerun_check` and `resolve_conflict` read the forge before the commit: the failed checks, or
    /// the PR's head. That read runs here, with no lock held, after a look at the request, the class
    /// and the cause, so no forge call is made for a settle that those checks refuse. The engine
    /// fills `facts.head_sha` from it. Then the locked steps run as they are and check everything
    /// again. A `Moved` or a `Refused` writes nothing.
    pub fn settle(
        &self,
        mut ask: SettleAsk,
        forge: Option<(&str, &str)>,
    ) -> Result<Admission, SettleError> {
        // What the controller passes as the head is dropped: the engine reads it.
        ask.facts.head_sha = None;
        if let Some(why) = self.degraded() {
            return Err(SettleError::Engine(EngineError::Degraded(why)));
        }
        if matches!(ask.act, Act::RerunCheck | Act::ResolveConflict) {
            self.settle_reads(&mut ask, forge)?;
        }
        self.settle_locked(ask, forge)
    }

    /// The forge reads that come before the commit of a `rerun_check` and a `resolve_conflict`.
    fn settle_reads(
        &self,
        ask: &mut SettleAsk,
        forge: Option<(&str, &str)>,
    ) -> Result<(), SettleError> {
        let id = command_id(&ask.request_id, ask.revision);
        // A settle that the store holds is a replay: the locked steps answer it, and no forge call
        // is made. A read that fails is left to the locked steps, which degrade the engine.
        if !matches!(self.store.admission(&self.cfg.run_id, &id), Ok(None)) {
            return Ok(());
        }
        let Ok(view) = self.view() else {
            return Ok(());
        };
        let request = open_for_settle(&view, &ask.request_id, ask.revision)
            .map_err(|why| settle_moved_or_refused(&view, &ask.request_id, ask.revision, why))?;
        // The kill switch comes before any forge read: a halted run calls no forge.
        if view.is_halted()
            && let Some(run) = view.run.as_ref()
        {
            return Err(SettleError::Refused(halted_text(&run.id)));
        }
        let record = self.request_line_record(&view, request, forge);
        if record.line != Line::Below {
            return Err(SettleError::Refused(format!(
                "request {} is above the line: only the claim may settle it",
                ask.request_id
            )));
        }
        check_cause_admits(record.cause, ask.act).map_err(SettleError::Refused)?;
        let unit_id = request.unit_id.as_deref().unwrap_or_default();
        let pr = view
            .node(unit_id)
            .and_then(|n| view.unit_detail(n).ok())
            .and_then(|d| d.pr)
            .ok_or_else(|| SettleError::Refused(format!("unit {unit_id} records no PR")))?;
        let pr_text = format!("{}#{}", pr.repo, pr.number);
        match ask.act {
            Act::RerunCheck => {
                let read = self.forge.failed_checks(&pr).map_err(|e| {
                    SettleError::Refused(format!("the PR's checks cannot be read: {}", e.0))
                })?;
                if read.failed.is_empty() {
                    return Err(SettleError::Refused(format!(
                        "PR {pr_text} has no failed check to run again"
                    )));
                }
                if let Some(blocked) = read.failed.iter().find(|c| c.blocked.is_some()) {
                    return Err(SettleError::Refused(format!(
                        "the failed check {} of PR {pr_text} cannot be run again by this forge: {}",
                        blocked.name,
                        blocked.blocked.as_deref().unwrap_or_default()
                    )));
                }
                ask.facts.head_sha = Some(read.head_sha);
            }
            _ => {
                let status = self.forge.pr_status(&pr).map_err(|e| {
                    SettleError::Refused(format!("the PR's head cannot be read: {}", e.0))
                })?;
                ask.facts.head_sha = Some(status.sha);
            }
        }
        Ok(())
    }

    /// The locked steps of a settle.
    fn settle_locked(
        &self,
        ask: SettleAsk,
        forge: Option<(&str, &str)>,
    ) -> Result<Admission, SettleError> {
        let mut inner = self.lock();
        // `Engine::degraded` takes the lock again: read the guard we hold.
        if let Some(why) = &inner.degraded {
            return Err(SettleError::Engine(EngineError::Degraded(why.clone())));
        }
        let id = command_id(&ask.request_id, ask.revision);
        let SettleAsk {
            request_id,
            revision,
            act,
            to_stage,
            mut facts,
        } = ask;
        // The replay first: a retry of a committed settle reaches its record, though its request is
        // closed now. The compare answers: the same content is the stored record, other content is a
        // conflict.
        let held = self.store.admission(&self.cfg.run_id, &id);
        let command = |facts: SettleFacts| Command {
            id: id.clone(),
            body: CommandBody::Settle {
                request_id: request_id.clone(),
                revision,
                act,
                to_stage: to_stage.clone(),
                facts,
            },
        };
        match held {
            Err(e) => {
                let why = format!("store read failed: {e}");
                return Err(SettleError::Engine(self.degrade(&mut inner, why)));
            }
            Ok(Some(_)) => {
                return self
                    .submit_locked(&mut inner, command(facts))
                    .map_err(settle_error);
            }
            Ok(None) => {}
        }
        let view = match self.view() {
            Ok(v) => v,
            Err(EngineError::Store(e)) => {
                let why = format!("store read failed: {e}");
                return Err(SettleError::Engine(self.degrade(&mut inner, why)));
            }
            Err(e) => return Err(SettleError::Engine(e)),
        };
        let request = view.request(&request_id).ok_or_else(|| {
            SettleError::Refused(format!("request {request_id} is unknown in this run"))
        })?;
        if request.status != RequestStatus::Open {
            return Err(SettleError::Moved(closed_text(&request_id)));
        }
        if request.revision != revision {
            return Err(SettleError::Moved(revision_text(
                &request_id,
                request.revision,
                revision,
            )));
        }
        let record = self.request_line_record(&view, request, forge);
        let pause = self
            .settle_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause();
        }
        if record.line != Line::Below {
            return Err(SettleError::Refused(format!(
                "request {request_id} is above the line: only the claim may settle it"
            )));
        }
        check_cause_admits(record.cause, act).map_err(SettleError::Refused)?;
        facts.line = record.to_json();
        self.submit_locked(&mut inner, command(facts))
            .map_err(settle_error)
    }

    /// The unit's pending settle, when its effect has not stored its result.
    pub fn pending_settle_of(&self, unit_id: &str) -> Result<Option<PendingSettle>, EngineError> {
        Ok(pending_settle(&self.view()?, unit_id))
    }

    /// Run the effect of an admitted settle, then store its result. A stored result is returned as
    /// it is, and the effect does not run again. A settle with no effect is refused.
    ///
    /// A run halted after the commit runs no effect: the result is `failed`, and the unit parks
    /// again. No lock is held while the effect runs.
    pub fn settle_effect(&self, request_id: &str, revision: u32) -> Result<Admission, SettleError> {
        let result_id = result_command_id(request_id, revision);
        match self.store.admission(&self.cfg.run_id, &result_id) {
            Ok(Some(row)) => {
                return Ok(Admission {
                    row,
                    replayed: true,
                });
            }
            Ok(None) => {}
            Err(e) => {
                let why = format!("store read failed: {e}");
                let mut inner = self.lock();
                return Err(SettleError::Engine(self.degrade(&mut inner, why)));
            }
        }
        let view = self.view().map_err(SettleError::Engine)?;
        let name = format!("{request_id}@{revision}");
        let settle_id = command_id(request_id, revision);
        let settle = view
            .admissions
            .iter()
            .find(|a| a.kind == "settle" && a.command_id == settle_id)
            .filter(|a| a.outcome == AdmissionOutcome::Admitted)
            .ok_or_else(|| SettleError::Refused(format!("settle {name} was not admitted")))?;
        let Some(CommandBody::Settle { act, .. }) = body_of_content(&settle.content) else {
            return Err(SettleError::Refused(format!(
                "settle {name} is not readable"
            )));
        };
        let request = view.request(request_id).ok_or_else(|| {
            SettleError::Refused(format!("request {request_id} is unknown in this run"))
        })?;
        let effect = effect_of(act, stored_cause(request)).ok_or_else(|| {
            SettleError::Refused(format!("settle {name} runs no effect, so it has no result"))
        })?;
        let unit_id = request.unit_id.clone().unwrap_or_default();
        let (outcome, detail, process_ref) = if view.is_halted() {
            (
                SettleOutcome::Failed,
                "the run is halted: the effect did not run".to_string(),
                None,
            )
        } else {
            match effect {
                EffectKind::Rerun => {
                    let (outcome, detail) = self.run_rerun(&view, request, &unit_id);
                    (outcome, detail, None)
                }
                EffectKind::Reattach => self.run_reattach(&view, &unit_id),
            }
        };
        self.submit(Command {
            id: result_id,
            body: CommandBody::SettleResult {
                request_id: request_id.to_string(),
                revision,
                outcome,
                detail: short_detail(&detail),
                process_ref,
            },
        })
        .map_err(settle_error)
    }

    /// Ask the forge to run the failed checks of the unit's PR again, on the head the settle stored.
    fn run_rerun(
        &self,
        view: &RunView,
        request: &RequestRow,
        unit_id: &str,
    ) -> (SettleOutcome, String) {
        let Some(pr) = view
            .node(unit_id)
            .and_then(|n| view.unit_detail(n).ok())
            .and_then(|d| d.pr)
        else {
            return (
                SettleOutcome::Failed,
                format!("unit {unit_id} records no PR: ran= not_ran=pr: no PR is recorded"),
            );
        };
        let head = request
            .answer
            .as_deref()
            .and_then(|a| serde_json::from_str::<Value>(a).ok())
            .and_then(|a| {
                a.get("head_sha")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let at = |sha: &str| format!("repo={} pr={} sha={sha}", pr.repo, pr.number);
        if head.is_empty() {
            return (
                SettleOutcome::Failed,
                format!(
                    "{}: ran= not_ran=head: the stored answer holds no head",
                    at("")
                ),
            );
        }
        match self.forge.rerun_failed_checks(&pr, &head) {
            Err(e) => (
                SettleOutcome::Failed,
                format!("{}: ran= not_ran=forge: {}", at(&head), e.0),
            ),
            Ok(effect) => {
                let ran = effect.ran.join(",");
                let mut not_ran: Vec<String> = effect
                    .not_ran
                    .iter()
                    .map(|(name, why)| format!("{name}: {why}"))
                    .collect();
                if let Some(moved) = &effect.head_moved {
                    not_ran.insert(0, format!("head: moved to {moved}"));
                }
                if effect.ran.is_empty() && not_ran.is_empty() {
                    not_ran.push("checks: none was run again".to_string());
                }
                if not_ran.is_empty() {
                    (
                        SettleOutcome::Ran,
                        format!("{} checks={ran}", at(&effect.sha)),
                    )
                } else {
                    (
                        SettleOutcome::Failed,
                        format!("{}: ran={ran} not_ran={}", at(&head), not_ran.join("; ")),
                    )
                }
            }
        }
    }

    /// Attach to the recorded host of the unit's stage again: the unit's newest session that holds a
    /// process reference.
    fn run_reattach(
        &self,
        view: &RunView,
        unit_id: &str,
    ) -> (SettleOutcome, String, Option<String>) {
        let Some(handle) = view
            .sessions
            .iter()
            .rfind(|s| s.unit_id == unit_id && s.process_ref.is_some())
            .and_then(|s| s.process_ref.clone())
        else {
            return (
                SettleOutcome::Failed,
                format!("unit {unit_id} has no session with a process reference"),
                None,
            );
        };
        let session = SessionRef(handle.clone());
        let failed = |why: String| (SettleOutcome::Failed, why, None);
        match self.agent.rebind(&session) {
            Ok(RebindOutcome::Survived) => (
                SettleOutcome::Attached,
                format!("host answered for {handle}"),
                Some(handle),
            ),
            Ok(RebindOutcome::Unreachable { why }) => {
                failed(format!("the host did not answer: {why}"))
            }
            Ok(RebindOutcome::Lost { cause }) if cause == NO_SESSION_HOST => {
                match self.agent.is_alive(&session) {
                    Ok(true) => (
                        SettleOutcome::Attached,
                        format!("the session {handle} is alive"),
                        Some(handle),
                    ),
                    Ok(false) => (
                        SettleOutcome::Gone,
                        format!("the session {handle} is gone"),
                        None,
                    ),
                    Err(e) => failed(format!("liveness cannot be read: {e}")),
                }
            }
            Ok(RebindOutcome::Lost { cause }) => (
                SettleOutcome::Gone,
                format!("the host is gone: {cause}"),
                None,
            ),
            Err(e) => failed(format!("the host cannot be attached: {e}")),
        }
    }
}

/// The detail line of a result: redacted, one line, at most [`RESULT_DETAIL_MAX`] characters.
fn short_detail(text: &str) -> String {
    one_line_reason(text)
        .chars()
        .take(RESULT_DETAIL_MAX)
        .collect()
}

/// A refusal of `open_for_settle` as the settle's error: the confirm path's texts are `Moved`.
fn settle_moved_or_refused(view: &RunView, id: &str, revision: u32, why: String) -> SettleError {
    let moved = view
        .request(id)
        .is_some_and(|r| r.status != RequestStatus::Open || r.revision != revision);
    if moved {
        SettleError::Moved(why)
    } else {
        SettleError::Refused(why)
    }
}
