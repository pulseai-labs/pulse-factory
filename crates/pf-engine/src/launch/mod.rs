//! Launch admission and the `halt` command.
//!
//! A launch is admitted as a command. Each admitted launch writes one record with run, unit,
//! role, model and attempt. After `halt`, a launch command is refused with a recorded reason.
//!
//! There are two meanings of "halt". The `halt` command is the operator's kill switch on a run.
//! A halt condition (ADR-0006) is what stops one unit, and it parks that unit.
//!
//! The `halt` command stops two things, because both are irreversible (ADR-0005): the next
//! launch and the next merge. A unit whose next step is one of them keeps its status and gains
//! the reason "run halted". It is not parked. Every other command is still admitted, so a
//! running unit's signals, its end status, its usage and cost and its ledger lines are recorded
//! until it ends. `halt` never stops a running session.

use crate::admission::{Command, CommandBody, Decision, EngineError, RunView};
use crate::engine::Engine;
use crate::port::agent::{AgentError, LaunchPlan};
use crate::port::store::{AdmissionOutcome, Effect, NodeKind, NodeRow};
use crate::status::{EndStatus, REASON_RUN_HALTED, Status, check_reason};
use crate::unit::{UnitDetail, stage_node_id};
use crate::workflow::Workflow;

type Rejected = String;

/// Does this unit wait for a launch or a merge, with no stage running?
pub(crate) fn next_step_is_launch_or_merge(
    view: &RunView,
    workflow: &Workflow,
    unit: &NodeRow,
) -> bool {
    if unit.kind != NodeKind::Unit || unit.status != Status::Running {
        return false;
    }
    if view.running_stage_of(&unit.id).is_some() {
        return false;
    }
    let Ok(detail) = UnitDetail::from_text(&unit.detail) else {
        return false;
    };
    let stages = workflow.stages.len();
    let done = detail.stages_done as usize;
    done < stages || (done == stages && workflow.merge.is_some())
}

/// A copy of the unit that carries the "run halted" reason, or `None` when it has it already.
pub(crate) fn with_halt_reason(unit: &NodeRow, now: &str) -> Option<NodeRow> {
    if unit.reason.as_deref() == Some(REASON_RUN_HALTED) {
        return None;
    }
    let mut u = unit.clone();
    u.reason = Some(REASON_RUN_HALTED.to_string());
    u.updated_at = now.to_string();
    Some(u)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_launch(
    view: &RunView,
    workflow: &Workflow,
    unit_id: &str,
    stage: &str,
    role: &str,
    model: &str,
    attempt: u32,
    now: &str,
) -> Result<Decision, Rejected> {
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
    let detail = view.unit_detail(unit)?;
    let def = workflow
        .stages
        .get(detail.stages_done as usize)
        .ok_or_else(|| format!("unit {unit_id} has no stage left to launch"))?;
    if def.name != stage {
        return Err(format!(
            "the next stage of unit {unit_id} is {}, not {stage}",
            def.name
        ));
    }
    if def.role != role || def.model != model {
        return Err(format!(
            "stage {stage} runs role {} on model {}",
            def.role, def.model
        ));
    }
    if view.running_stage_of(unit_id).is_some() {
        return Err(format!("unit {unit_id} already has a running stage"));
    }
    let earlier = view
        .stages_of(unit_id)
        .into_iter()
        .filter(|n| n.detail == stage)
        .count();
    let expected = u32::try_from(earlier + 1).unwrap_or(u32::MAX);
    if attempt != expected {
        return Err(format!(
            "the next attempt of stage {stage} for unit {unit_id} is {expected}, not {attempt}"
        ));
    }

    let mut d = if view.is_halted() {
        let mut d = Decision::refused(REASON_RUN_HALTED);
        if let Some(u) = with_halt_reason(unit, now) {
            d.effects.push(Effect::PutNode(u));
        }
        d.ledger = Some(format!(
            "launch of stage {stage} for unit {unit_id} refused: {REASON_RUN_HALTED}"
        ));
        d
    } else {
        let node_id = stage_node_id(unit_id, stage, attempt);
        let run_id = unit.run_id.clone();
        let mut d = Decision::admitted();
        d.effects.push(Effect::PutNode(NodeRow {
            run_id: run_id.clone(),
            id: node_id.clone(),
            parent_id: Some(unit_id.to_string()),
            kind: NodeKind::Stage,
            status: Status::Running,
            reason: None,
            role: Some(role.to_string()),
            model: Some(model.to_string()),
            attempt,
            detail: stage.to_string(),
            created_at: now.to_string(),
            updated_at: now.to_string(),
        }));
        d.effects
            .push(Effect::PutSession(crate::port::store::SessionRow {
                run_id,
                id: node_id.clone(),
                node_id,
                unit_id: unit_id.to_string(),
                process_ref: None,
                ended: false,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_micros: 0,
                created_at: now.to_string(),
                updated_at: now.to_string(),
            }));
        if unit.reason.is_some() {
            let mut u = unit.clone();
            u.reason = None;
            u.updated_at = now.to_string();
            d.effects.push(Effect::PutNode(u));
        }
        d.ledger = Some(format!(
            "launch of stage {stage} for unit {unit_id} admitted: role {role}, model {model}, attempt {attempt}"
        ));
        d
    };
    d.unit_id = Some(unit_id.to_string());
    d.role = Some(role.to_string());
    d.model = Some(model.to_string());
    d.attempt = Some(attempt);
    Ok(d)
}

pub(crate) fn decide_halt(
    view: &RunView,
    workflow: &Workflow,
    reason: &str,
    now: &str,
) -> Result<Decision, Rejected> {
    let reason = check_reason(reason).map_err(|e| e.to_string())?;
    let run = view.run.as_ref().ok_or("the run is not open")?;
    let mut d = Decision::admitted();
    if run.halted_reason.is_none() {
        let mut r = run.clone();
        r.halted_reason = Some(reason.to_string());
        r.updated_at = now.to_string();
        d.effects.push(Effect::PutRun(r));
    }
    for unit in view.units() {
        if next_step_is_launch_or_merge(view, workflow, unit)
            && let Some(u) = with_halt_reason(unit, now)
        {
            d.effects.push(Effect::PutNode(u));
        }
    }
    d.ledger = Some(format!("run halted: {reason}"));
    Ok(d)
}

/// What `start_stage` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchStep {
    /// The launch was admitted and the adapter started the session.
    Launched {
        session_id: String,
        process_ref: String,
    },
    /// The run refused the launch and recorded the reason. After `halt` this is "run halted".
    Refused { reason: String },
    /// The adapter refused or failed the launch. The unit is parked with the reason.
    Parked { reason: String },
    /// The unit's stage is running already.
    AlreadyRunning,
    /// The unit has no stage left to launch.
    NothingToLaunch,
}

