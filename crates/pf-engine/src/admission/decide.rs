//! The decision for one command: what it writes, and whether it is admitted or refused.
//!
//! `decide` is a pure function of the stored state. A command that is not valid in that state
//! returns `Err` and records nothing. A command the run refuses on purpose, such as a launch after
//! `halt`, returns a `Refused` decision, and the store records the refusal with its reason.

use crate::claim;
use crate::launch;
use crate::merge;
use crate::port::store::{
    AdmissionOutcome, Effect, NodeKind, NodeRow, RequestKind, RunRow, RunState,
};
use crate::status::{EndStatus, Status, check_reason};
use crate::unit::{Signal, UnitDetail};
use crate::workflow::Workflow;

use super::{Command, CommandBody, PushOutcome, RunView};

/// What `decide` returns for a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub outcome: AdmissionOutcome,
    pub reason: Option<String>,
    /// For the admission record: the unit, role, model and attempt of a launch.
    pub unit_id: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub attempt: Option<u32>,
    pub effects: Vec<Effect>,
    /// The text of the ledger line, when the command writes one.
    pub ledger: Option<String>,
}

impl Decision {
    pub(crate) fn admitted() -> Decision {
        Decision {
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            effects: Vec::new(),
            ledger: None,
        }
    }

    pub(crate) fn refused(reason: &str) -> Decision {
        Decision {
            outcome: AdmissionOutcome::Refused,
            reason: Some(reason.to_string()),
            ..Decision::admitted()
        }
    }
}

type Rejected = String;

