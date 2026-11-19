//! AC-4 of `r2.s2.w3`: the controller's host service (ADR-0001's amendment of 2026-11-16, Q4, Q6,
//! Q9). A launch writes revision 1 of the launch record before the host starts, makes the attach
//! key, starts the host, attaches with the controller's half of the checks, and registers the host.
//! The tests use the detached launcher in a temp runtime directory, a temp state directory (so a
//! server name of their own) and a guard that stops every host of the server when a test ends.
//! Nothing here starts a seat program, spends a token or reaches beyond the temp directories.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::ctl::Live;
use common::{BIN, code};
use pf_engine::port::host::{
    HostError, HostEvent, HostExit, HostHandle, HostLaunch, HostTransport, SessionHosts,
};
use pf_engine::port::store::Store;
use pf_host::launcher::{
    Detached, HostRef, HostState, LaunchError, Launcher, StartSpec, Started, Systemd,
};
use pf_host::{names, sha256};
use pulse_factory::controller::hosts::{HostLauncher, host_server_name, read_host_server_name};
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

    fn set_tweak(&self, t: impl Fn(&mut StartSpec) + Send + 'static) {
        *self.tweak.lock().unwrap() = Some(Box::new(t));
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

    /// Hosts of this server that still run.
    fn live_hosts(&self) -> Vec<HostRef> {
        Detached::new(self.rt.clone())
            .list(&self.server())
            .unwrap_or_default()
            .into_iter()
            .filter(|h| h.state != HostState::Gone)
            .collect()
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

/// Read the host's output to its exit and acknowledge the output.
fn drain_to_exit(h: &HostHandle) -> (Vec<u8>, HostExit) {
    h.read(0).unwrap();
    let mut out = Vec::new();
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        for ev in h.events(Duration::from_millis(100)).unwrap() {
            match ev {
                HostEvent::Out { data, .. } => out.extend(data),
                HostEvent::Exit(e) => {
                    h.ack(e.at).unwrap();
                    return (out, e);
                }
                HostEvent::Gap { .. } => {}
            }
        }
    }
    panic!("the host sent no exit within 10 s");
}

fn all_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            all_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
}

// ------------------------------------------------------------------ the record and the row

#[test]
fn a_launch_writes_the_record_before_the_host_starts_and_the_row_after() {
    let rig = Rig::detached();
    let h = rig.launch(KEY, "sleep 60").unwrap();
    // The record row existed when the launcher's start ran.
    assert_eq!(*rig.probe.record_rows.lock().unwrap(), vec![true]);
    assert_eq!(rig.probe.starts.lock().unwrap().len(), 1);

    let store = rig.live.controller.store();
    let rec = store
        .launch_record("seat", KEY, 1, 1)
        .unwrap()
        .expect("record");
    let row = store.host(&h.unit).unwrap().expect("host row");
    assert_eq!(row.session_kind, "seat");
    assert_eq!(row.session_id, KEY);
    assert_eq!(row.incarnation, 1);
    assert_eq!(row.launched_revision, 1);
    assert_eq!(row.launched_record_sha256, rec.record_sha256);
    assert_eq!(row.launcher, "detached");
    assert!(row.ended_at.is_none());
    assert_eq!(store.live_hosts().unwrap().len(), 1);

    // The record file is the canonical text, and its hash is the stored one.
    let (record_file, key_file) = rig.files(&h.unit);
    let bytes = std::fs::read(&record_file).unwrap();
    assert_eq!(sha256::sha256_hex(&bytes), rec.record_sha256);
    assert_eq!(String::from_utf8(bytes).unwrap(), rec.record);
    assert_eq!(
        h.record,
        serde_json::from_str::<Value>(&rec.record).unwrap()
    );

    // The host's own `hello` names the same hash.
    let key = std::fs::read_to_string(&key_file).unwrap();
    let (_conn, hello) = pf_host::client::connect(Path::new(&row.socket), key.trim()).unwrap();
    assert_eq!(hello.record_sha256, rec.record_sha256);
    assert_eq!(hello.child_pid, h.child_pid);
}

