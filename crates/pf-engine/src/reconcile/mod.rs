//! Reconcile at start-up (ADR-0007), after ledger recovery.
//!
//! Every unit in `running`, which includes a unit that waits on a seat's next signal, whose
//! session process the agent port reports as gone becomes `parked`, with the reason "seat lost
//! across a controller restart". A live session, and a unit already done, failed, stopped or
//! parked, is left alone. Nothing is relaunched. (ADR-0006 also opens a request here; requests
//! come in a later item.)

use crate::admission::EngineError;
use crate::engine::Engine;
use crate::port::agent::SessionRef;
use crate::status::{EndStatus, REASON_SEAT_LOST, Status};

/// What `reconcile` found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Units parked in this pass.
    pub parked: Vec<String>,
    /// Running units whose session is alive.
    pub alive: Vec<String>,
}

impl Engine {
    /// Park every running unit whose session process is gone. Launch nothing.
    ///
    /// A session whose launch was admitted but never got a process handle counts as gone.
    /// A running unit with no open session, such as one that waits for its next launch, is left alone.
    /// When the agent port cannot say whether a session is alive, the call fails and parks nothing
    /// for that unit.
    pub fn reconcile(&self) -> Result<ReconcileReport, EngineError> {
        let view = self.view()?;
        let mut report = ReconcileReport::default();
        for unit in view.units().filter(|u| u.status == Status::Running) {
            let Some(session) = view
                .sessions
                .iter()
                .rfind(|s| s.unit_id == unit.id && !s.ended)
            else {
                continue;
            };
            let alive = match &session.process_ref {
                None => false,
                Some(handle) => self
                    .agent
                    .is_alive(&SessionRef(handle.clone()))
                    .map_err(|e| {
                        EngineError::Port(format!(
                            "cannot tell whether the session of unit {} is alive: {e}",
                            unit.id
                        ))
                    })?,
            };
            if alive {
                report.alive.push(unit.id.clone());
                continue;
            }
            // Park first: a stop between the two leaves a parked unit, never a running unit with
            // no session.
            self.park(&unit.id, REASON_SEAT_LOST)?;
            self.end(&session.id, EndStatus::Failed, REASON_SEAT_LOST)?;
            report.parked.push(unit.id.clone());
        }
        Ok(report)
    }
}
