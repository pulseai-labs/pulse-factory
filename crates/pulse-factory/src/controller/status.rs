//! The `status` answer: what the store says about one run. It reads the store and takes no lock
//! of the driver, so it answers while a tick runs.

use std::sync::atomic::Ordering;

use pf_engine::admission::{CommandBody, body_of_content};
use pf_engine::port::store::{NodeKind, Store};
use pf_engine::unit::UnitDetail;
use serde_json::{Value, json};

use super::{Controller, RunHandle};
use crate::format::dollars;

pub const SCHEMA_VERSION: u32 = 1;

pub fn build(c: &Controller, h: &RunHandle) -> Value {
    let id = &h.info.run_id;
    let run = c.store.run(id).ok().flatten();
    let nodes = c.store.nodes(id).unwrap_or_default();
    let sessions = c.store.sessions(id).unwrap_or_default();
    let admissions = c.store.admissions(id).unwrap_or_default();
    let end_reason = admissions
        .iter()
        .rev()
        .find_map(|a| match body_of_content(&a.content)? {
            CommandBody::EndRun { reason } => Some(reason),
            _ => None,
        });

    let stage_of = |node_id: &str| -> Option<(String, String, String)> {
        let n = nodes.iter().find(|n| n.id == node_id)?;
        Some((
            n.detail.clone(),
            n.role.clone().unwrap_or_default(),
            n.model.clone().unwrap_or_default(),
        ))
    };
    let units: Vec<Value> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Unit)
        .map(|u| {
            let detail = UnitDetail::from_text(&u.detail).ok();
            let stages: Vec<&pf_engine::port::store::NodeRow> = nodes
                .iter()
                .filter(|n| {
                    n.kind == NodeKind::Stage && n.parent_id.as_deref() == Some(u.id.as_str())
                })
                .collect();
            let current = stages.last().map(|s| s.detail.clone()).unwrap_or_default();
            json!({
                "id": u.id,
                "track": detail.as_ref().map(|d| d.track.clone()).unwrap_or_default(),
                "status": u.status.as_str(),
                "stage": current,
                "stages_done": detail.as_ref().map_or(0, |d| d.stages_done),
                "reason": u.reason,
                "pr": detail.and_then(|d| d.pr).map(|p| format!("{}#{}", p.repo, p.number)),
            })
        })
        .collect();

    let mut total = [0u64; 5];
    let session_rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            total[0] = total[0].saturating_add(s.input_tokens);
            total[1] = total[1].saturating_add(s.output_tokens);
            total[2] = total[2].saturating_add(s.cache_read_tokens);
            total[3] = total[3].saturating_add(s.cache_write_tokens);
            total[4] = total[4].saturating_add(s.cost_micros);
            let node = nodes.iter().find(|n| n.id == s.node_id);
            let (stage, role, model) = stage_of(&s.node_id).unwrap_or_default();
            json!({
                "id": s.id,
                "unit": s.unit_id,
                "stage": stage,
                "role": role,
                "model": model,
                "runtime": runtime_of(h, &role),
                "status": node.map_or("unknown", |n| n.status.as_str()),
                "ended": s.ended,
                "tokens": {
                    "input": s.input_tokens,
                    "output": s.output_tokens,
                    "cache_read": s.cache_read_tokens,
                    "cache_write": s.cache_write_tokens,
                },
                "cost_micros": s.cost_micros,
                "cost": dollars(s.cost_micros),
            })
        })
        .collect();

    json!({
        "schema_version": SCHEMA_VERSION,
        "run": h.info.run_id,
        "spine": h.info.spine,
        "workflow": h.info.workflow,
        "env": h.info.env,
        "state": run.as_ref().map_or("unknown", |r| r.state.as_str()),
        "driven": h.driven.load(Ordering::SeqCst),
        "halted_reason": run.as_ref().and_then(|r| r.halted_reason.clone()),
        "degraded": h.engine.degraded(),
        "end_reason": end_reason,
        "notes": h.driver.notes(),
        "units": units,
        "sessions": session_rows,
        "total": {
            "tokens": {
                "input": total[0], "output": total[1], "cache_read": total[2], "cache_write": total[3],
            },
            "cost_micros": total[4],
            "cost": dollars(total[4]),
        },
    })
}

fn runtime_of(h: &RunHandle, role: &str) -> String {
    h.engine
        .workflow()
        .tracks
        .iter()
        .flat_map(|t| t.stages.iter())
        .find(|s| s.role == role)
        .map(|s| s.runtime.clone())
        .unwrap_or_default()
}