/// Decide one command against the stored state of its run.
pub fn decide(
    view: &RunView,
    run_id: &str,
    workflow: &Workflow,
    command: &Command,
    now: &str,
) -> Result<Decision, Rejected> {
    if let CommandBody::OpenRun {
        workflow: name,
        ledger_file,
    } = &command.body
    {
        if view.run.is_some() {
            return Err(format!("run {run_id} is already open"));
        }
        let mut d = Decision::admitted();
        d.effects.push(Effect::PutRun(RunRow {
            id: run_id.to_string(),
            workflow: name.clone(),
            ledger_file: ledger_file.clone(),
            state: RunState::Active,
            halted_reason: None,
            project_id: None,
            created_at: now.to_string(),
            updated_at: now.to_string(),
        }));
        d.ledger = Some(format!("run opened for workflow {name}"));
        return Ok(d);
    }

    let run = view
        .run
        .as_ref()
        .ok_or_else(|| format!("run {run_id} is not open"))?;
    if run.state != RunState::Active {
        return Err(format!("run {run_id} has ended"));
    }

    match &command.body {
        CommandBody::OpenRun { .. } => unreachable!("handled above"),
        CommandBody::AdmitUnit { unit, track } => {
            admit_unit(view, workflow, run_id, unit, track, now)
        }
        CommandBody::Launch {
            unit_id,
            stage,
            role,
            model,
            attempt,
        } => launch::decide_launch(view, workflow, unit_id, stage, role, model, *attempt, now),
        CommandBody::Attach {
            session_id,
            process_ref,
        } => attach(view, session_id, process_ref, now),
        CommandBody::Signal {
            unit_id,
            stage,
            signal,
            pr,
        } => signal_stage(
            view,
            workflow,
            unit_id,
            stage,
            signal,
            pr.clone(),
            None,
            now,
        ),
        CommandBody::Outcome {
            unit_id,
            stage,
            entry,
            signal,
            pr,
        } => signal_stage(
            view,
            workflow,
            unit_id,
            stage,
            signal,
            pr.clone(),
            Some(*entry),
            now,
        ),
        CommandBody::Usage { session_id, usage } => {
            let session = view
                .session(session_id)
                .ok_or_else(|| format!("session {session_id} is unknown"))?;
            let mut s = session.clone();
            s.input_tokens = s.input_tokens.saturating_add(usage.input_tokens);
            s.output_tokens = s.output_tokens.saturating_add(usage.output_tokens);
            s.cache_read_tokens = s.cache_read_tokens.saturating_add(usage.cache_read_tokens);
            s.cache_write_tokens = s
                .cache_write_tokens
                .saturating_add(usage.cache_write_tokens);
            s.cost_micros = s.cost_micros.saturating_add(usage.cost_micros);
            s.updated_at = now.to_string();
            let mut d = Decision::admitted();
            d.unit_id = Some(s.unit_id.clone());
            d.effects.push(Effect::PutSession(s));
            Ok(d)
        }
        CommandBody::End {
            node_id,
            status,
            reason,
        } => end_node(view, node_id, *status, reason, now),
        CommandBody::Park {
            unit_id,
            reason,
            request_kind,
            detail,
        } => park_unit(
            view,
            workflow,
            unit_id,
            *request_kind,
            reason,
            detail.as_ref(),
            now,
        ),
        CommandBody::Halt { reason } => launch::decide_halt(view, workflow, reason, now),
        CommandBody::Merge { unit_id } => merge::decide_merge(view, workflow, unit_id, now),
        CommandBody::MergeResult {
            unit_id,
            repo,
            branch,
            sha,
            outcome,
            landed,
        } => merge::decide_merge_result(
            view, workflow, unit_id, repo, branch, sha, outcome, *landed, now,
        ),
        CommandBody::Push {
            unit_id,
            repo,
            branch,
            sha,
            outcome,
            ..
        } => push_effect(view, unit_id, repo, branch, sha, outcome),
        CommandBody::Decision { unit_id, text, .. } => {
            view.node(unit_id)
                .filter(|n| n.kind == NodeKind::Unit)
                .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
            let mut d = Decision::admitted();
            d.unit_id = Some(unit_id.clone());
            d.ledger = Some(format!("decision: unit={unit_id} {text}"));
            Ok(d)
        }
        CommandBody::Note { unit_id, text, .. } => {
            view.node(unit_id)
                .filter(|n| n.kind == NodeKind::Unit)
                .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
            let mut d = Decision::admitted();
            d.unit_id = Some(unit_id.clone());
            d.ledger = Some(format!("note: unit={unit_id} {text}"));
            Ok(d)
        }
        CommandBody::EndRun { reason } => {
            let reason = check_reason(reason).map_err(|e| e.to_string())?;
            let mut r = run.clone();
            r.state = RunState::Ended;
            r.updated_at = now.to_string();
            let mut d = Decision::admitted();
            d.effects.push(Effect::PutRun(r));
            // Nothing answers a request of an ended run.
            d.effects.extend(claim::withdraw_all(
                view,
                &format!("run ended: {reason}"),
                now,
            ));
            d.ledger = Some(format!("run ended: {reason}"));
            Ok(d)
        }
        CommandBody::OpenRequest { request } => {
            claim::decide_open_request(view, run_id, request, now)
        }
        CommandBody::ReviseRequest {
            id,
            revision,
            body,
            reason,
        } => claim::decide_revise(view, id, *revision, body, reason.as_deref()),
        CommandBody::AnswerRequest {
            id,
            revision,
            generation,
            answer,
            hook_failed,
        } => claim::decide_answer(
            view,
            workflow,
            id,
            *revision,
            *generation,
            answer,
            hook_failed.as_deref(),
            now,
        ),
        CommandBody::AnswerHook { id, rc, note, .. } => {
            claim::decide_answer_hook(view, id, *rc, note)
        }
        CommandBody::WithdrawRequest { id, reason } => {
            claim::decide_withdraw(view, id, reason, now)
        }
        CommandBody::Claim {
            generation,
            holder,
            token_hash,
        } => claim::decide_claim(view, *generation, holder, token_hash, now),
        CommandBody::ClearUnit {
            unit_id,
            to_stage,
            generation,
            request_id,
        } => claim::decide_clear(
            view,
            workflow,
            unit_id,
            to_stage.as_deref(),
            *generation,
            request_id,
            now,
        ),
        CommandBody::OpenMergeAck {
            unit_id,
            reason,
            facts,
        } => claim::decide_open_merge_ack(view, workflow, unit_id, reason, facts, now),
        CommandBody::AckRequest {
            id,
            revision,
            generation,
            head_sha,
        } => claim::decide_ack(view, workflow, id, *revision, *generation, head_sha, now),
        CommandBody::AckRefused {
            id,
            revision,
            generation,
            cause,
            ..
        } => claim::decide_ack_refused(view, id, *revision, *generation, cause),
    }
}

/// A push is recorded for any unit of the run, in any state.
fn push_effect(
    view: &RunView,
    unit_id: &str,
    repo: &str,
    branch: &str,
    sha: &str,
    outcome: &PushOutcome,
) -> Result<Decision, Rejected> {
    view.node(unit_id)
        .filter(|n| n.kind == NodeKind::Unit)
        .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    let how = match outcome {
        PushOutcome::Pushed => "pushed".to_string(),
        PushOutcome::Failed(why) => format!("failed: {why}"),
    };
    d.ledger = Some(format!(
        "push: repo={repo} branch={branch} sha={sha} outcome={how}"
    ));
    Ok(d)
}

