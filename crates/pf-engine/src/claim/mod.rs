//! Requests and the claim (ADR-0005, ADR-0006, ADR-0009; `docs/contracts/requests.md`).
//!
//! Every halt opens a request. A request is something an operator can see and answer. Only the
//! session that holds the claim answers, and an answer binds to an exact request ID and revision
//! and to the claim's generation. A new claim fences the old one out with a fresh generation.
//!
//! **Commands.** Each of these is admitted, so each is in the admissions table with its ID:
//! `OpenRequest` (`request:<run>:<unit or ->:<kind>:<generation>`), `ReviseRequest`
//! (`revise:<id>:<revision>`), `AnswerRequest` (`answer:<id>:<revision>`), `WithdrawRequest`
//! (`withdraw:<id>`), `Claim` (`claim:<generation>`) and `ClearUnit`
//! (`clear:<unit>:<park generation>`). A park opens its request in the same commit as the park.
//!
//! **The claim token** never reaches the engine. The controller makes it, hashes it and sends the
//! hash. The engine stores the hash and compares generations.

mod detail;
mod sha256;

pub use detail::{
    GAP_TEXT_MAX_CHARS, GAPS_FILE_MAX_BYTES, KEY_GAPS, KEY_GAPS_ERROR, KEY_STOP_TEXT,
    STOP_TEXT_MAX_BYTES, gaps_detail, gaps_of, stop_detail,
};
pub use sha256::sha256_hex;

use serde_json::{Value, json};

use crate::admission::{Admission, Command, CommandBody, Decision, EngineError, RunView};
use crate::engine::Engine;
use crate::port::store::{
    ClaimRow, Effect, NewRequest, NodeKind, NodeRow, RequestKind, RequestRow, RequestStatus,
};
use crate::status::{REASON_RUN_HALTED, Status, check_reason, one_line_reason};
use crate::workflow::Workflow;

type Rejected = String;

/// The longest answer text a ledger line carries, in characters.
pub const ANSWER_LEDGER_CHARS: usize = 200;

/// The verb of an answer that records a ruling.
pub const VERB_ANSWER: &str = "answer";
/// The verb of an answer that resumes a unit at a stage.
pub const VERB_CLEAR: &str = "clear";
/// The verb of a withdrawal.
pub const VERB_WITHDRAW: &str = "withdraw";

/// The stage a request names when the unit parked at its merge, with every stage done.
pub const STAGE_MERGE: &str = "merge";

// ------------------------------------------------------------------ the claim

/// Check that `generation` is the current claim of `claims`. The refusal names its cause.
pub fn check_claim_in(claims: &[ClaimRow], generation: u64) -> Result<(), Rejected> {
    let current = claims
        .iter()
        .filter(|c| c.fenced_at.is_none())
        .map(|c| c.generation)
        .max();
    match current {
        Some(now) if now == generation => Ok(()),
        _ => {
            let held = claims.iter().find(|c| c.generation == generation);
            match (held, current) {
                (Some(old), Some(now)) if old.fenced_at.is_some() => {
                    let by = generation + 1;
                    if by == now {
                        Err(format!("claim {generation} was fenced by claim {now}"))
                    } else {
                        Err(format!(
                            "claim {generation} was fenced by claim {by}; the current claim is {now}"
                        ))
                    }
                }
                (_, Some(now)) => Err(format!(
                    "claim {generation} is not the current claim; the current claim is {now}"
                )),
                (_, None) => Err(format!(
                    "claim {generation} is not the current claim; no claim has been made"
                )),
            }
        }
    }
}

/// Check that `generation` is the current claim. The refusal names its cause.
pub fn check_claim(view: &RunView, generation: u64) -> Result<(), Rejected> {
    check_claim_in(&view.claims, generation)
}

/// The claim a token hash belongs to, current or fenced.
pub fn claim_of_hash<'a>(claims: &'a [ClaimRow], token_hash: &str) -> Option<&'a ClaimRow> {
    claims.iter().find(|c| c.token_hash == token_hash)
}