#[test]
fn the_record_holds_each_session_key_with_its_value_and_scope_and_no_secret() {
    let rig = Rig::detached();
    let out = rig
        .live
        .cli(&["settings", "set", "session.replay_bytes", "1048576"]);
    assert_eq!(code(&out), 0, "{}", common::stderr(&out));
    let h = rig.launch(KEY, "sleep 60").unwrap();
    let settings = &h.record["settings"];
    assert_eq!(
        settings["session.replay_bytes"],
        json!({"value": 1_048_576, "scope": "server"})
    );
    assert_eq!(
        settings["session.exit_hold_minutes"],
        json!({"value": 1440, "scope": "built_in"})
    );
    assert_eq!(
        settings["session.attach_wait_ms"],
        json!({"value": 3000, "scope": "built_in"})
    );
    assert_eq!(
        settings["session.hook_wait_seconds"],
        json!({"value": 15, "scope": "built_in"})
    );
    // A secret has its stand-in everywhere: the env, the settings entry that names it, the list.
    let stand_in = format!("sha256:{}", sha256::sha256_hex(SECRET.as_bytes()));
    assert_eq!(h.record["env"]["PF_SESSION_KEY"], stand_in);
    assert_eq!(settings["env.PF_SESSION_KEY"], stand_in);
    assert_eq!(h.record["secret_names"], json!(["PF_SESSION_KEY"]));
    assert_eq!(h.record["transport"], json!({"kind": "pipes"}));
}

#[test]
fn a_seats_record_env_carries_the_resolved_hook_wait_and_the_child_sees_it() {
    let rig = Rig::detached();
    // The built-in is 15 seconds.
    let h = rig.launch(KEY, "sleep 60").unwrap();
    assert_eq!(h.record["env"]["PF_HOOK_WAIT_SECONDS"], "15");
    let environ = std::fs::read(format!("/proc/{}/environ", h.child_pid)).unwrap();
    assert!(contains(&environ, "PF_HOOK_WAIT_SECONDS=15"));
    // A server setting is what the next launch resolves.
    let out = rig
        .live
        .cli(&["settings", "set", "session.hook_wait_seconds", "7"]);
    assert_eq!(code(&out), 0, "{}", common::stderr(&out));
    let h = rig.launch("R-1/u2", "sleep 60").unwrap();
    assert_eq!(h.record["env"]["PF_HOOK_WAIT_SECONDS"], "7");
    assert_eq!(
        h.record["settings"]["session.hook_wait_seconds"]["value"],
        7
    );
    let environ = std::fs::read(format!("/proc/{}/environ", h.child_pid)).unwrap();
    assert!(contains(&environ, "PF_HOOK_WAIT_SECONDS=7"));
}

