//! Helpers of the notice tests (`r2.s11.w3`). It is included with `#[path]` and is no module of
//! `common`, so a file here never gets a test crate of its own. It reads the store and the
//! controller ledger and writes nothing but the run's project link and the settings the tests set.
#![allow(dead_code)]

use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, DeliveryRow, Effect, NewAdmission, NoticeRow, Store,
};
use serde_json::{Value, json};

use crate::common::chat::{cli_json, controller_ledger};
use crate::common::ctl::Live;

/// Every notice of the store, oldest first.
pub fn notices(live: &Live) -> Vec<NoticeRow> {
    live.controller
        .store()
        .notices_after(0, 10_000)
        .expect("read the notices")
}

/// The delivery of a notice on a channel.
pub fn delivery(live: &Live, position: u64, channel: &str) -> DeliveryRow {
    live.controller
        .store()
        .notice_deliveries(position)
        .expect("read the deliveries")
        .into_iter()
        .find(|d| d.channel == channel)
        .unwrap_or_else(|| panic!("notice {position} has no {channel} delivery"))
}

/// The text of a notice's delivery on a channel.
pub fn text_of(live: &Live, position: u64, channel: &str) -> String {
    delivery(live, position, channel).text
}

/// How a delivery stands: `waiting`, or `superseded:<why>`.
pub fn state_of(live: &Live, position: u64, channel: &str) -> String {
    let d = delivery(live, position, channel);
    match d.why {
        Some(why) => format!("{}:{why}", d.state.as_str()),
        None => d.state.as_str().to_string(),
    }
}

/// The lines of the controller ledger that name a command kind, as `<kind> <rest>`.
pub fn ledger(live: &Live, kind: &str) -> Vec<String> {
    controller_ledger(live)
        .into_iter()
        .filter_map(|l| {
            let at = l.find(&format!(" {kind} "))?;
            Some(l[at + 1..].to_string())
        })
        .collect()
}

/// The open request of a unit.
pub fn request_of(live: &Live, unit: &str) -> (String, u32) {
    let view = live.handle().engine.view().unwrap();
    let r = view
        .open_request_of(unit)
        .unwrap_or_else(|| panic!("no open request for {unit}"));
    (r.id.clone(), r.revision)
}

/// Link a run to a project, as the proposal of a start card does.
pub fn link(live: &Live, run: &str, project: &str) {
    let committed = live
        .controller
        .store()
        .commit(Commit {
            run_id: CONTROLLER_SCOPE.to_string(),
            command_id: format!("run-project-{run}"),
            content: json!({ "run": run, "project": project }).to_string(),
            admission: NewAdmission {
                kind: "run_project".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: Some(run.to_string()),
                role: None,
                model: None,
                attempt: None,
                admitted_at: "2026-11-21T00:00:00.000Z".to_string(),
            },
            effects: vec![Effect::SetRunProject {
                run_id: run.to_string(),
                project_id: project.to_string(),
            }],
            ledger: None,
        })
        .unwrap();
    assert!(
        matches!(committed, pf_engine::port::store::Committed::Applied { .. }),
        "{committed:?}"
    );
}

/// `settings set`, answering the stored record.
pub fn set(live: &Live, key: &str, value: &str, scope: &[&str]) -> Value {
    let mut args = vec!["settings", "set", key, value];
    args.extend_from_slice(scope);
    cli_json(live, &args)
}

/// The open requests as `requests --json` shows them.
pub fn requests_json(live: &Live) -> Vec<Value> {
    let v = cli_json(live, &["requests"]);
    v["requests"].as_array().cloned().unwrap_or_default()
}

/// Ticks of the controller.
pub fn ticks(live: &Live, n: usize) {
    for _ in 0..n {
        live.controller.tick_all();
    }
}
