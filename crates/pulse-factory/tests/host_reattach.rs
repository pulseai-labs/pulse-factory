//! AC-10 (the host half) of `r2.s3.w3`: the host service's re-attach under the clear grant
//! (`docs/contracts/session-host.md`, "The attach checks"; ADR-0001, ADR-0007, amended). A seat that a
//! settle marked is re-attached with the hello and the checks of tier 1, a mandatory `host_attach`
//! line and an outcome line `host_reattach` in place of `host_tier1`. A rebind that no settle
//! marked keeps tier 1's best-effort lines. The hosts are real detached `pf-host` processes in a
//! temp runtime directory, as `host_service.rs` runs them. Nothing here starts a seat program.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::BIN;
use common::ctl::Live;
use pf_engine::port::host::{
    HostError, HostHandle, HostLaunch, HostTransport, Rebind, SessionHosts,
};
use pf_engine::port::store::Store;
use pf_host::launcher::{Detached, HostRef, HostState, LaunchError, Launcher, StartSpec, Started};
use pf_host::names;
use pulse_factory::controller::hosts::HostLauncher;
use serde_json::{Value, json};

const KEY: &str = "R-1/u1";
const SECRET: &str = "seat-secret-text-3f9a";

static N: AtomicUsize = AtomicUsize::new(0);

/// The `pf-host` binary: `PF_HOST_BIN`, else `pf-host` in the test binary's target directory.
fn host_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("PF_HOST_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    std::env::current_exe()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .expect("the test binary has a grandparent directory")
        .join("pf-host")
}

type Tweak = Box<dyn Fn(&mut StartSpec) + Send>;

/// A launcher that counts and records what the service asks of it, over a real one.
struct Probe {
    inner: Box<dyn Launcher + Send + Sync>,
    /// `(session, incarnation)` of each `start`.
    starts: Mutex<Vec<(String, u64)>>,
    /// Whether the launch record's row existed when each `start` ran.
    record_rows: Mutex<Vec<bool>>,
    /// Asked with the incarnation; says whether revision 1 of the record is in the store.
    record_exists: OnceLock<Box<dyn Fn(u64) -> bool + Send + Sync>>,
    /// Edits the spec before the real launcher sees it: a fault a test injects.
    tweak: Mutex<Option<Tweak>>,
    /// The attach key of each start, and the process ID of each host.
    keys: Mutex<Vec<String>>,
    pids: Mutex<Vec<u32>>,
}

impl Probe {
    fn over(inner: impl Launcher + Send + Sync + 'static) -> Arc<Probe> {
        Arc::new(Probe {
            inner: Box::new(inner),
            starts: Mutex::new(Vec::new()),
            record_rows: Mutex::new(Vec::new()),
            record_exists: OnceLock::new(),
            tweak: Mutex::new(None),
            keys: Mutex::new(Vec::new()),
            pids: Mutex::new(Vec::new()),
        })
    }
}

impl Launcher for Probe {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn start(&self, spec: &StartSpec) -> Result<Started, LaunchError> {
        self.starts
            .lock()
            .unwrap()
            .push((spec.session.clone(), spec.incarnation));
        self.keys.lock().unwrap().push(spec.attach_key.clone());
        if let Some(check) = self.record_exists.get() {
            let seen = check(spec.incarnation);
            self.record_rows.lock().unwrap().push(seen);
        }
        let mut spec = spec.clone();
        if let Some(t) = self.tweak.lock().unwrap().as_ref() {
            t(&mut spec);
        }
        let started = self.inner.start(&spec)?;
        self.pids.lock().unwrap().push(started.pid);
        Ok(started)
    }

    fn state(&self, unit: &str) -> HostState {
        self.inner.state(unit)
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        self.inner.stop(unit)
    }

    fn list(&self, server: &str) -> Result<Vec<HostRef>, LaunchError> {
        self.inner.list(server)
    }
}

/// A controller in the test process with a host service on a probe launcher.
struct Rig {
    live: Live,
    probe: Arc<Probe>,
    rt: PathBuf,
    tmp: PathBuf,
}

impl Rig {
    fn detached() -> Rig {
        Rig::over(|rt| Probe::over(Detached::new(rt.to_path_buf())))
    }

    fn over(make: impl FnOnce(&Path) -> Arc<Probe>) -> Rig {
        Rig::over_with(make, |_| {})
    }

