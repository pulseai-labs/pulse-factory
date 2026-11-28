//! Reconcile at start-up (ADR-0007), after ledger recovery. This is tier 1.
//!
//! Every unit in `running`, which includes a unit that waits on a seat's next signal, has its open
//! session rebound: the agent port's `rebind` says whether the session's host survived, is lost
//! or cannot be reached.
//!
//! - **Survived:** the unit stays running, and no request opens.
//! - **Lost:** the unit becomes `parked`, with the reason "seat lost across a controller
//!   restart", and a `seat_lost` request opens in the same commit. The session ends.
//! - **Unreachable:** the same, with the reason "host unreachable: <why>". Nothing is relaunched,
//!   so no second host starts for the seat.
//!
//! An adapter with no host answers `Lost` with the cause "no session host". Then `is_alive`
//! decides, as it did before hosts: a live process stays running, and a gone one parks the unit.
//! A live session, and a unit already done, failed, stopped or parked, is left alone. Nothing is
//! relaunched: the claim decides each relaunch (ADR-0007), with `clear`.

use crate::admission::{Command, CommandBody, EngineError};
use crate::engine::Engine;
use crate::port::agent::{NO_SESSION_HOST, RebindOutcome, SessionRef};
use crate::port::store::{Cause, RequestKind};
use crate::status::{EndStatus, REASON_HOST_UNREACHABLE, REASON_SEAT_LOST, Status};

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
        let mut report = ReconcileReport::default();
        // A settle whose effect never stored its result is closed first: its effect is never run a
        // second time, and the unit parks again.
        for unit_id in self.close_pending_settles()? {
            report.parked.push(unit_id);
        }
        let view = self.view()?;
        for unit in view.units().filter(|u| u.status == Status::Running) {
            let Some(session) = view
                .sessions
                .iter()
                .rfind(|s| s.unit_id == unit.id && !s.ended)
            else {
                continue;
            };
            let verdict = match &session.process_ref {
                // A launch that never got a process handle counts as gone.
                None => Some((Cause::SeatLost, REASON_SEAT_LOST.to_string())),
                Some(handle) => self.tier1_verdict(&unit.id, &SessionRef(handle.clone()))?,
            };
            let Some((cause, reason)) = verdict else {
                report.alive.push(unit.id.clone());
                continue;
            };
            // Park first: a stop between the two leaves a parked unit, never a running unit with
            // no session.
            self.park_as(&unit.id, RequestKind::SeatLost, cause, &reason)?;
            self.end(&session.id, EndStatus::Failed, &reason)?;
            report.parked.push(unit.id.clone());
        }
        Ok(report)
    }

    /// Store `unknown` as the result of every pending settle of the run. Returns the units that this
    /// parked.
    fn close_pending_settles(&self) -> Result<Vec<String>, EngineError> {
        let view = self.view()?;
        let mut parked = Vec::new();
        let pending: Vec<_> = view
            .units()
            .filter_map(|u| {
                crate::claim::pending_settle(&view, &u.id)
                    .map(|p| (u.id.clone(), u.status == Status::Running, p))
            })
            .collect();
        for (unit_id, was_running, p) in pending {
            self.submit(Command {
                id: crate::claim::result_command_id(&p.request_id, p.revision),
                body: CommandBody::SettleResult {
                    request_id: p.request_id.clone(),
                    revision: p.revision,
                    outcome: crate::claim::SettleOutcome::Unknown,
                    detail: "the controller stopped before the result was stored".to_string(),
                    process_ref: None,
                },
            })?;
            if was_running {
                parked.push(unit_id);
            }
        }
        Ok(parked)
    }

    /// `None` when the session lives. Otherwise the cause and the reason the unit parks with.
    fn tier1_verdict(
        &self,
        unit_id: &str,
        session: &SessionRef,
    ) -> Result<Option<(Cause, String)>, EngineError> {
        let cannot_tell = |e: &dyn std::fmt::Display| {
            EngineError::Port(format!(
                "cannot tell whether the session of unit {unit_id} is alive: {e}"
            ))
        };
        match self.agent.rebind(session).map_err(|e| cannot_tell(&e))? {
            RebindOutcome::Survived => Ok(None),
            RebindOutcome::Unreachable { why } => Ok(Some((
                Cause::HostUnreachable,
                format!("{REASON_HOST_UNREACHABLE}{why}"),
            ))),
            // The adapter runs no host: its own liveness check decides.
            RebindOutcome::Lost { cause } if cause == NO_SESSION_HOST => {
                let alive = self.agent.is_alive(session).map_err(|e| cannot_tell(&e))?;
                Ok((!alive).then(|| (Cause::SeatLost, REASON_SEAT_LOST.to_string())))
            }
            RebindOutcome::Lost { .. } => Ok(Some((Cause::SeatLost, REASON_SEAT_LOST.to_string()))),
        }
    }
}
