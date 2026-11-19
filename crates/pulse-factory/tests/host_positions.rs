//! AC-4 of `r2.s2.w4`: the controller's host service keeps the position an adapter has read to
//! (ADR-0003's amendment of 2026-11-16, ADR-0011's kept-and-resent event test). `save_position`
//! commits one admission `host-position-<unit>-<seq>` under `@controller` and writes no ledger
//! line; `position` reads the last one; a controller opened again on the same state directory reads
//! it. A launch on a terminal records the `pty` transport and gives the child a terminal.
//!
//! The tests use the detached launcher in a temp runtime directory, a temp state directory (so a
//! server name of their own) and a guard that stops every host of the server when a test ends.
//! Nothing here starts a seat program, spends a token or reaches beyond the temp directories.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::BIN;
use common::ctl::Live;
use common::restart::host_bin;
use pf_engine::port::host::{HostError, HostHandle, HostLaunch, HostTransport, SessionHosts};
use pf_engine::port::store::{AdmissionOutcome, Store};
use pf_host::launcher::{Detached, HostState, Launcher};
use pulse_factory::controller::hosts::HostLauncher;
use pulse_factory::controller::{Config, Controller};
use serde_json::{Value, json};

const KEY: &str = "R-1/u1";

static N: AtomicUsize = AtomicUsize::new(0);

struct Rig {
    live: Live,
    rt: PathBuf,
    tmp: PathBuf,
}

fn tweak(cfg: &mut Config, rt: &std::path::Path) {
    cfg.runtime_dir = rt.to_path_buf();
    cfg.host_launcher = HostLauncher::Detached;
    cfg.host_bin = Some(host_bin());
}