pub(crate) fn decide_claim(
    view: &RunView,
    generation: u64,
    holder: &str,
    token_hash: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    let next = view.last_claim_generation() + 1;
    if generation != next {
        return Err(format!(
            "claim generation {generation} is not the next one; the next is {next}"
        ));
    }
    let holder = check_reason(holder).map_err(|e| format!("the claim's holder: {e}"))?;
    if token_hash.len() != 64 || !token_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the claim's token hash is not 64 hex digits".to_string());
    }
    let mut d = Decision::admitted();
    if let Some(old) = view.current_claim() {
        let mut old = old.clone();
        old.fenced_at = Some(now.to_string());
        d.effects.push(Effect::PutClaim(old));
    }
    d.effects.push(Effect::PutClaim(ClaimRow {
        generation,
        token_hash: token_hash.to_ascii_lowercase(),
        holder: holder.to_string(),
        claimed_at: now.to_string(),
        fenced_at: None,
    }));
    d.ledger = Some(format!("claim {generation} by {holder}"));
    Ok(d)
}

// ------------------------------------------------------------------ parks and requests

/// The stage a unit is in now: its running stage, else the next stage to launch, else
/// [`STAGE_MERGE`] when every stage is done and the track merges.
pub(crate) fn parked_stage(view: &RunView, workflow: &Workflow, unit: &NodeRow) -> String {
    if let Some(running) = view.running_stage_of(&unit.id) {
        return running.detail.clone();
    }
    let Ok(detail) = view.unit_detail(unit) else {
        return String::new();
    };
    let Some(track) = workflow.track(&detail.track) else {
        return String::new();
    };
    match track.stages.get(detail.stages_done as usize) {
        Some(stage) => stage.name.clone(),
        None if track.merge.is_some() => STAGE_MERGE.to_string(),
        None => String::new(),
    }
}

/// The keys of a request body that the engine sets. A park's `detail` may not set them.
const RESERVED_BODY_KEYS: [&str; 3] = ["reason", "stage", "park_generation"];

/// The effects of a park: the unit becomes `parked` and its request opens, in one commit.
/// `detail` holds more facts for the request body, such as a gap list or a stop file's text.
pub(crate) fn park_effects(
    view: &RunView,
    workflow: &Workflow,
    unit: &NodeRow,
    kind: RequestKind,
    reason: &str,
    detail_facts: Option<&Value>,
    now: &str,
) -> Result<Vec<Effect>, Rejected> {
    if kind == RequestKind::MergeAck {
        return Err("a unit park cannot open a merge_ack request".to_string());
    }
    if let Some(open) = view.open_request_of(&unit.id) {
        return Err(format!(
            "unit {} has request {} open already",
            unit.id, open.id
        ));
    }
    let detail = view.unit_detail(unit)?;
    let stage = parked_stage(view, workflow, unit);
    let mut body = match kind {
        // A gaps or a stop request holds its reason, and the facts of its seat's file.
        RequestKind::Gaps | RequestKind::Stop => json!({ "reason": reason }),
        _ => json!({
            "reason": reason,
            "stage": stage,
            "park_generation": detail.park_generation,
        }),
    };
    if let Some(facts) = detail_facts {
        let facts = facts
            .as_object()
            .ok_or_else(|| "a park's facts are not a JSON object".to_string())?;
        if let Some(key) = RESERVED_BODY_KEYS.iter().find(|k| facts.contains_key(**k)) {
            return Err(format!("a park's facts may not set the body key {key:?}"));
        }
        if let Some(object) = body.as_object_mut() {
            object.extend(facts.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    let mut parked = unit.clone();
    parked.status = Status::Parked;
    parked.reason = Some(reason.to_string());
    parked.updated_at = now.to_string();
    Ok(vec![
        Effect::PutNode(parked),
        Effect::OpenRequest(NewRequest {
            run_id: unit.run_id.clone(),
            unit_id: Some(unit.id.clone()),
            stage,
            kind,
            reason: reason.to_string(),
            body: body.to_string(),
            opened_at: now.to_string(),
        }),
    ])
}

pub(crate) fn decide_open_request(
    view: &RunView,
    run_id: &str,
    request: &crate::admission::OpenRequestBody,
    now: &str,
) -> Result<Decision, Rejected> {
    let reason = check_reason(&request.reason).map_err(|e| format!("the request's reason: {e}"))?;
    serde_json::from_str::<Value>(&request.body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| "the request's body is not a JSON object".to_string())?;
    let mut d = Decision::admitted();
    if let Some(unit_id) = &request.unit_id {
        view.node(unit_id)
            .filter(|n| n.kind == NodeKind::Unit)
            .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
        if let Some(open) = view.open_request_of(unit_id) {
            return Err(format!(
                "unit {unit_id} has request {} open already",
                open.id
            ));
        }
        d.unit_id = Some(unit_id.clone());
    }
    d.effects.push(Effect::OpenRequest(NewRequest {
        run_id: run_id.to_string(),
        unit_id: request.unit_id.clone(),
        stage: request.stage.clone(),
        kind: request.kind,
        reason: reason.to_string(),
        body: request.body.clone(),
        opened_at: now.to_string(),
    }));
    Ok(d)
}

/// The request a command names, in this run.
fn run_request<'a>(view: &'a RunView, id: &str) -> Result<&'a RequestRow, Rejected> {
    view.request(id)
        .ok_or_else(|| format!("request {id} is unknown in this run"))
}

/// The request is open, and `revision` is its current one.
fn open_at<'a>(view: &'a RunView, id: &str, revision: u32) -> Result<&'a RequestRow, Rejected> {
    let request = run_request(view, id)?;
    if request.status != RequestStatus::Open {
        return Err(format!(
            "request {id} is {}, not open",
            request.status.as_str()
        ));
    }
    if request.revision != revision {
        return Err(format!(
            "request {id} is at revision {}, not {revision}",
            request.revision
        ));
    }
    Ok(request)
}

