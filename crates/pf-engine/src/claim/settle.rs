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

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::line::{Line, stored_cause};
use super::{STAGE_MERGE, resume_effects};
use crate::admission::{Admission, Command, CommandBody, Decision, EngineError, RunView};
use crate::engine::Engine;
use crate::port::store::{Cause, Effect, NodeKind, RequestKind, RequestRow, RequestStatus};
use crate::status::Status;
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
        return Err(format!(
            "run {} is halted: a halted run refuses every settle",
            run.id
        ));
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
        to_stage,
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
    d.ledger = Some(format!(
        "settle {request_id}@{revision} by grant {}#{} session {}: act={}{to_tail} reason={}",
        facts.project,
        facts.generation,
        facts.session,
        act.as_str(),
        facts.reason
    ));
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
    /// The whole method runs under the engine's lock, so the class that it reads is the class of the
    /// request that the commit closes. A `Moved` or a `Refused` writes nothing.
    pub fn settle(
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
}