impl Rig {
    fn new() -> Rig {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("pfhp-{}-{n}", std::process::id()));
        let rt = tmp.join("rt");
        std::fs::create_dir_all(&rt).unwrap();
        let runtime = rt.clone();
        let live = Live::start_config(BIN.into(), move |cfg| tweak(cfg, &runtime));
        Rig { live, rt, tmp }
    }

    fn hosts(&self) -> &std::sync::Arc<pulse_factory::controller::hosts::HostService> {
        self.live.controller.host_service()
    }

    fn state(&self) -> PathBuf {
        self.live.sb.state.clone()
    }

    /// A launch of `script` under `/bin/sh`, on a terminal of 30 rows and 100 columns.
    fn launch_terminal(&self, key: &str, script: &str) -> Result<HostHandle, HostError> {
        self.hosts().launch(HostLaunch {
            session_kind: "seat".to_string(),
            session_key: key.to_string(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.to_string()],
            cwd: self.tmp.display().to_string(),
            env: [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
            files: Vec::new(),
            secrets: Default::default(),
            transport: HostTransport::Terminal {
                rows: 30,
                cols: 100,
            },
            runtime: json!({"id": "stub"}),
            settings: json!({}),
            run_binding: json!({"run_id": "R-1", "unit_id": "u1"}),
            resume: json!({"kind": "none"}),
        })
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(self.state().join("controller.ledger")).unwrap_or_default()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let detached = Detached::new(self.rt.clone());
        for h in detached
            .list(self.hosts().server())
            .unwrap_or_default()
            .into_iter()
            .filter(|h| h.state != HostState::Gone)
        {
            let _ = detached.stop(&h.unit);
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn wait_for(what: &str, within: Duration, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{what} did not happen within {within:?}");
}

#[test]
fn a_save_commits_one_admission_with_no_ledger_line_and_the_read_gives_it_back() {
    let rig = Rig::new();
    let h = rig.launch_terminal(KEY, "sleep 60").unwrap();
    let unit = h.unit.clone();
    let store = rig.live.controller.store().clone();
    assert_eq!(rig.hosts().position(&unit), Ok(None), "nothing saved yet");
    let lines_before = rig.ledger().lines().count();

    rig.hosts()
        .save_position(&unit, json!({"offset": 17, "pending": []}))
        .unwrap();
    let id = format!("host-position-{unit}-1");
    let admission = store
        .admission("@controller", &id)
        .unwrap()
        .expect("the admission of the first save");
    assert_eq!(admission.outcome, AdmissionOutcome::Admitted);
    assert_eq!(admission.kind, "host_position");
    assert_eq!(
        rig.ledger().lines().count(),
        lines_before,
        "a save writes no controller.ledger line"
    );
    assert!(!rig.ledger().contains("host_position"), "{}", rig.ledger());
    let read = rig.hosts().position(&unit).unwrap().unwrap();
    assert_eq!(read, json!({"offset": 17, "pending": []}));
    assert_eq!(store.host_position(&unit).unwrap().unwrap().seq, 1);

    // The second save is seq 2, under its own command ID.
    rig.hosts()
        .save_position(&unit, json!({"offset": 40, "pending": []}))
        .unwrap();
    let second = format!("host-position-{unit}-2");
    assert!(store.admission("@controller", &second).unwrap().is_some());
    assert_eq!(
        rig.hosts().position(&unit).unwrap().unwrap(),
        json!({"offset": 40, "pending": []})
    );
    assert_eq!(store.host_position(&unit).unwrap().unwrap().seq, 2);
    assert_eq!(rig.ledger().lines().count(), lines_before);
}

#[test]
fn a_controller_opened_again_on_the_same_state_directory_reads_the_last_position() {
    let rig = Rig::new();
    let h = rig.launch_terminal(KEY, "sleep 60").unwrap();
    let unit = h.unit.clone();
    rig.hosts()
        .save_position(&unit, json!({"offset": 5}))
        .unwrap();
    rig.hosts()
        .save_position(&unit, json!({"offset": 9, "pending": [{"id": "x"}]}))
        .unwrap();

    let mut cfg = Config::new(rig.state(), 3_600_000, BIN.into());
    tweak(&mut cfg, &rig.rt);
    let second = Controller::open(cfg, rig.live.ports.clone()).unwrap();
    let read = second.host_service().position(&unit).unwrap().unwrap();
    assert_eq!(read, json!({"offset": 9, "pending": [{"id": "x"}]}));
    // The reopened controller goes on with seq 3.
    second
        .host_service()
        .save_position(&unit, json!({"offset": 12}))
        .unwrap();
    assert_eq!(second.store().host_position(&unit).unwrap().unwrap().seq, 3);
}

#[test]
fn a_save_for_a_unit_the_registry_does_not_hold_answers_a_host_error() {
    let rig = Rig::new();
    let err = rig
        .hosts()
        .save_position("pf-host-nosuch-unit-1", json!({"offset": 1}))
        .unwrap_err();
    assert!(!err.0.is_empty());
    assert!(err.0.contains("pf-host-nosuch-unit-1"), "{err}");
    assert_eq!(rig.hosts().position("pf-host-nosuch-unit-1"), Ok(None));
}

#[test]
fn a_launch_on_a_terminal_records_the_transport_and_the_child_sees_a_terminal() {
    let rig = Rig::new();
    let out = rig.tmp.join("tty.txt");
    let script = format!(
        "if test -t 0; then echo tty > {out}; else echo notty > {out}; fi; sleep 60",
        out = out.display()
    );
    let h = rig.launch_terminal(KEY, &script).unwrap();
    let transport: &Value = &h.record["transport"];
    assert_eq!(transport, &json!({"kind": "pty", "rows": 30, "cols": 100}));
    wait_for("the child's answer", Duration::from_secs(10), || {
        out.exists()
    });
    wait_for("the child's line", Duration::from_secs(10), || {
        std::fs::read_to_string(&out).is_ok_and(|t| !t.is_empty())
    });
    assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "tty");
    // The stored record says the same.
    let rec = rig
        .live
        .controller
        .store()
        .launch_record("seat", KEY, 1, 1)
        .unwrap()
        .unwrap();
    let body: Value = serde_json::from_str(&rec.record).unwrap();
    assert_eq!(body["transport"]["kind"], "pty");
}