pub(crate) fn decide_revise(
    view: &RunView,
    id: &str,
    revision: u32,
    body: &str,
) -> Result<Decision, Rejected> {
    let request = open_at(view, id, revision)?;
    serde_json::from_str::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| "the request's body is not a JSON object".to_string())?;
    let mut next = request.clone();
    next.revision += 1;
    next.body = body.to_string();
    let mut d = Decision::admitted();
    d.unit_id = request.unit_id.clone();
    d.effects.push(Effect::PutRequest(next));
    Ok(d)
}

pub(crate) fn decide_withdraw(
    view: &RunView,
    id: &str,
    reason: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    let reason = check_reason(reason).map_err(|e| format!("the withdrawal's reason: {e}"))?;
    let request = run_request(view, id)?;
    if request.status != RequestStatus::Open {
        return Err(format!(
            "request {id} is {}, not open",
            request.status.as_str()
        ));
    }
    let mut d = Decision::admitted();
    d.unit_id = request.unit_id.clone();
    d.effects
        .push(Effect::PutRequest(withdrawn(request, reason, now)));
    Ok(d)
}

fn withdrawn(request: &RequestRow, reason: &str, now: &str) -> RequestRow {
    let mut r = request.clone();
    r.status = RequestStatus::Withdrawn;
    r.answered_at = Some(now.to_string());
    r.answer = Some(json!({ "verb": VERB_WITHDRAW, "reason": reason }).to_string());
    r
}

/// Withdraw every open request of the run, because the run ends.
pub(crate) fn withdraw_all(view: &RunView, reason: &str, now: &str) -> Vec<Effect> {
    view.open_requests()
        .map(|r| Effect::PutRequest(withdrawn(r, reason, now)))
        .collect()
}

// ------------------------------------------------------------------ answers