    /// As [`Rig::over`], with a last edit of the controller's config.
    fn over_with(
        make: impl FnOnce(&Path) -> Arc<Probe>,
        tweak: impl FnOnce(&mut pulse_factory::controller::Config) + 'static,
    ) -> Rig {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("pfhs-{}-{n}", std::process::id()));
        let rt = tmp.join("rt");
        std::fs::create_dir_all(&rt).unwrap();
        let probe = make(&rt);
        let (launcher, runtime) = (probe.clone(), rt.clone());
        let live = Live::start_config(BIN.into(), move |cfg| {
            cfg.runtime_dir = runtime;
            cfg.host_launcher = HostLauncher::Custom(launcher);
            cfg.host_bin = Some(host_bin());
            tweak(cfg);
        });
        let store = live.controller.store().clone();
        probe
            .record_exists
            .set(Box::new(move |inc| {
                store
                    .launch_record("seat", KEY, u32::try_from(inc).unwrap(), 1)
                    .unwrap()
                    .is_some()
            }))
            .ok();
        Rig {
            live,
            probe,
            rt,
            tmp,
        }
    }

    fn hosts(&self) -> &Arc<pulse_factory::controller::hosts::HostService> {
        self.live.controller.host_service()
    }

    fn state(&self) -> PathBuf {
        self.live.sb.state.clone()
    }

    fn server(&self) -> String {
        self.hosts().server().to_string()
    }

    fn launch(&self, key: &str, script: &str) -> Result<HostHandle, HostError> {
        self.hosts().launch(HostLaunch {
            session_kind: "seat".to_string(),
            session_key: key.to_string(),
            argv: vec!["/bin/sh".into(), "-c".into(), script.to_string()],
            cwd: self.tmp.display().to_string(),
            env: [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
            files: Vec::new(),
            secrets: [("PF_SESSION_KEY".to_string(), SECRET.to_string())].into(),
            transport: HostTransport::Pipes,
            runtime: json!({"id": "paper"}),
            settings: json!({"env.PF_SESSION_KEY": SECRET}),
            run_binding: json!({"run_id": "R-1", "unit_id": "u1"}),
            resume: Value::Null,
        })
    }

    /// The record file and the key file of a unit.
    fn files(&self, unit: &str) -> (PathBuf, PathBuf) {
        let (session, inc) =
            names::parse_unit(&self.server(), unit).expect("a unit of this server");
        let dir = self.state().join("hosts").join(self.server());
        (
            dir.join(format!("{session}-{inc}.record.json")),
            dir.join(format!("{session}-{inc}.key")),
        )
    }

    fn ledger(&self) -> String {
        std::fs::read_to_string(self.state().join("controller.ledger")).unwrap_or_default()
    }

    /// The ledger lines of one kind.
    fn lines(&self, kind: &str) -> Vec<String> {
        self.ledger()
            .lines()
            .filter(|l| l.contains(kind))
            .map(str::to_string)
            .collect()
    }

    fn host_state(&self, unit: &str) -> HostState {
        Detached::new(self.rt.clone()).state(unit)
    }

    fn wait_gone(&self, unit: &str, within: Duration) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if self.host_state(unit) == HostState::Gone {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.host_state(unit) == HostState::Gone
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let detached = Detached::new(self.rt.clone());
        for h in detached.list(&self.server()).unwrap_or_default() {
            let _ = detached.stop(&h.unit);
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

// ------------------------------------------------------------------ the re-attach

const SETTLE: &str = "R-7@1";

fn marked(rig: &Rig) {
    rig.hosts().mark_reattach("R-1", "u1", SETTLE);
}

/// Start a host and let go of the controller's attach, as a controller that stopped would.
fn started(rig: &Rig) -> String {
    let h = rig.launch(KEY, "sleep 60").unwrap();
    let unit = h.unit.clone();
    drop(h);
    unit
}

fn survived(r: Rebind) {
    match r {
        Rebind::Survived(_) => {}
        Rebind::Lost { exit } => panic!("lost: {exit}"),
        Rebind::Unreachable { why } => panic!("unreachable: {why}"),
    }
}

#[test]
fn a_marked_rebind_of_a_live_host_writes_one_attach_line_and_one_reattach_line_and_no_tier1_line() {
    let rig = Rig::detached();
    let unit = started(&rig);
    let attach_before = rig.lines("host_attach").len();
    marked(&rig);
    survived(rig.hosts().rebind(&unit));

    assert_eq!(
        rig.lines("host_attach").len(),
        attach_before + 1,
        "{}",
        rig.ledger()
    );
    let reattach = rig.lines("host_reattach");
    assert_eq!(reattach.len(), 1, "{reattach:#?}");
    assert!(
        reattach[0].contains(&format!("host_reattach {unit} survived session={KEY}"))
            && reattach[0].contains(&format!("settle={SETTLE}"))
            && reattach[0].contains("reason=")
            && reattach[0].contains("command="),
        "{reattach:#?}"
    );
    assert!(rig.lines("host_tier1").is_empty(), "{}", rig.ledger());

    // The mark gone, the same rebind is tier 1: its line, no new re-attach line.
    rig.hosts().clear_reattach("R-1", "u1");
    survived(rig.hosts().rebind(&unit));
    assert_eq!(rig.lines("host_tier1").len(), 1, "{}", rig.ledger());
    assert!(rig.lines("host_tier1")[0].contains(&format!("host_tier1 {unit} survived")));
    assert_eq!(rig.lines("host_reattach").len(), 1);
    assert_eq!(rig.lines("host_attach").len(), attach_before + 2);
}

#[test]
fn a_marked_rebind_with_no_attach_line_is_unreachable_and_an_unmarked_one_still_survives() {
    let rig = Rig::detached();
    let unit = started(&rig);
    // The controller's ledger file cannot be appended to: a directory is in its place.
    let ledger = rig.state().join("controller.ledger");
    let _ = std::fs::remove_file(&ledger);
    std::fs::create_dir(&ledger).unwrap();

    marked(&rig);
    match rig.hosts().rebind(&unit) {
        Rebind::Unreachable { why } => {
            assert!(why.contains("the attach line cannot be written"), "{why}");
        }
        Rebind::Survived(_) => panic!("a re-attach with no attach line must not survive"),
        Rebind::Lost { exit } => panic!("lost: {exit}"),
    }
    // Tier 1 is as it was: the line is best effort, and the host is attached.
    rig.hosts().clear_reattach("R-1", "u1");
    survived(rig.hosts().rebind(&unit));
}

#[test]
fn a_slow_host_is_unreachable_and_answers_later() {
    let rig = Rig::detached();
    let unit = started(&rig);
    let pid = *rig.probe.pids.lock().unwrap().last().unwrap();
    let signal = |s: &str| {
        assert!(
            std::process::Command::new("kill")
                .args([s, &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
    };
    marked(&rig);
    signal("-STOP");
    let stopped = rig.hosts().rebind(&unit);
    signal("-CONT");
    match stopped {
        Rebind::Unreachable { why } => assert!(why.contains("no attach"), "{why}"),
        Rebind::Survived(_) => panic!("a stopped host cannot answer"),
        Rebind::Lost { exit } => panic!("lost: {exit}"),
    }
    let lines = rig.lines("host_reattach");
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains(&format!("host_reattach {unit} unreachable")),
        "{lines:#?}"
    );
    // The host runs again and a marked rebind answers.
    survived(rig.hosts().rebind(&unit));
    assert_eq!(rig.lines("host_reattach").len(), 2);
    assert!(rig.lines("host_reattach")[1].contains("survived"));
}

#[test]
fn a_gone_host_is_lost_and_its_row_ends() {
    let rig = Rig::detached();
    let unit = started(&rig);
    Detached::new(rig.rt.clone()).stop(&unit).unwrap();
    assert!(rig.wait_gone(&unit, Duration::from_secs(10)));
    marked(&rig);
    match rig.hosts().rebind(&unit) {
        Rebind::Lost { .. } => {}
        Rebind::Survived(_) => panic!("a gone host cannot survive"),
        Rebind::Unreachable { why } => panic!("unreachable: {why}"),
    }
    let row = rig.live.controller.store().host(&unit).unwrap().unwrap();
    assert!(row.ended_at.is_some(), "the row ends");
    let (record, key) = rig.files(&unit);
    assert!(!record.exists() && !key.exists(), "the files go");
    let lines = rig.lines("host_reattach");
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains(&format!("host_reattach {unit} lost")),
        "{lines:#?}"
    );
    assert!(rig.lines("host_tier1").is_empty());
    // A row that ended is lost again at once, with no new line.
    assert!(matches!(rig.hosts().rebind(&unit), Rebind::Lost { .. }));
    assert_eq!(rig.lines("host_reattach").len(), 1);
}
