//! The `run_list` verb (`r1.s4.w3`, W4): every run the controller holds, for the Needs you page.
//!
//! It is a read. It takes no lock of the driver, writes no `controller.ledger` line, admits nothing
//! and touches no row of the store; the request ledger's one `api.ledger` line is its only trace.
//! A run shows eight fields and no unit, session or cost (`pf_api::types::RunSummary`).

use pf_api::types::{RunList, RunSummary, SCHEMA_VERSION};
use pf_engine::admission::{CommandBody, body_of_content};
use pf_engine::port::store::{RequestStatus, RunState, Store};
use pf_ipc::Response;
use serde_json::Value;

use super::{Controller, RunHandle};

impl Controller {
    pub(super) fn verb_run_list(&self) -> Response {
        let runs: Vec<RunSummary> = self
            .handles()
            .iter()
            .map(|handle| self.run_summary(handle))
            .collect();
        let list = RunList {
            schema_version: SCHEMA_VERSION,
            runs,
        };
        Response::ok(serde_json::to_value(list).unwrap_or(Value::Null))
    }

    /// One run, as the store and its driver say it now. A run the store cannot read is `active` with
    /// no reason, as `runs` shows it.
    fn run_summary(&self, handle: &RunHandle) -> RunSummary {
        let id = &handle.info.run_id;
        let row = self.store.run(id).ok().flatten();
        let ended = row.as_ref().is_some_and(|r| r.state == RunState::Ended);
        // The reason of the last `EndRun`, as `status` reads it.
        let end_reason = self
            .store
            .admissions(id)
            .unwrap_or_default()
            .iter()
            .rev()
            .find_map(|a| match body_of_content(&a.content)? {
                CommandBody::EndRun { reason } => Some(reason),
                _ => None,
            });
        let open_requests = self
            .store
            .requests(id)
            .unwrap_or_default()
            .iter()
            .filter(|r| r.status == RequestStatus::Open)
            .count() as u64;
        RunSummary {
            id: id.clone(),
            spine: handle.info.spine.clone(),
            workflow: handle.info.workflow.clone(),
            state: if ended { "ended" } else { "active" }.to_string(),
            halted_reason: row.and_then(|r| r.halted_reason),
            end_reason,
            open_requests,
            waiting: handle.driver.waiting(),
        }
    }
}