#[test]
fn the_key_file_is_private_and_the_key_text_is_in_no_other_place() {
    let rig = Rig::detached();
    let h = rig.launch(KEY, "sleep 60").unwrap();
    let (record_file, key_file) = rig.files(&h.unit);
    let meta = std::fs::metadata(&key_file).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        key_file.parent().unwrap(),
        rig.state().join("hosts").join(rig.server())
    );
    assert_eq!(
        std::fs::metadata(key_file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&record_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let key = std::fs::read_to_string(&key_file).unwrap();
    let key = key.trim();
    assert_eq!(key.len(), 64, "32 bytes as hex");
    assert_eq!(*rig.probe.keys.lock().unwrap(), vec![key.to_string()]);

    // No store file, no record file, no ledger line holds the key or the seat's secret.
    let mut files = Vec::new();
    all_files(&rig.state(), &mut files);
    assert!(files.len() > 3, "{files:?}");
    for f in files.iter().filter(|f| **f != key_file) {
        let bytes = std::fs::read(f).unwrap_or_default();
        assert!(!contains(&bytes, key), "the key is in {}", f.display());
        assert!(
            !contains(&bytes, SECRET),
            "the secret is in {}",
            f.display()
        );
    }
    // No host argv holds either: the host's and the child's.
    let host_pid = rig.probe.pids.lock().unwrap()[0];
    for pid in [host_pid, h.child_pid] {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        assert!(!contains(&cmdline, key), "the key is in the argv of {pid}");
        assert!(
            !contains(&cmdline, SECRET),
            "the secret is in the argv of {pid}"
        );
    }
}

#[test]
fn each_attach_writes_one_attached_line() {
    let rig = Rig::detached();
    let h = rig.launch(KEY, "sleep 60").unwrap();
    let attached: Vec<String> = rig
        .lines("host_attach")
        .into_iter()
        .filter(|l| l.contains("attached"))
        .collect();
    assert_eq!(attached.len(), 1, "{attached:?}");
    assert!(attached[0].contains(&h.unit), "{}", attached[0]);
    assert!(attached[0].contains("incarnation=1"), "{}", attached[0]);
    // The record write has its own line, and neither holds the key.
    assert_eq!(rig.lines("host_record").len(), 1);
    // A second session is a second attach.
    let h2 = rig.launch("R-1/u2", "sleep 60").unwrap();
    let attached: Vec<String> = rig
        .lines("host_attach")
        .into_iter()
        .filter(|l| l.contains("attached"))
        .collect();
    assert_eq!(attached.len(), 2, "{attached:?}");
    assert!(attached[1].contains(&h2.unit));
}

// ------------------------------------------------------------------ the controller's half of the checks

/// Launch with `tweak` applied to the start, and check the refusal.
fn refused(tweak: impl Fn(&mut StartSpec) + Send + 'static, check: &str) {
    let rig = Rig::detached();
    rig.probe.set_tweak(tweak);
    let err = rig
        .launch(KEY, "sleep 60")
        .expect_err("the attach must be refused");
    assert!(err.0.contains("was refused"), "{err}");
    assert!(err.0.contains(check), "{err}");
    let store = rig.live.controller.store();
    assert!(store.live_hosts().unwrap().is_empty(), "no host row");
    let lines = rig.lines("host_attach");
    let refusals: Vec<&String> = lines.iter().filter(|l| l.contains("refused")).collect();
    assert_eq!(refusals.len(), 1, "{lines:?}");
    assert!(refusals[0].contains("check="), "{}", refusals[0]);
    assert!(refusals[0].contains(check), "{}", refusals[0]);
    assert!(lines.iter().all(|l| !l.contains(" attached ")), "{lines:?}");
    let key = rig.probe.keys.lock().unwrap()[0].clone();
    assert!(!rig.ledger().contains(&key), "the key is in the ledger");
    // The host the service had started is stopped, and its key file is gone.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rig.live_hosts().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(rig.live_hosts().is_empty(), "the refused host still runs");
    let session = rig.probe.starts.lock().unwrap()[0].0.clone();
    let dir = rig.state().join("hosts").join(rig.server());
    assert!(
        !dir.join(format!("{session}-1.key")).exists(),
        "the key file of a refused host stays"
    );
}

#[test]
fn a_host_that_names_another_record_hash_is_refused() {
    refused(
        |spec| {
            let mut bytes = std::fs::read(&spec.record_path).unwrap();
            bytes.push(b'\n');
            let other = spec.record_path.with_extension("other.json");
            std::fs::write(&other, bytes).unwrap();
            spec.record_path = other;
        },
        "record_sha256",
    );
}

#[test]
fn a_host_that_names_another_session_is_refused() {
    refused(
        |spec| spec.session = "sdeadbeefdeadbeef".to_string(),
        "session",
    );
}

#[test]
fn a_host_that_names_another_incarnation_is_refused() {
    refused(|spec| spec.incarnation += 1, "incarnation");
}

// ------------------------------------------------------------------ the launcher

#[test]
fn a_failing_systemd_run_fails_the_launch_with_its_text_and_starts_no_detached_host() {
    let stub_dir = std::env::temp_dir().join(format!("pfhs-stub-{}", std::process::id()));
    std::fs::create_dir_all(&stub_dir).unwrap();
    let stub = stub_dir.join("systemd-run-stub");
    std::fs::write(
        &stub,
        "#!/bin/sh\necho 'Failed to connect to bus: no user manager here' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let stub_text = stub.display().to_string();
    let rig = Rig::over(|rt| {
        Probe::over(Systemd::new(rt.to_path_buf()).with_programs(&stub_text, &stub_text))
    });
    let err = rig.launch(KEY, "sleep 60").expect_err("systemd-run fails");
    assert!(
        err.0
            .contains("Failed to connect to bus: no user manager here"),
        "{err}"
    );
    // The launcher's start ran once, and nothing fell back to the detached launcher.
    assert_eq!(rig.probe.starts.lock().unwrap().len(), 1);
    assert_eq!(rig.probe.name(), "systemd");
    assert!(
        !rig.rt.join("pf-hosts").exists(),
        "a detached host directory was made"
    );
    assert!(rig.live_hosts().is_empty());
    // The record row stays (it is write-once). No host row, no key file, no record file.
    let store = rig.live.controller.store();
    assert!(store.launch_record("seat", KEY, 1, 1).unwrap().is_some());
    assert!(store.live_hosts().unwrap().is_empty());
    let dir = rig.state().join("hosts").join(rig.server());
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".key") || n.ends_with(".record.json"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(&stub_dir);
}

// ------------------------------------------------------------------ the end of a host

#[test]
fn a_consumed_exit_ends_the_row_and_the_host_at_once_and_removes_the_files() {
    let rig = Rig::detached();
    let h = rig.launch(KEY, "echo out; exit 3").unwrap();
    let (record_file, key_file) = rig.files(&h.unit);
    let (out, exit) = drain_to_exit(&h);
    assert_eq!(out, b"out\n");
    assert_eq!(exit.code, Some(3));
    // The exit is read and not yet acknowledged: the host holds.
    assert_eq!(rig.host_state(&h.unit), HostState::Live);
    assert!(
        rig.live
            .controller
            .store()
            .host(&h.unit)
            .unwrap()
            .unwrap()
            .ended_at
            .is_none()
    );

    h.exit_ack().unwrap();
    let row = rig.live.controller.store().host(&h.unit).unwrap().unwrap();
    assert!(row.ended_at.is_some());
    let end: Value = serde_json::from_str(&row.exit.unwrap()).unwrap();
    assert_eq!(end["reason"], "exit_consumed");
    assert_eq!(end["exit"]["code"], 3);
    assert!(
        rig.wait_gone(&h.unit, Duration::from_secs(5)),
        "the host did not end"
    );
    assert!(!key_file.exists(), "the key file stays");
    assert!(!record_file.exists(), "the record file stays");
    assert_eq!(rig.lines("host_end").len(), 1);
    assert!(rig.live.controller.store().live_hosts().unwrap().is_empty());
}

#[test]
fn an_unconsumed_exit_holds_for_the_hold_and_then_the_host_ends() {
    let rig = Rig::detached();
    // The smallest setting is one minute; the test shortens the host's hold to one second.
    rig.probe
        .set_tweak(|spec| spec.exit_hold = Duration::from_secs(1));
    let h = rig.launch(KEY, "exit 0").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        rig.host_state(&h.unit),
        HostState::Live,
        "the host did not hold"
    );
    assert!(
        rig.wait_gone(&h.unit, Duration::from_secs(10)),
        "the host did not end after its hold"
    );
}

// ------------------------------------------------------------------ the server name

#[test]
fn the_server_name_is_made_once_as_twelve_hex_digits() {
    let rig = Rig::detached();
    let state = rig.state();
    let name = rig.server();
    assert_eq!(name.len(), 12, "{name}");
    assert!(
        name.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let file = state.join("hosts").join("server");
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), name);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(state.join("hosts"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    // A second open reads the same name and changes nothing.
    assert_eq!(
        host_server_name(&state, Path::new("/dev/urandom")).unwrap(),
        name
    );
    assert_eq!(read_host_server_name(&state).unwrap(), Some(name.clone()));
    assert_eq!(std::fs::read_to_string(&file).unwrap().trim(), name);
    // A state directory with no file has no name, and reading makes none.
    let empty = rig.tmp.join("empty-state");
    std::fs::create_dir_all(&empty).unwrap();
    assert_eq!(read_host_server_name(&empty).unwrap(), None);
    assert!(!empty.join("hosts").exists());
    // Another state directory gets a name of its own.
    let other = host_server_name(&empty, Path::new("/dev/urandom")).unwrap();
    assert_ne!(other, name);
}

#[test]
fn a_server_name_that_cannot_be_made_still_opens_the_controller_and_refuses_each_launch() {
    // The random source gives 5 bytes and 6 are needed, so no name can be made. The controller opens
    // (`fail_closed_key.rs` needs that), a launch is refused with the cause and starts nothing, a
    // rebind is unreachable, and the orphan sweep is skipped with a reason `status` shows.
    let short = std::env::temp_dir().join(format!("pfhs-{}-short", std::process::id()));
    std::fs::write(&short, [7u8, 8, 9, 10, 11]).unwrap();
    let device = short.clone();
    let rig = Rig::over_with(
        |rt| Probe::over(Detached::new(rt.to_path_buf())),
        move |cfg| cfg.random_device = device,
    );
    let err = rig.launch(KEY, "exit 0").unwrap_err().to_string();
    assert!(err.contains("no host server name can be made"), "{err}");
    assert!(err.contains("gave 5 bytes, and 6 are needed"), "{err}");
    assert!(
        rig.probe.starts.lock().unwrap().is_empty(),
        "a host started"
    );
    assert!(
        matches!(
            rig.hosts().rebind("pf-host-aaaaaaaaaaaa-sx-1"),
            pf_engine::port::host::Rebind::Unreachable { .. }
        ),
        "rebind did not answer unreachable"
    );
    let skipped = rig.hosts().sweep_skipped().expect("the sweep was skipped");
    assert!(
        skipped.contains("no host server name can be made"),
        "{skipped}"
    );
    let _ = std::fs::remove_file(&short);
}
