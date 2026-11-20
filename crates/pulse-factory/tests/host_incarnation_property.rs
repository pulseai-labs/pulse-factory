//! AC-7 of `r2.s2.w3`, the `run-state-admission` property (Q9, G2): for any sequence of launches,
//! relaunches, controller restarts, launches whose start fails, launches that crash between the
//! host's start and the commit of its row, and launches whose older incarnation will not stop,
//!
//! - the launcher's start runs at most once for each (session, incarnation);
//! - at most one host of a session runs at any time;
//! - an older incarnation is stopped before the next one starts (no host of the session runs when
//!   a start begins);
//! - the incarnation of a launch is the next one after every launch record of the session;
//! - a launch whose older incarnation will not stop is refused with a named reason and starts
//!   nothing.
//!
//! The hosts are real detached hosts in a temp runtime directory, and a guard stops every one of them
//! when a case ends. A controller restart is a new `Controller` on the same state directory; its
//! orphan sweep stops the hosts, since no run claims them.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::BIN;
use common::restart::host_bin;
use pf_engine::port::host::{HostLaunch, HostTransport, SessionHosts};
use pf_engine::port::store::Store;
use pf_host::launcher::{Detached, HostRef, HostState, LaunchError, Launcher, StartSpec, Started};
use pf_ipc::{Handler, Peer, Request};
use proptest::prelude::*;
use pulse_factory::controller::hosts::HostLauncher;
use pulse_factory::controller::ports::{Ports, RunPorts, RunSpec};
use pulse_factory::controller::{Config, Controller};
use serde_json::{Value, json};

static N: AtomicUsize = AtomicUsize::new(0);

const SESSIONS: [&str; 3] = ["R-1/a", "R-1/b", "R-1/c"];

struct NoPorts;

impl Ports for NoPorts {
    fn build(&self, _: &RunSpec) -> Result<RunPorts, String> {
        Err("this test arms no run".to_string())
    }
}

/// The launcher the service starts hosts with: real detached hosts, counted and checked.
struct Counting {
    inner: Detached,
    server: Mutex<String>,
    /// `(session, incarnation)` of each `start` call.
    starts: Mutex<Vec<(String, u64)>>,
    /// What went wrong: a start while a host of the session still ran.
    violations: Mutex<Vec<String>>,
    /// The next start fails.
    fail_next: AtomicBool,
    /// Units whose stop does nothing and fails: a host that will not stop.
    stuck: Mutex<BTreeSet<String>>,
}

impl Launcher for Counting {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn start(&self, spec: &StartSpec) -> Result<Started, LaunchError> {
        self.starts
            .lock()
            .unwrap()
            .push((spec.session.clone(), spec.incarnation));
        // An older incarnation is stopped before the next one starts.
        let server = self.server.lock().unwrap().clone();
        for h in self.inner.list(&server).unwrap_or_default() {
            if h.session == spec.session && h.state != HostState::Gone {
                self.violations.lock().unwrap().push(format!(
                    "start of {} incarnation {} while {} runs",
                    spec.session, spec.incarnation, h.unit
                ));
            }
        }
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(LaunchError(
                "the start failed (a fault a test injected)".into(),
            ));
        }
        self.inner.start(spec)
    }

    fn state(&self, unit: &str) -> HostState {
        self.inner.state(unit)
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        if self.stuck.lock().unwrap().contains(unit) {
            return Err(LaunchError(format!(
                "{unit} will not stop (a fault a test injected)"
            )));
        }
        self.inner.stop(unit)
    }

    fn list(&self, server: &str) -> Result<Vec<HostRef>, LaunchError> {
        self.inner.list(server)
    }
}

struct World {
    tmp: PathBuf,
    rt: PathBuf,
    state: PathBuf,
    launcher: Arc<Counting>,
    controller: Option<Arc<Controller>>,
    /// The launch records each session has written: the model's next incarnation is one more.
    records: BTreeMap<String, u64>,
}