fn admit_unit(
    view: &RunView,
    workflow: &Workflow,
    run_id: &str,
    unit: &crate::port::unit_source::UnitRecord,
    track: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    unit.check().map_err(|e| e.to_string())?;
    if workflow.track(track).is_none() {
        return Err(format!(
            "unit {} names track {track:?}, which the workflow does not define",
            unit.id
        ));
    }
    if view.node(&unit.id).is_some() {
        return Err(format!(
            "unit {} is already admitted under another command",
            unit.id
        ));
    }
    let ingest_seq = u32::try_from(view.units().count() + 1).unwrap_or(u32::MAX);
    // A first stage that only an outcome enters is passed over, as any later one is.
    let first = workflow.track(track).map_or(0, |t| t.next_stage_index(0));
    let detail = UnitDetail {
        record: unit.clone(),
        track: track.to_string(),
        ingest_seq,
        stages_done: u32::try_from(first).unwrap_or(u32::MAX),
        pr: None,
        park_generation: 0,
        visits: std::collections::BTreeMap::new(),
    };
    let mut d = Decision::admitted();
    d.unit_id = Some(unit.id.clone());
    d.effects.push(Effect::PutNode(NodeRow {
        run_id: run_id.to_string(),
        id: unit.id.clone(),
        parent_id: None,
        kind: NodeKind::Unit,
        status: Status::Running,
        reason: None,
        role: None,
        model: None,
        attempt: 0,
        detail: detail.to_text(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
    }));
    Ok(d)
}

pub(crate) fn unknown_track(unit_id: &str, track: &str) -> String {
    format!("unit {unit_id} is on track {track:?}, which the workflow does not define")
}

fn attach(
    view: &RunView,
    session_id: &str,
    process_ref: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    let session = view
        .session(session_id)
        .ok_or_else(|| format!("session {session_id} is unknown"))?;
    if session.ended {
        return Err(format!("session {session_id} has ended"));
    }
    if let Some(existing) = &session.process_ref
        && existing != process_ref
    {
        return Err(format!("session {session_id} has another process"));
    }
    let mut s = session.clone();
    s.process_ref = Some(process_ref.to_string());
    s.updated_at = now.to_string();
    let mut d = Decision::admitted();
    d.unit_id = Some(s.unit_id.clone());
    d.effects.push(Effect::PutSession(s));
    Ok(d)
}

fn running_unit<'a>(view: &'a RunView, unit_id: &str) -> Result<&'a NodeRow, Rejected> {
    let unit = view
        .node(unit_id)
        .filter(|n| n.kind == NodeKind::Unit)
        .ok_or_else(|| format!("unit {unit_id} is unknown"))?;
    if unit.status != Status::Running {
        return Err(format!(
            "unit {unit_id} is {}, not running",
            unit.status.as_str()
        ));
    }
    Ok(unit)
}