impl Engine {
    /// Launch the unit's next stage: admit the launch, then start the session through the agent
    /// port. A launch the adapter refuses or fails parks the unit through [`Engine::park`].
    pub fn start_stage(&self, unit_id: &str) -> Result<LaunchStep, EngineError> {
        let view = self.view()?;
        let unit = view
            .node(unit_id)
            .filter(|n| n.kind == NodeKind::Unit)
            .ok_or_else(|| EngineError::Rejected(format!("unit {unit_id} is unknown")))?;
        if unit.status != Status::Running {
            return Err(EngineError::Rejected(format!(
                "unit {unit_id} is {}, not running",
                unit.status.as_str()
            )));
        }
        let detail = view.unit_detail(unit).map_err(EngineError::Rejected)?;
        let Some(def) = self.cfg.workflow.stages.get(detail.stages_done as usize) else {
            return Ok(LaunchStep::NothingToLaunch);
        };
        if view.running_stage_of(unit_id).is_some() {
            return Ok(LaunchStep::AlreadyRunning);
        }
        let earlier = view
            .stages_of(unit_id)
            .into_iter()
            .filter(|n| n.detail == def.name)
            .count();
        let attempt = u32::try_from(earlier + 1).unwrap_or(u32::MAX);

        let admission = self.submit(Command {
            id: format!("launch:{unit_id}:{}:{attempt}", def.name),
            body: CommandBody::Launch {
                unit_id: unit_id.to_string(),
                stage: def.name.clone(),
                role: def.role.clone(),
                model: def.model.clone(),
                attempt,
            },
        })?;
        if admission.row.outcome == AdmissionOutcome::Refused {
            return Ok(LaunchStep::Refused {
                reason: admission.row.reason.unwrap_or_default(),
            });
        }

        let session_id = stage_node_id(unit_id, &def.name, attempt);
        let mut plan = LaunchPlan {
            run_id: self.cfg.run_id.clone(),
            unit_id: unit_id.to_string(),
            stage: def.name.clone(),
            role: def.role.clone(),
            model: def.model.clone(),
            attempt,
            settings: std::collections::BTreeMap::new(),
        };
        let started = self
            .agent
            .apply_rules(&mut plan, &def.rules)
            .and_then(|()| self.agent.remove_ask_tool(&mut plan))
            .and_then(|()| self.agent.launch(&plan));
        match started {
            Ok(session) => {
                self.submit(Command {
                    id: format!("attach:{session_id}"),
                    body: CommandBody::Attach {
                        session_id: session_id.clone(),
                        process_ref: session.0.clone(),
                    },
                })?;
                Ok(LaunchStep::Launched {
                    session_id,
                    process_ref: session.0,
                })
            }
            Err(e) => {
                let reason = match e {
                    AgentError::Refused(why) => format!("launch refused: {why}"),
                    AgentError::Failed(why) => format!("launch failed: {why}"),
                };
                // Park first: a stop between the two leaves a parked unit, never a running
                // unit with no session.
                self.park(unit_id, &reason)?;
                self.end(&session_id, EndStatus::Failed, &reason)?;
                Ok(LaunchStep::Parked { reason })
            }
        }
    }
}