/// Resume a parked unit at a stage and close its request. Answer and clear share this.
///
/// The unit becomes `running` with no running session. Its park generation rises, so each resume
/// uses new command IDs. A unit that parked after its PR was recorded keeps its PR. On a halted
/// run the unit carries the reason "run halted", and the driver launches and merges nothing.
#[allow(clippy::too_many_arguments)]
fn resume_effects(
    view: &RunView,
    workflow: &Workflow,
    unit: &NodeRow,
    request: &RequestRow,
    to_stage: Option<&str>,
    generation: u64,
    answer: impl FnOnce(&str) -> Value,
    now: &str,
) -> Result<(Vec<Effect>, String), Rejected> {
    if unit.status != Status::Parked {
        return Err(format!(
            "unit {} is {}, not parked",
            unit.id,
            unit.status.as_str()
        ));
    }
    let mut detail = view.unit_detail(unit)?;
    let track = workflow
        .track(&detail.track)
        .ok_or_else(|| crate::admission::unknown_track(&unit.id, &detail.track))?;
    let resolved = to_stage.map_or_else(|| request.stage.clone(), str::to_string);
    match track.stages.iter().position(|s| s.name == resolved) {
        Some(index) => detail.stages_done = u32::try_from(index).unwrap_or(u32::MAX),
        None if to_stage.is_some() => {
            let names: Vec<&str> = track.stages.iter().map(|s| s.name.as_str()).collect();
            return Err(format!(
                "{resolved:?} is not a stage of track {} (its stages: {})",
                track.name,
                names.join(", ")
            ));
        }
        // The unit parked at its merge, or before it had a stage: it resumes where it is.
        None => {}
    }
    if view.running_stage_of(&unit.id).is_some() {
        return Err(format!(
            "unit {} still has a running session; the controller ends it on its next tick",
            unit.id
        ));
    }
    detail.park_generation += 1;
    let mut resumed = unit.clone();
    resumed.status = Status::Running;
    resumed.reason = view.is_halted().then(|| REASON_RUN_HALTED.to_string());
    resumed.detail = detail.to_text();
    resumed.updated_at = now.to_string();
    let mut closed = request.clone();
    closed.status = RequestStatus::Answered;
    closed.answered_at = Some(now.to_string());
    closed.answer = Some(answer(&resolved).to_string());
    closed.answer_generation = Some(generation);
    Ok((
        vec![Effect::PutNode(resumed), Effect::PutRequest(closed)],
        resolved,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_answer(
    view: &RunView,
    workflow: &Workflow,
    id: &str,
    revision: u32,
    generation: u64,
    answer: &Value,
    hook_failed: Option<&str>,
    now: &str,
) -> Result<Decision, Rejected> {
    check_claim(view, generation)?;
    let request = open_at(view, id, revision)?;
    let verb = answer.get("verb").and_then(Value::as_str).unwrap_or("");
    if verb != VERB_ANSWER {
        return Err(format!(
            "an answer carries the verb {VERB_ANSWER:?}, not {verb:?}"
        ));
    }
    let text = answer
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "an answer needs text".to_string())?;
    // The text is the operator's own words. It is stored as given, and the ledger line carries
    // its first characters, flat and redacted.
    let brief: String = one_line_reason(text)
        .chars()
        .take(ANSWER_LEDGER_CHARS)
        .collect();
    let mut d = Decision::admitted();
    d.unit_id = request.unit_id.clone();
    match &request.unit_id {
        Some(unit_id) => {
            let unit = view
                .node(unit_id)
                .filter(|n| n.kind == NodeKind::Unit)
                .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
            d.effects = match hook_failed {
                None => {
                    resume_effects(
                        view,
                        workflow,
                        unit,
                        request,
                        None,
                        generation,
                        |_| answer.clone(),
                        now,
                    )?
                    .0
                }
                Some(cause) => {
                    parked_again_effects(view, unit, request, generation, answer, cause, now)?
                }
            };
        }
        None => {
            let mut closed = request.clone();
            closed.status = RequestStatus::Answered;
            closed.answered_at = Some(now.to_string());
            closed.answer = Some(answer.to_string());
            closed.answer_generation = Some(generation);
            d.effects.push(Effect::PutRequest(closed));
        }
    }
    d.ledger = Some(match hook_failed {
        None => format!("answer {id}@{revision} by claim {generation}: {brief}"),
        Some(_) => format!(
            "answer {id}@{revision} by claim {generation}: {brief} (the answer hook failed; the unit parked again)"
        ),
    });
    Ok(d)
}

/// The effects of an answer whose hook failed. The request is answered, as it would be for a
/// resume. The unit stays parked and a new `park` request opens for it, in the same commit, with
/// the hook's cause. The park generation rises, as it does for a resume, so the new request has
/// its own command IDs.
fn parked_again_effects(
    view: &RunView,
    unit: &NodeRow,
    request: &RequestRow,
    generation: u64,
    answer: &Value,
    cause: &str,
    now: &str,
) -> Result<Vec<Effect>, Rejected> {
    if unit.status != Status::Parked {
        return Err(format!(
            "unit {} is {}, not parked",
            unit.id,
            unit.status.as_str()
        ));
    }
    if view.running_stage_of(&unit.id).is_some() {
        return Err(format!(
            "unit {} still has a running session; the controller ends it on its next tick",
            unit.id
        ));
    }
    let reason = check_reason(&one_line_reason(&format!("answer hook failed: {cause}")))
        .map_err(|e| format!("the hook's cause: {e}"))?
        .to_string();
    let mut detail = view.unit_detail(unit)?;
    detail.park_generation += 1;
    let mut parked = unit.clone();
    parked.status = Status::Parked;
    parked.reason = Some(reason.clone());
    parked.detail = detail.to_text();
    parked.updated_at = now.to_string();
    let mut closed = request.clone();
    closed.status = RequestStatus::Answered;
    closed.answered_at = Some(now.to_string());
    closed.answer = Some(answer.to_string());
    closed.answer_generation = Some(generation);
    Ok(vec![
        Effect::PutNode(parked),
        Effect::PutRequest(closed),
        Effect::OpenRequest(NewRequest {
            run_id: unit.run_id.clone(),
            unit_id: Some(unit.id.clone()),
            stage: request.stage.clone(),
            kind: RequestKind::Park,
            reason: reason.clone(),
            body: json!({
                "reason": reason,
                "stage": request.stage,
                "park_generation": detail.park_generation,
            })
            .to_string(),
            opened_at: now.to_string(),
        }),
    ])
}

/// An answer hook ran: write its ledger line. It changes no state, so it is recorded whether the
/// hook passed or failed. The request must exist in this run.
pub(crate) fn decide_answer_hook(
    view: &RunView,
    id: &str,
    rc: Option<i32>,
    note: &str,
) -> Result<Decision, Rejected> {
    let request = run_request(view, id)?;
    let mut d = Decision::admitted();
    d.unit_id = request.unit_id.clone();
    d.ledger = Some(match (rc, note.is_empty()) {
        (Some(code), true) => format!("answer hook {id}: rc {code}"),
        (Some(code), false) => format!("answer hook {id}: rc {code}: {note}"),
        (None, true) => format!("answer hook {id}: rc none"),
        (None, false) => format!("answer hook {id}: rc none: {note}"),
    });
    Ok(d)
}

pub(crate) fn decide_clear(
    view: &RunView,
    workflow: &Workflow,
    unit_id: &str,
    to_stage: Option<&str>,
    generation: u64,
    request_id: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    check_claim(view, generation)?;
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
    let request = match view.open_request_of(unit_id) {
        Some(r) if r.id == request_id => r,
        Some(r) => {
            return Err(format!(
                "unit {unit_id} has request {} open, not {request_id}",
                r.id
            ));
        }
        None => return Err(format!("unit {unit_id} has no open request")),
    };
    // The stored answer names the stage the unit resumed at, also when the caller left it out.
    let (effects, resolved) = resume_effects(
        view,
        workflow,
        unit,
        request,
        to_stage,
        generation,
        |resolved| json!({ "verb": VERB_CLEAR, "to_stage": resolved }),
        now,
    )?;
    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    d.effects = effects;
    d.ledger = Some(format!(
        "answer {}@{} by claim {generation}: clear {unit_id} --to {resolved}",
        request.id, request.revision
    ));
    Ok(d)
}

// ------------------------------------------------------------------ the engine's calls

/// What `Engine::claim` returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    pub generation: u64,
    pub admission: Admission,
}

impl Engine {
    /// The park generation of a unit: how many times a claim has resumed it.
    fn park_generation_of(&self, view: &RunView, unit_id: &str) -> Result<u32, EngineError> {
        let unit = view
            .node(unit_id)
            .filter(|n| n.kind == NodeKind::Unit)
            .ok_or_else(|| EngineError::Rejected(format!("unit {unit_id} is unknown")))?;
        view.unit_detail(unit)
            .map(|d| d.park_generation)
            .map_err(EngineError::Rejected)
    }

    /// Park a unit that is `running`, and open its request of `kind`, in one commit. The kind
    /// comes from where the park happens in code, never from the reason text.
    pub fn park_as(
        &self,
        unit_id: &str,
        kind: RequestKind,
        reason: &str,
    ) -> Result<Admission, EngineError> {
        self.park_with(unit_id, kind, reason, None)
    }

    /// [`Engine::park_as`], with more facts for the request body: a JSON object, such as the
    /// result of [`gaps_detail`] or [`stop_detail`]. The facts are data and may not set `reason`,
    /// `stage` or `park_generation`.
    pub fn park_with(
        &self,
        unit_id: &str,
        kind: RequestKind,
        reason: &str,
        detail: Option<&Value>,
    ) -> Result<Admission, EngineError> {
        let view = self.view()?;
        let generation = self.park_generation_of(&view, unit_id)?;
        self.submit(Command {
            id: format!("park:{unit_id}:{generation}"),
            body: CommandBody::Park {
                unit_id: unit_id.to_string(),
                reason: one_line_reason(reason),
                request_kind: kind,
                detail: detail.cloned(),
            },
        })
    }

    /// Open a request that no park carries. `unit_id` is `None` for a run-level request.
    pub fn open_request(
        &self,
        unit_id: Option<&str>,
        kind: RequestKind,
        stage: &str,
        reason: &str,
        body: &Value,
    ) -> Result<Admission, EngineError> {
        let view = self.view()?;
        let generation = match unit_id {
            Some(unit) => u64::from(self.park_generation_of(&view, unit)?),
            // A run has at most one open run-level request of a kind. The ID counts the ones
            // that closed, so a repeat of the open one is a replay.
            None => view
                .requests
                .iter()
                .filter(|r| r.unit_id.is_none() && r.kind == kind)
                .filter(|r| r.status != RequestStatus::Open)
                .count() as u64,
        };
        self.submit(Command {
            id: format!(
                "request:{}:{}:{}:{generation}",
                self.cfg.run_id,
                unit_id.unwrap_or("-"),
                kind.as_str()
            ),
            body: CommandBody::OpenRequest {
                request: crate::admission::OpenRequestBody {
                    unit_id: unit_id.map(str::to_string),
                    stage: stage.to_string(),
                    kind,
                    reason: one_line_reason(reason),
                    body: body.to_string(),
                },
            },
        })
    }

    /// Change the facts of an open request. `revision` is the one the caller read.
    pub fn revise_request(
        &self,
        id: &str,
        revision: u32,
        body: &Value,
    ) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("revise:{id}:{revision}"),
            body: CommandBody::ReviseRequest {
                id: id.to_string(),
                revision,
                body: body.to_string(),
            },
        })
    }

    /// Answer a request with a ruling. The claim `generation` must be the current one.
    pub fn answer_request(
        &self,
        id: &str,
        revision: u32,
        generation: u64,
        text: &str,
    ) -> Result<Admission, EngineError> {
        self.answer_request_after_hook(id, revision, generation, text, None)
    }

    /// [`Engine::answer_request`] after the workflow's answer hook ran. With `hook_failed` set,
    /// the request is answered, the unit stays parked, and a new `park` request opens with
    /// `answer hook failed: <cause>`, all in one commit.
    pub fn answer_request_after_hook(
        &self,
        id: &str,
        revision: u32,
        generation: u64,
        text: &str,
        hook_failed: Option<&str>,
    ) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("answer:{id}:{revision}"),
            body: CommandBody::AnswerRequest {
                id: id.to_string(),
                revision,
                generation,
                answer: json!({ "verb": VERB_ANSWER, "text": text }),
                hook_failed: hook_failed.map(one_line_reason),
            },
        })
    }

    /// Would [`Engine::answer_request`] admit this answer now? It decides against the stored
    /// state and records nothing. The controller asks before it runs a hook, so a refused
    /// answer never runs one.
    pub fn check_answer(
        &self,
        id: &str,
        revision: u32,
        generation: u64,
        text: &str,
    ) -> Result<(), EngineError> {
        let view = self.view()?;
        let answer = json!({ "verb": VERB_ANSWER, "text": text });
        decide_answer(
            &view,
            &self.cfg.workflow,
            id,
            revision,
            generation,
            &answer,
            None,
            &self.now(),
        )
        .map(|_| ())
        .map_err(EngineError::Rejected)
    }

    /// Write the ledger line of one run of an answer hook: `answer hook <R-n>: rc <n>`. Each run
    /// has its own command ID, so a second answer attempt on the same request is a new line.
    pub fn record_answer_hook(
        &self,
        id: &str,
        revision: u32,
        rc: Option<i32>,
        note: &str,
    ) -> Result<Admission, EngineError> {
        let view = self.view()?;
        let prefix = format!("answer-hook:{id}:{revision}:");
        let attempt = view
            .admissions
            .iter()
            .filter(|a| a.command_id.starts_with(&prefix))
            .count() as u32;
        self.submit(Command {
            id: format!("{prefix}{attempt}"),
            body: CommandBody::AnswerHook {
                id: id.to_string(),
                revision,
                attempt,
                rc,
                // An empty note stays empty: `one_line_reason` would fill it in.
                note: if note.trim().is_empty() {
                    String::new()
                } else {
                    one_line_reason(note)
                },
            },
        })
    }

    /// Close an open request that nothing will answer.
    pub fn withdraw_request(&self, id: &str, reason: &str) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("withdraw:{id}"),
            body: CommandBody::WithdrawRequest {
                id: id.to_string(),
                reason: one_line_reason(reason),
            },
        })
    }

    /// Take the claim. `token_hash` is the SHA-256 of a token the caller keeps. The old claim is
    /// fenced out with the next generation.
    pub fn claim(&self, holder: &str, token_hash: &str) -> Result<Granted, EngineError> {
        let view = self.view()?;
        let generation = view.last_claim_generation() + 1;
        let admission = self.submit(Command {
            id: format!("claim:{generation}"),
            body: CommandBody::Claim {
                generation,
                holder: one_line_reason(holder),
                token_hash: token_hash.to_string(),
            },
        })?;
        Ok(Granted {
            generation,
            admission,
        })
    }

    /// Resume a parked unit at `to_stage` (default: the stage it parked in), in the name of the
    /// claim `generation`, answering the unit's open request `request_id`.
    pub fn clear_unit(
        &self,
        unit_id: &str,
        to_stage: Option<&str>,
        generation: u64,
        request_id: &str,
    ) -> Result<Admission, EngineError> {
        let view = self.view()?;
        let park_generation = self.park_generation_of(&view, unit_id)?;
        self.submit(Command {
            id: format!("clear:{unit_id}:{park_generation}"),
            body: CommandBody::ClearUnit {
                unit_id: unit_id.to_string(),
                to_stage: to_stage.map(str::to_string),
                generation,
                request_id: request_id.to_string(),
            },
        })
    }

    /// The run's requests, in the order they were opened.
    pub fn requests(&self) -> Result<Vec<RequestRow>, EngineError> {
        Ok(self.view()?.requests)
    }
}
