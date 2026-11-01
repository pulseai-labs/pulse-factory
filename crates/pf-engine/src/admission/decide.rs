//! The decision for one command: what it writes, and whether it is admitted or refused.
//!
//! `decide` is a pure function of the stored state. A command that is not valid in that state
//! returns `Err` and records nothing. A command the run refuses on purpose, such as a launch after
//! `halt`, returns a `Refused` decision, and the store records the refusal with its reason.

use crate::launch;
use crate::merge;
use crate::port::store::{AdmissionOutcome, Effect, NodeKind, NodeRow, RunRow, RunState};
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
        } => signal_stage(view, workflow, unit_id, stage, signal, pr.clone(), now),
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
        CommandBody::Park { unit_id, reason } => park_unit(view, unit_id, reason, now),
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
        CommandBody::EndRun { reason } => {
            let reason = check_reason(reason).map_err(|e| e.to_string())?;
            let mut r = run.clone();
            r.state = RunState::Ended;
            r.updated_at = now.to_string();
            let mut d = Decision::admitted();
            d.effects.push(Effect::PutRun(r));
            d.ledger = Some(format!("run ended: {reason}"));
            Ok(d)
        }
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
    let detail = UnitDetail {
        record: unit.clone(),
        track: track.to_string(),
        ingest_seq,
        stages_done: 0,
        pr: None,
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

fn signal_stage(
    view: &RunView,
    workflow: &Workflow,
    unit_id: &str,
    stage: &str,
    signal: &Signal,
    pr: Option<crate::port::forge::PrRef>,
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
    if pr.is_some() {
        detail.pr = pr;
    }

    let mut stage_end = stage_node.clone();
    stage_end.reason = Some(signal.describe());
    stage_end.updated_at = now.to_string();
    let mut unit_next = unit.clone();
    unit_next.updated_at = now.to_string();
    let halted = view.is_halted();

    let text;
    if signal.is_success() {
        stage_end.status = Status::Done;
        detail.stages_done += 1;
        let all_done = detail.stages_done as usize >= track.stages.len();
        if all_done && track.merge.is_none() {
            unit_next.status = Status::Done;
            unit_next.reason = Some("all stages done".to_string());
            text = format!("stage {stage} done ({}); unit done", signal.describe());
        } else {
            unit_next.reason = halted.then(|| crate::status::REASON_RUN_HALTED.to_string());
            text = format!("stage {stage} done ({})", signal.describe());
        }
    } else {
        stage_end.status = Status::Failed;
        unit_next.status = Status::Failed;
        unit_next.reason = Some(format!("stage {stage} failed: {}", signal.describe()));
        text = format!("stage {stage} failed ({}); unit failed", signal.describe());
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
    d.effects.push(Effect::PutNode(unit_next));
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

fn park_unit(view: &RunView, unit_id: &str, reason: &str, now: &str) -> Result<Decision, Rejected> {
    let reason = check_reason(reason).map_err(|e| e.to_string())?;
    let unit = running_unit(view, unit_id)?;
    let mut u = unit.clone();
    u.status = Status::Parked;
    u.reason = Some(reason.to_string());
    u.updated_at = now.to_string();
    let mut d = Decision::admitted();
    d.unit_id = Some(unit_id.to_string());
    d.effects.push(Effect::PutNode(u));
    d.ledger = Some(format!("unit {unit_id} parked: {reason}"));
    Ok(d)
}