/// What ends a stage with a signal: the stage ends `done`, and the unit goes on to the next stage
/// in order (`outcome` is `None`) or the stage's outcome entry decides where it goes.
#[allow(clippy::too_many_arguments)]
fn signal_stage(
    view: &RunView,
    workflow: &Workflow,
    unit_id: &str,
    stage: &str,
    signal: &Signal,
    pr: Option<crate::port::forge::PrRef>,
    outcome: Option<u32>,
    now: &str,
) -> Result<Decision, Rejected> {
    let unit = running_unit(view, unit_id)?;
    let stage_node = view
        .stages_of(unit_id)
        .into_iter()
        .rfind(|n| n.detail == stage)
        .filter(|n| n.status == Status::Running)
        .ok_or_else(|| format!("unit {unit_id} has no running stage {stage}"))?;
    let mut detail = view.unit_detail(unit)?;
    let track = workflow
        .track(&detail.track)
        .ok_or_else(|| unknown_track(unit_id, &detail.track))?;
    if outcome.is_some() && !signal.is_success() {
        return Err(format!(
            "an outcome of stage {stage} needs a signal that succeeded, not {}",
            signal.describe()
        ));
    }
    if pr.is_some() {
        detail.pr = pr;
    }

    let mut stage_end = stage_node.clone();
    stage_end.reason = Some(signal.describe());
    stage_end.updated_at = now.to_string();
    let mut unit_next = unit.clone();
    unit_next.updated_at = now.to_string();
    let halted = view.is_halted();

    // The unit stands at the stage that ended: `stages_done` is its place in the track.
    let place = track
        .stage_index(stage)
        .unwrap_or(detail.stages_done as usize);
    // Set when the entry is at its cap: the unit parks with this reason.
    let mut cap_park: Option<String> = None;
    let text;
    if !signal.is_success() {
        stage_end.status = Status::Failed;
        unit_next.status = Status::Failed;
        unit_next.reason = Some(format!("stage {stage} failed: {}", signal.describe()));
        text = format!("stage {stage} failed ({}); unit failed", signal.describe());
    } else if let Some(entry) = outcome {
        stage_end.status = Status::Done;
        let entry = entry as usize;
        let def = track
            .stages
            .get(place)
            .and_then(|s| s.outcomes.get(entry).map(|o| (s, o)))
            .ok_or_else(|| format!("stage {stage} of unit {unit_id} has no outcome {entry}"))?;
        let (_, target) = def;
        let to = track.stage_index(&target.to).ok_or_else(|| {
            format!(
                "outcome {entry} of stage {stage} names {}, which track {} does not define",
                target.to, track.name
            )
        })?;
        let key = crate::unit::visit_key(stage, entry);
        let visit = detail
            .visits
            .get(&key)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if visit > target.max {
            let reason = format!("{stage}: {} cap {} reached", target.to, target.max);
            text = format!(
                "stage {stage} done ({}); {reason}; unit parked",
                signal.describe()
            );
            cap_park = Some(reason);
        } else {
            detail.visits.insert(key, visit);
            detail.stages_done = u32::try_from(to).unwrap_or(u32::MAX);
            unit_next.reason = halted.then(|| crate::status::REASON_RUN_HALTED.to_string());
            text = format!(
                "stage {stage} done ({}); outcome {} sends the unit to {} (visit {visit} of {})",
                signal.describe(),
                entry + 1,
                target.to,
                target.max
            );
        }
    } else {
        stage_end.status = Status::Done;
        // A stage that only an outcome enters is passed over.
        let next = track.next_stage_index(place + 1);
        detail.stages_done = u32::try_from(next).unwrap_or(u32::MAX);
        let all_done = next >= track.stages.len();
        if all_done && track.merge.is_none() {
            unit_next.status = Status::Done;
            unit_next.reason = Some("all stages done".to_string());
            text = format!("stage {stage} done ({}); unit done", signal.describe());
        } else {
            unit_next.reason = halted.then(|| crate::status::REASON_RUN_HALTED.to_string());
            text = format!("stage {stage} done ({})", signal.describe());
        }
    }
    unit_next.detail = detail.to_text();

    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    if let Some(session) = view.session(&stage_node.id) {
        let mut s = session.clone();
        s.ended = true;
        s.updated_at = now.to_string();
        d.effects.push(Effect::PutSession(s));
    }
    d.effects.push(Effect::PutNode(stage_end));
    match cap_park {
        // The stage is over, and the unit parks in the same commit with its request (ADR-0006).
        Some(reason) => {
            let reason = check_reason(&reason).map_err(|e| e.to_string())?;
            d.effects.extend(claim::park_effects(
                view,
                workflow,
                &unit_next,
                RequestKind::Park,
                reason,
                None,
                now,
            )?);
        }
        None => d.effects.push(Effect::PutNode(unit_next)),
    }
    d.ledger = Some(text);
    Ok(d)
}

fn end_node(
    view: &RunView,
    node_id: &str,
    status: EndStatus,
    reason: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    let reason = check_reason(reason).map_err(|e| e.to_string())?;
    let node = view
        .node(node_id)
        .ok_or_else(|| format!("node {node_id} is unknown"))?;
    if node.status != Status::Running {
        return Err(format!(
            "node {node_id} is {}, not running",
            node.status.as_str()
        ));
    }
    let mut n = node.clone();
    n.status = status.status();
    n.reason = Some(reason.to_string());
    n.updated_at = now.to_string();
    let mut d = Decision::admitted();
    d.unit_id = Some(node.parent_id.clone().unwrap_or_else(|| node.id.clone()));
    if node.kind == NodeKind::Stage
        && let Some(session) = view.session(node_id)
    {
        let mut s = session.clone();
        s.ended = true;
        s.updated_at = now.to_string();
        d.effects.push(Effect::PutSession(s));
    }
    d.effects.push(Effect::PutNode(n));
    d.ledger = Some(format!(
        "node {node_id} {}: {reason}",
        status.status().as_str()
    ));
    Ok(d)
}

/// Park a unit that is `running`. The same commit opens the unit's request (ADR-0006).
fn park_unit(
    view: &RunView,
    workflow: &Workflow,
    unit_id: &str,
    kind: RequestKind,
    reason: &str,
    detail: Option<&serde_json::Value>,
    now: &str,
) -> Result<Decision, Rejected> {
    let reason = check_reason(reason).map_err(|e| e.to_string())?;
    let unit = running_unit(view, unit_id)?;
    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    d.effects = claim::park_effects(view, workflow, unit, kind, reason, detail, now)?;
    d.ledger = Some(format!("unit {unit_id} parked: {reason}"));
    Ok(d)
}