impl World {
    fn new() -> World {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("pfp-{}-{n}", std::process::id()));
        let (rt, state) = (tmp.join("rt"), tmp.join("st"));
        std::fs::create_dir_all(&rt).unwrap();
        pf_ipc::prepare_state_dir(&state).unwrap();
        let launcher = Arc::new(Counting {
            inner: Detached::new(rt.clone()),
            server: Mutex::new(String::new()),
            starts: Mutex::new(Vec::new()),
            violations: Mutex::new(Vec::new()),
            fail_next: AtomicBool::new(false),
            stuck: Mutex::new(BTreeSet::new()),
        });
        let mut w = World {
            tmp,
            rt,
            state,
            launcher,
            controller: None,
            records: BTreeMap::new(),
        };
        w.open();
        w
    }

    fn open(&mut self) {
        let mut cfg = Config::new(self.state.clone(), 3_600_000, BIN.into());
        cfg.runtime_dir = self.rt.clone();
        cfg.host_launcher = HostLauncher::Custom(self.launcher.clone());
        cfg.host_bin = Some(host_bin());
        let controller = Controller::open(cfg, Arc::new(NoPorts)).unwrap();
        *self.launcher.server.lock().unwrap() = controller.host_service().server().to_string();
        self.controller = Some(controller);
    }

    fn controller(&self) -> &Arc<Controller> {
        self.controller.as_ref().unwrap()
    }

    fn server(&self) -> String {
        self.controller().host_service().server().to_string()
    }

    /// Make the controller's attach wait short, so a host that will not stop is refused in about
    /// a second and not in thirty.
    fn short_attach_wait(&self) {
        let answer = self.controller().handle_from(
            Request::new(
                "settings_set",
                json!({"key": "session.attach_wait_ms", "value": 100, "command_id": "set-wait"}),
            ),
            Peer {
                pid: Some(std::process::id()),
            },
        );
        assert!(answer.ok, "{answer:?}");
    }

    fn launch(&self, key: &str) -> Result<u32, String> {
        self.controller()
            .host_service()
            .launch(HostLaunch {
                session_kind: "seat".to_string(),
                session_key: key.to_string(),
                argv: vec!["/bin/sh".into(), "-c".into(), "sleep 60".into()],
                cwd: self.tmp.display().to_string(),
                env: [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
                files: Vec::new(),
                secrets: [("PF_SESSION_KEY".to_string(), "k".to_string())].into(),
                transport: HostTransport::Pipes,
                runtime: json!({"id": "paper"}),
                settings: json!({}),
                run_binding: json!({"run_id": "R-1"}),
                resume: Value::Null,
            })
            .map(|h| h.incarnation)
            .map_err(|e| e.0)
    }

    /// Hosts that run, by session.
    fn live(&self) -> BTreeMap<String, Vec<String>> {
        let mut by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for h in Detached::new(self.rt.clone())
            .list(&self.server())
            .unwrap_or_default()
        {
            if h.state != HostState::Gone {
                by.entry(h.session).or_default().push(h.unit);
            }
        }
        by
    }

    fn check_invariants(&self) -> Result<(), TestCaseError> {
        let starts = self.launcher.starts.lock().unwrap().clone();
        let mut seen = BTreeSet::new();
        for s in &starts {
            prop_assert!(
                seen.insert(s.clone()),
                "a second start of {s:?}: {starts:?}"
            );
        }
        let violations = self.launcher.violations.lock().unwrap().clone();
        prop_assert!(violations.is_empty(), "{violations:?}");
        for (session, units) in self.live() {
            prop_assert!(units.len() <= 1, "{session} runs {units:?}");
        }
        // Every live row names a running host, and no two live rows name one session.
        let rows = self.controller().store().live_hosts().unwrap();
        let mut sessions = BTreeSet::new();
        for row in &rows {
            prop_assert!(
                sessions.insert(row.session_id.clone()),
                "two live rows of {}",
                row.session_id
            );
        }
        Ok(())
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.launcher.stuck.lock().unwrap().clear();
        self.controller = None;
        let detached = Detached::new(self.rt.clone());
        if let Ok(servers) = std::fs::read_dir(self.rt.join("pf-hosts")) {
            for server in servers.flatten() {
                let name = server.file_name().to_string_lossy().into_owned();
                for h in detached.list(&name).unwrap_or_default() {
                    let _ = detached.stop(&h.unit);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

#[derive(Clone, Debug)]
enum Op {
    /// A launch of a session: the first, or the next incarnation after a `clear`.
    Launch(usize),
    /// The same launch twice, one after the other.
    LaunchTwice(usize),
    /// The controller stops and starts again.
    Restart,
    /// The next start fails.
    StartFails(usize),
    /// The next launch crashes between the host's start and the commit of its row.
    CrashAfterStart(usize),
    /// The live host of the session will not stop, and a launch of the session follows.
    WillNotStop(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..SESSIONS.len()).prop_map(Op::Launch),
        1 => (0..SESSIONS.len()).prop_map(Op::LaunchTwice),
        1 => Just(Op::Restart),
        1 => (0..SESSIONS.len()).prop_map(Op::StartFails),
        1 => (0..SESSIONS.len()).prop_map(Op::CrashAfterStart),
        1 => (0..SESSIONS.len()).prop_map(Op::WillNotStop),
    ]
}

/// One launch, checked against the model of the incarnation.
fn launch_checked(w: &mut World, key: &str) -> Result<(), TestCaseError> {
    let expected = w.records.get(key).copied().unwrap_or(0) + 1;
    let starts_before = w.launcher.starts.lock().unwrap().len();
    match w.launch(key) {
        Ok(inc) => {
            prop_assert_eq!(u64::from(inc), expected, "the incarnation of {}", key);
            w.records.insert(key.to_string(), expected);
        }
        Err(why) => {
            // A start that fails or crashes still wrote its record before it started.
            prop_assert!(
                why.contains("fault a test injected") || why.contains("did not stop"),
                "an unexpected failure: {why}"
            );
            if why.contains("did not stop") {
                prop_assert_eq!(
                    w.launcher.starts.lock().unwrap().len(),
                    starts_before,
                    "a launch that is refused started a host"
                );
            } else {
                w.records.insert(key.to_string(), expected);
            }
        }
    }
    Ok(())
}

fn run(ops: &[Op]) -> Result<(), TestCaseError> {
    let mut w = World::new();
    for op in ops {
        match op {
            Op::Launch(i) => launch_checked(&mut w, SESSIONS[*i])?,
            Op::LaunchTwice(i) => {
                launch_checked(&mut w, SESSIONS[*i])?;
                launch_checked(&mut w, SESSIONS[*i])?;
            }
            Op::Restart => {
                w.controller = None;
                w.open();
                // The orphan sweep stops every host: no run claims one.
                prop_assert!(
                    w.live().is_empty(),
                    "a host outlived the sweep: {:?}",
                    w.live()
                );
            }
            Op::StartFails(i) => {
                w.launcher.fail_next.store(true, Ordering::SeqCst);
                launch_checked(&mut w, SESSIONS[*i])?;
                w.launcher.fail_next.store(false, Ordering::SeqCst);
            }
            Op::CrashAfterStart(i) => {
                w.controller()
                    .host_service()
                    .faults
                    .crash_after_start
                    .store(true, Ordering::SeqCst);
                launch_checked(&mut w, SESSIONS[*i])?;
                w.controller()
                    .host_service()
                    .faults
                    .crash_after_start
                    .store(false, Ordering::SeqCst);
            }
            Op::WillNotStop(i) => {
                let key = SESSIONS[*i];
                if let Some(units) = w.live().get(&pf_session(key)).cloned() {
                    w.short_attach_wait();
                    w.launcher
                        .stuck
                        .lock()
                        .unwrap()
                        .extend(units.iter().cloned());
                    let before = w.launcher.starts.lock().unwrap().len();
                    let err = w
                        .launch(key)
                        .expect_err("a host that will not stop refuses the launch");
                    prop_assert!(err.contains("did not stop"), "{err}");
                    prop_assert!(err.contains(&units[0]), "the reason names the host: {err}");
                    prop_assert_eq!(w.launcher.starts.lock().unwrap().len(), before);
                    w.launcher.stuck.lock().unwrap().clear();
                }
                // With no host to stick, it is an ordinary launch.
                launch_checked(&mut w, key)?;
            }
        }
        w.check_invariants()?;
    }
    Ok(())
}

/// The session name of a key, as the service makes it.
fn pf_session(key: &str) -> String {
    pulse_factory::controller::hosts::session_name("seat", key)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn no_second_host_starts_for_one_incarnation_and_no_two_copies_run(
        ops in proptest::collection::vec(op(), 1..9)
    ) {
        run(&ops)?;
    }
}

/// The deterministic floor under the property: a refused launch starts nothing, and the launch that
/// follows the host's stop takes the next incarnation.
#[test]
fn a_launch_whose_older_incarnation_will_not_stop_is_refused_and_starts_nothing() {
    let w = World::new();
    w.short_attach_wait();
    assert_eq!(w.launch("R-1/a").unwrap(), 1);
    let unit = w.live().values().next().unwrap()[0].clone();
    w.launcher.stuck.lock().unwrap().insert(unit.clone());
    let err = w.launch("R-1/a").unwrap_err();
    assert!(err.contains("did not stop") && err.contains(&unit), "{err}");
    assert_eq!(w.launcher.starts.lock().unwrap().len(), 1);
    // Nothing else changed: the old host still runs, and no second record was written.
    assert_eq!(w.live().values().map(Vec::len).sum::<usize>(), 1);
    assert!(
        w.controller()
            .store()
            .newest_launch_record("seat", "R-1/a", 2)
            .unwrap()
            .is_none()
    );
    // Once it stops, the next launch is incarnation 2.
    w.launcher.stuck.lock().unwrap().clear();
    assert_eq!(w.launch("R-1/a").unwrap(), 2);
    assert_eq!(w.live().values().map(Vec::len).sum::<usize>(), 1);
}
