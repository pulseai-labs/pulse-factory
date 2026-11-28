//! AC-11 of `r2.s3.w3`: an act with an effect outside the store commits first and runs its effect
//! after (`docs/contracts/requests.md`, "Settle"; ADR-0005, amended). It holds for `rerun_check`
//! (the forge's re-run) and for the re-attach of `resume` (the adapter's rebind). A revoke that
//! commits before the settle commits refuses it, and no effect runs. Between the commit and the
//! result the settle is pending: no tick launches or merges its unit. A failed effect parks the unit
//! with its cause. A controller that stopped between the commit and the effect leaves a settle with no
//! result, and the next start stores `unknown`, parks the unit and runs no effect.
//!
//! The controller runs in the test process on the fake adapter, with a recording chat. The forge is
//! the bare forge of the paper env.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::chat::*;
use common::ctl::{RebindScript, TestPorts};
use common::*;
use pf_engine::Cause;
use pf_engine::port::agent::NO_SESSION_HOST;
use pf_engine::port::store::{
    AdmissionOutcome, Commit, Committed, Effect, NewAdmission, RequestKind, Store,
};
use pf_forge_bare::PrChecks;
use pulse_factory::controller::{Config, Controller};
use settle_rig::*;

/// One of the two acts with an effect, set up to its settle.
struct Case {
    rig: SRig,
    forge: Option<Forge>,
    act: &'static str,
    cause: &'static str,
}

fn rerun() -> Case {
    let rig = SRig::new(&[], Some(20));
    let forge = Forge::new(&rig);
    forge.item(&rig, "u1", 1, 5, PrChecks::Red);
    forge.finish_item(&rig, "u1", 5);
    Case {
        rig,
        forge: Some(forge),
        act: "rerun_check",
        cause: "merge_checks_red",
    }
}

fn reattach() -> Case {
    let rig = SRig::new(&[], Some(10));
    rig.live.queue_item("u1", 1);
    rig.tick();
    rig.tick();
    rig.live
        .handle()
        .engine
        .park_with(
            "u1",
            RequestKind::SeatLost,
            Cause::HostUnreachable,
            "host unreachable: no answer",
            None,
        )
        .unwrap();
    rig.tick();
    Case {
        rig,
        forge: None,
        act: "resume",
        cause: "host_unreachable",
    }
}

impl Case {
    fn each(f: impl Fn(Case)) {
        f(rerun());
        f(reattach());
    }

    fn request(&self) -> (String, u32) {
        assert_eq!(self.rig.status_of("u1"), "parked");
        self.rig.request("u1")
    }

    /// The effect has run: the forge's re-run left a line, or the adapter's `rebind` was called.
    fn effect_ran(&self) -> bool {
        match &self.forge {
            Some(f) => f.forge.reruns_path(5).exists(),
            None => !self.rig.live.ports.rebinds.calls.lock().unwrap().is_empty(),
        }
    }

    /// The effect will succeed.
    fn effect_ok(&self) {
        if self.forge.is_none() {
            *self.rig.live.ports.rebinds.script.lock().unwrap() =
                Some(RebindScript::Lost(NO_SESSION_HOST.to_string()));
            *self.rig.live.ports.rebinds.alive.lock().unwrap() = Some(true);
        }
    }

    /// The effect will fail.
    fn effect_fails(&self) {
        match &self.forge {
            Some(f) => f
                .forge
                .write_pr_with(5, "work/u1", SPINE_BRANCH, PrChecks::Red, &[], Some("boom"))
                .unwrap(),
            None => {
                *self.rig.live.ports.rebinds.script.lock().unwrap() =
                    Some(RebindScript::Unreachable("no hello".to_string()));
            }
        }
    }

    fn settle(&self) -> serde_json::Value {
        let (id, rev) = self.request();
        self.rig.settle(&id, rev, self.act, None, WHY)
    }

    /// Launches and merges that a tick could make for the unit: the count of admitted launches, and
    /// whether the PR merged.
    fn launches(&self) -> usize {
        self.rig.admissions("launch")
    }
}

fn revoke_in_the_gap(rig: &SRig) {
    let store = rig.live.controller.store().clone();
    let project = rig.project.clone();
    rig.live
        .controller
        .settle_faults()
        .set_after_grant(Some(Arc::new(move || {
            let committed = store
                .commit(Commit {
                    run_id: pf_engine::port::store::CONTROLLER_SCOPE.to_string(),
                    command_id: "revoke-in-the-gap".to_string(),
                    content: "{}".to_string(),
                    admission: NewAdmission {
                        kind: "grant_revoke".to_string(),
                        outcome: AdmissionOutcome::Admitted,
                        reason: None,
                        unit_id: None,
                        role: None,
                        model: None,
                        attempt: None,
                        admitted_at: "2026-11-28T00:00:00.000Z".to_string(),
                    },
                    effects: vec![Effect::RevokeClearGrant {
                        project_id: project.clone(),
                        generation: 1,
                        revoked_by: "cli".to_string(),
                        revoked_at: "2026-11-28T00:00:00.000Z".to_string(),
                    }],
                    ledger: None,
                })
                .unwrap();
            assert!(
                matches!(committed, Committed::Applied { .. }),
                "{committed:?}"
            );
        })));
}

/// Hold the verb between the commit and the effect. The test gets a signal when it holds, and lets it
/// go by sending on the second end.
fn hold_after_commit(rig: &SRig) -> (Receiver<()>, Sender<()>) {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    rig.live
        .controller
        .settle_faults()
        .set_after_commit(Some(Arc::new(move || {
            let _ = entered_tx.send(());
            let _ = release_rx.lock().unwrap().recv();
        })));
    (entered, release)
}

fn open_cause(rig: &SRig) -> String {
    let (id, _) = rig.request("u1");
    cli_json(&rig.live, &["requests", "--all"])["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap()["cause"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_revoke_that_commits_before_the_settle_refuses_it_and_no_effect_runs() {
    Case::each(|case| {
        revoke_in_the_gap(&case.rig);
        let (id, _) = case.request();
        let text = why(&case.settle());
        assert!(
            text.contains("holds no live clear grant"),
            "{}: {text}",
            case.act
        );
        assert_eq!(case.rig.admissions("settle"), 0, "{}", case.act);
        assert!(!case.effect_ran(), "{}: the effect ran", case.act);
        assert!(case.rig.results().is_empty());
        assert_eq!(case.rig.status_of("u1"), "parked");
        let stored = case
            .rig
            .live
            .controller
            .store()
            .request(&id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status.as_str(), "open");
    });
}

#[test]
fn a_settle_is_pending_between_the_commit_and_the_result_and_no_tick_launches_or_merges_its_unit() {
    Case::each(|case| {
        case.effect_ok();
        let (id, rev) = case.request();
        let args = settle_args(&id, rev, case.act, None, WHY);
        let (entered, release) = hold_after_commit(&case.rig);
        let (child, go) =
            case.rig
                .spawn_caller(true, &[], "settle", &args, "helper_calls_the_controller");
        std::fs::write(&go, "").unwrap();
        entered
            .recv_timeout(Duration::from_secs(20))
            .expect("the verb reached the pause");

        // The settle is in `admissions` with no `settle-result`, and the effect has not run.
        assert_eq!(case.rig.admissions("settle"), 1, "{}", case.act);
        assert!(case.rig.results().is_empty(), "{}", case.act);
        assert!(!case.effect_ran(), "{}", case.act);
        let pending = case
            .rig
            .live
            .handle()
            .engine
            .pending_settle_of("u1")
            .unwrap();
        assert!(pending.is_some(), "{}: the settle is pending", case.act);
        // No tick launches the unit or merges it meanwhile.
        let launches = case.launches();
        if let Some(f) = &case.forge {
            f.forge
                .write_pr(5, "work/u1", SPINE_BRANCH, PrChecks::Green)
                .unwrap();
        }
        case.rig.tick();
        case.rig.tick();
        // The tick skips the unit before it reaches the stage's `before` hook or the launch, so it
        // writes no note about either: the engine's hold is the second guard, not the first.
        let notes = case.rig.live.sb.status_json("")["notes"].to_string();
        assert!(
            !notes.contains("launch of u1") && !notes.contains("unit u1:"),
            "{}: a tick reached the hook or the launch of the pending unit: {notes}",
            case.act
        );
        assert_eq!(case.launches(), launches, "{}: a tick launched", case.act);
        assert_eq!(
            case.rig.status_of("u1"),
            "running",
            "{}: a tick merged or parked",
            case.act
        );
        assert!(!case.effect_ran());

        release.send(()).unwrap();
        done(&case.rig.finish_caller(child));
        assert_eq!(case.rig.results().len(), 1, "{}", case.act);
        assert!(case.effect_ran(), "{}", case.act);
        assert!(
            case.rig
                .live
                .handle()
                .engine
                .pending_settle_of("u1")
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn a_failed_effect_parks_the_unit_with_its_cause_and_writes_one_result_line() {
    Case::each(|case| {
        case.effect_fails();
        done(&case.settle());
        let results = case.rig.results();
        assert_eq!(results.len(), 1, "{}", case.act);
        assert_eq!(results[0].1["outcome"], "failed", "{}", case.act);
        assert_eq!(case.rig.status_of("u1"), "parked", "{}", case.act);
        assert_eq!(open_cause(&case.rig), case.cause, "{}", case.act);
        let lines = case.rig.run_ledger("settle-result");
        assert_eq!(lines.len(), 1, "{}: {lines:#?}", case.act);
        assert!(
            lines[0].contains(" failed") && lines[0].contains("parked"),
            "{lines:#?}"
        );
    });
}

#[test]
fn a_controller_that_stops_after_the_commit_leaves_a_result_the_next_start_stores_as_unknown() {
    Case::each(|case| {
        case.effect_ok();
        case.rig
            .live
            .controller
            .settle_faults()
            .skip_effect
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let (id, rev) = case.request();
        done(&case.rig.settle(&id, rev, case.act, None, WHY));
        // The verb answered after the commit, and the effect did not run.
        assert_eq!(case.rig.admissions("settle"), 1);
        assert!(case.rig.results().is_empty());
        assert!(!case.effect_ran(), "{}", case.act);

        // The unit's seat, which the re-attach would call `rebind` for.
        let process = case
            .rig
            .live
            .handle()
            .engine
            .view()
            .unwrap()
            .sessions
            .iter()
            .rfind(|s| s.unit_id == "u1" && s.process_ref.is_some())
            .and_then(|s| s.process_ref.clone())
            .unwrap();
        // The controller is opened again on the same state directory: a new adapter and forge.
        let fake = Arc::new(pf_adapter_fake::FakeAdapter::new());
        fake.add_plugin(
            "methodkit",
            "1.26.1",
            case.rig
                .live
                .sb
                .plugin_dir("methodkit", "1.26.1")
                .to_str()
                .unwrap(),
        );
        let ports = Arc::new(TestPorts {
            fake,
            plans: Default::default(),
            gate: Default::default(),
            pids: Default::default(),
            rebinds: Default::default(),
            block_run_json: Default::default(),
        });
        let second = Controller::open(
            Config::new(case.rig.live.sb.state.clone(), 3_600_000, BIN.into()),
            ports.clone(),
        )
        .unwrap();
        let handle = second.handles()[0].clone();
        let view = handle.engine.view().unwrap();
        assert_eq!(
            view.node("u1").unwrap().status.as_str(),
            "parked",
            "{}",
            case.act
        );
        let open = view.open_request_of("u1").unwrap();
        assert!(
            open.body.contains(case.cause),
            "{}: {}",
            case.act,
            open.body
        );
        let stored: Vec<_> = second
            .store()
            .admissions(&case.rig.run)
            .unwrap()
            .into_iter()
            .filter(|a| a.kind == "settle_result")
            .collect();
        assert_eq!(stored.len(), 1, "{}", case.act);
        assert!(
            stored[0].content.contains("\"outcome\":\"unknown\""),
            "{}",
            stored[0].content
        );
        assert_eq!(stored[0].command_id, format!("settle-result:{id}:{rev}"));
        let lines: Vec<String> = std::fs::read_to_string(&handle.info.ledger)
            .unwrap()
            .lines()
            .filter(|l| l.contains(" settle-result "))
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 1, "{}: {lines:#?}", case.act);
        assert!(
            lines[0].contains("unknown") && lines[0].contains("was not run again"),
            "{lines:#?}"
        );
        // No effect ran in either controller.
        assert!(!case.effect_ran(), "{}", case.act);
        // The start-up reconcile may rebind a seat that still runs, but never the settled unit's.
        assert!(
            !ports.rebinds.calls.lock().unwrap().contains(&process),
            "{}: the settled unit's seat was rebound",
            case.act
        );
    });
}
