//! The host service (ADR-0001's amendment of 2026-11-16, `docs/contracts/session-host.md`).
//!
//! The controller starts each session in a host outside its own process. For one launch the
//! service ends an older incarnation of the same session, writes the launch record, makes the
//! attach key, starts the host through the configured launcher, attaches with the checks, registers
//! the host, and writes one ledger line for the attach. At start, before the controller binds its
//! socket, it runs tier 1 (ADR-0007): each live seat is survived, lost, unreachable or an orphan.
//!
//! The service implements the neutral port `SessionHosts` of `pf-engine`; the engine and the
//! adapters name no launcher and no host crate.
//!
//! Every write to the store is an admitted command under `@controller` with a command ID, and
//! every command is one line of `controller.ledger`. The attach key is in memory and in a key file
//! (mode 0600) beside the record file. It is in no store row, no record, no argument and no line.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use pf_engine::port::host::{
    HostError, HostEvent, HostExit, HostHandle, HostLaunch, HostSession, HostSignal, HostTransport,
    Rebind, SessionHosts,
};
use pf_engine::port::store::{
    Effect, HostRow, NewHost, NewLaunchRecord, SESSION_KINDS, Store, canonical_json,
};
use pf_host::client::{self, ClientError, Conn, Event};
use pf_host::launcher::{Detached, HostState, Launcher, StartSpec, Systemd};
use pf_host::protocol::{Hello, SignalKind};
use pf_host::{names, peer, sha256};
use serde_json::{Value, json};

use super::Controller;
use super::commands::{Plan, field, now};
use super::seat::{hex, random_bytes};

/// The launcher a controller starts its hosts with (`PF_HOST_LAUNCHER`, Q6).
#[derive(Clone)]
pub enum HostLauncher {
    /// A double-forked process in its own session. A fake for tests and fenced seats.
    Detached,
    /// A transient systemd user unit for each host.
    Systemd,
    /// A launcher a test supplies: a counting wrapper, or `Systemd` with test doubles of
    /// `systemd-run` and `systemctl`.
    Custom(Arc<dyn Launcher + Send + Sync>),
}

impl HostLauncher {
    /// The launcher `PF_HOST_LAUNCHER` names. Unset is `detached`. Any other value is an error that
    /// names it, so a typo never starts a different launcher than the operator meant.
    pub fn from_env_value(value: Option<&str>) -> Result<HostLauncher, String> {
        match value {
            None | Some("") | Some("detached") => Ok(HostLauncher::Detached),
            Some("systemd") => Ok(HostLauncher::Systemd),
            Some(other) => Err(format!(
                "PF_HOST_LAUNCHER is {other:?}; it is systemd or detached (unset means detached)"
            )),
        }
    }
}

/// Faults a test injects into the service, as `StartFaults` does for the start card.
#[derive(Default)]
pub struct HostFaults {
    /// After the launcher's start, return an error without cleaning up and without the registry
    /// row: the controller died between the host's start and the commit of its row.
    pub crash_after_start: AtomicBool,
}

// ------------------------------------------------------------------ the server name

/// 12 lower-case hex digits.
fn is_server_name(text: &str) -> bool {
    text.len() == 12
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn server_file(state: &Path) -> PathBuf {
    state.join("hosts").join("server")
}

/// The host server name of a state directory, when one was made: `<state>/hosts/server`. It makes
/// nothing. `None` when the file does not exist, and then no host of this server can exist.
pub fn read_host_server_name(state: &Path) -> Result<Option<String>, String> {
    let file = server_file(state);
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let name = text.trim();
            if is_server_name(name) {
                Ok(Some(name.to_string()))
            } else {
                Err(format!(
                    "{} does not hold a host server name (12 hex digits)",
                    file.display()
                ))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

/// The host server name of a state directory (G9). It is 12 lower-case hex digits in
/// `<state>/hosts/server`, written once with a create-new write and never changed. When the file is
/// first made and `<state>/api/cert.pem` exists, the name is the first 12 hex digits of the server
/// id; otherwise it is 12 hex digits from the random device. It has no hyphen, so `names::parse_unit`
/// cannot misread it, and it is short enough for a socket path.
pub fn host_server_name(state: &Path, random_device: &Path) -> Result<String, String> {
    if let Some(name) = read_host_server_name(state)? {
        return Ok(name);
    }
    let dir = state.join("hosts");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let from_cert = crate::store_verbs::server_id(state)?;
    let name = match from_cert {
        Some(id) => id[..12].to_string(),
        None => hex(&random_bytes(random_device, 6)?),
    };
    let file = server_file(state);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
    {
        Ok(mut f) => f
            .write_all(format!("{name}\n").as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", file.display()))?,
        // Another controller made it first: its name stands.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return read_host_server_name(state)?
                .ok_or_else(|| format!("{} vanished", file.display()));
        }
        Err(e) => return Err(format!("cannot make {}: {e}", file.display())),
    }
    Ok(name)
}

// ------------------------------------------------------------------ the service

/// What the service needs from the controller's config.
pub struct HostConfig {
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub launcher: HostLauncher,
    /// The `pf-host` binary, when a test or the operator names one.
    pub host_bin: Option<PathBuf>,
    /// The controller's own binary: `pf-host` is looked for beside it.
    pub binary: PathBuf,
    pub proc_root: PathBuf,
    pub random_device: PathBuf,
}

/// The files of one incarnation, under `<state>/hosts/<server>/`.
struct Files {
    record: PathBuf,
    key: PathBuf,
}

pub struct HostService {
    me: Weak<HostService>,
    controller: Weak<Controller>,
    cfg: HostConfig,
    launcher: Arc<dyn Launcher + Send + Sync>,
    server: String,
    /// Why the server name could not be made (a random source that fails, an unwritable state
    /// directory). The controller still opens, as `fail_closed_key.rs` needs with a failing random
    /// source: a launch is refused with this reason, a rebind answers unreachable, and the orphan
    /// sweep is skipped.
    server_error: Option<String>,
    /// One launch at a time: the incarnation a launch takes is the next free one.
    launch_lock: Mutex<()>,
    /// Why the orphan sweep did not run, for `status`.
    sweep_skipped: Mutex<Option<String>>,
    pub faults: HostFaults,
}

/// A short, stable name of a session: `s` and the first 16 hex digits of the SHA-256 of
/// `<kind>:<key>`. With a 12-digit server it keeps both socket paths under 107 bytes for any
/// incarnation below 10^6. The registry row holds the full key.
pub fn session_name(kind: &str, key: &str) -> String {
    let hash = sha256::sha256_hex(format!("{kind}:{key}").as_bytes());
    format!("s{}", &hash[..16])
}

fn secret_stand_in(value: &str) -> String {
    format!("sha256:{}", sha256::sha256_hex(value.as_bytes()))
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn hold(msg: impl Into<String>) -> HostError {
    HostError(msg.into())
}

/// One admitted command of the service: what [`HostService::admit`] commits.
struct Commit<'a> {
    kind: &'static str,
    command_id: &'a str,
    content: String,
    effects: Vec<Effect>,
    subject: &'a str,
    line: String,
}

impl HostService {
    pub fn new(
        controller: Weak<Controller>,
        cfg: HostConfig,
        server: Result<String, String>,
    ) -> Arc<HostService> {
        let (server, server_error) = match server {
            Ok(name) => (name, None),
            Err(why) => (String::new(), Some(why)),
        };
        let launcher: Arc<dyn Launcher + Send + Sync> = match &cfg.launcher {
            HostLauncher::Detached => Arc::new(Detached::new(cfg.runtime_dir.clone())),
            HostLauncher::Systemd => Arc::new(Systemd::new(cfg.runtime_dir.clone())),
            HostLauncher::Custom(l) => l.clone(),
        };
        Arc::new_cyclic(|me| HostService {
            me: me.clone(),
            controller,
            cfg,
            launcher,
            server,
            server_error,
            launch_lock: Mutex::new(()),
            sweep_skipped: Mutex::new(None),
            faults: HostFaults::default(),
        })
    }

    fn arc(&self) -> Arc<HostService> {
        self.me.upgrade().expect("a host service lives in an Arc")
    }

    /// `systemd` or `detached`, as a record and `status` write it.
    pub fn launcher_name(&self) -> &'static str {
        self.launcher.name()
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Why the orphan sweep was skipped, when it was.
    pub fn sweep_skipped(&self) -> Option<String> {
        locked(&self.sweep_skipped).clone()
    }

    fn ctl(&self) -> Result<Arc<Controller>, HostError> {
        self.controller
            .upgrade()
            .ok_or_else(|| hold("the controller is stopping"))
    }

    fn files(&self, session: &str, incarnation: u64) -> Files {
        let dir = self.cfg.state_dir.join("hosts").join(&self.server);
        Files {
            record: dir.join(format!("{session}-{incarnation}.record.json")),
            key: dir.join(format!("{session}-{incarnation}.key")),
        }
    }

    fn files_dir(&self) -> PathBuf {
        self.cfg.state_dir.join("hosts").join(&self.server)
    }

    /// Where the host of a unit binds its socket, as the launcher makes it.
    fn socket_of(&self, session: &str, incarnation: u64) -> Result<PathBuf, HostError> {
        let unit = names::unit_name(&self.server, session, incarnation).map_err(hold)?;
        Ok(if self.launcher.name() == "systemd" {
            self.cfg.runtime_dir.join(&unit).join("host.sock")
        } else {
            names::detached_dir(&self.cfg.runtime_dir, &self.server, session, incarnation)
                .join("host.sock")
        })
    }

    /// The `pf-host` binary: `PF_HOST_BIN`, else `pf-host` beside the controller's own binary,
    /// canonicalized so an installed symlink resolves to its content-hash path.
    fn host_bin(&self) -> Result<PathBuf, HostError> {
        let named = self
            .cfg
            .host_bin
            .clone()
            .or_else(|| {
                std::env::var_os("PF_HOST_BIN")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(|| self.cfg.binary.parent().map(|dir| dir.join("pf-host")))
            .ok_or_else(|| hold("no pf-host binary: set PF_HOST_BIN"))?;
        let canonical = std::fs::canonicalize(&named).map_err(|e| {
            hold(format!(
                "no pf-host binary at {} ({e}); set PF_HOST_BIN or install pf-host beside the controller",
                named.display()
            ))
        })?;
        if !canonical.is_file() {
            return Err(hold(format!(
                "the pf-host binary {} is not a file",
                canonical.display()
            )));
        }
        Ok(canonical)
    }

    /// The controller's own cgroup, from `<proc_root>/self/cgroup`: the `0::` line.
    pub(super) fn own_cgroup(&self) -> Option<String> {
        peer::parse_cgroup(
            &std::fs::read_to_string(self.cfg.proc_root.join("self").join("cgroup")).ok()?,
        )
    }

    /// The cgroup the user manager names for a unit, whose last component is `<unit>.service`.
    fn unit_cgroup(&self, unit: &str) -> Result<String, String> {
        let out = std::process::Command::new("systemctl")
            .args(["--user", "show", "-p", "ControlGroup", "--value"])
            .arg(format!("{unit}.service"))
            .env("XDG_RUNTIME_DIR", &self.cfg.runtime_dir)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("cannot run systemctl to read the cgroup of {unit}: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || text.is_empty() {
            return Err(format!("the user manager names no cgroup for {unit}"));
        }
        if text.rsplit('/').next() != Some(&format!("{unit}.service")) {
            return Err(format!(
                "the cgroup {text} of {unit} does not end in {unit}.service"
            ));
        }
        Ok(text)
    }

    // -------------------------------------------------------------- ledger lines and commits

    /// Commit one admitted command with one ledger line. A replay of the same command is fine.
    fn admit(&self, ctl: &Controller, c: Commit<'_>) -> Result<(), String> {
        ctl.run_command(
            c.kind,
            c.command_id,
            c.content,
            Plan::Admit {
                effects: c.effects,
                subject: c.subject.to_string(),
                line: c.line,
            },
        )
        .map(|_| ())
    }

    /// One line for a refused attach: the session, the incarnation, the unit and the check that
    /// failed. No key text.
    fn line_refused(&self, ctl: &Controller, key: &str, unit: &str, incarnation: u64, check: &str) {
        let id = format!("host-refused-{unit}-{}", unique());
        let detail = format!(
            " session={} incarnation={incarnation} unit={unit} check={}",
            field(key),
            field(check)
        );
        let _ = ctl.run_command(
            "host_attach",
            &id,
            json!({"unit": unit, "check": check}).to_string(),
            Plan::RefuseDetailed {
                why: format!("the host of {unit} failed the attach check: {check}"),
                detail,
            },
        );
    }

    /// End a live host row with an exit. A row that has ended already is left as it is.
    fn end_row(&self, ctl: &Controller, row: &HostRow, exit: &Value, why: &str) {
        if ctl
            .store()
            .host(&row.unit)
            .ok()
            .flatten()
            .is_none_or(|r| r.ended_at.is_some())
        {
            return;
        }
        let exit_text = canonical_json(exit);
        let id = format!("host-end-{}", row.unit);
        let line = format!(
            "host_end {} ended session={} incarnation={} reason={} command={id}",
            row.unit,
            field(&row.session_id),
            row.incarnation,
            field(why)
        );
        let content = json!({"unit": row.unit, "exit": exit_text}).to_string();
        let ended = Effect::HostEnded {
            unit: row.unit.clone(),
            ended_at: now(),
            exit: exit_text,
        };
        if let Err(e) = self.admit(
            ctl,
            Commit {
                kind: "host_end",
                command_id: &id,
                content,
                effects: vec![ended],
                subject: &row.unit,
                line,
            },
        ) {
            eprintln!("pulse-factory: cannot record the end of {}: {e}", row.unit);
        }
    }

    /// What a host left: its exit file, or "ended, cause unknown" when the unit is gone and left
    /// none (a kill of the whole cgroup also kills the stop-post step; a killed detached host
    /// leaves none).
    fn what_it_left(&self, unit: &str) -> Value {
        match std::fs::read_to_string(names::exit_path(&self.cfg.state_dir, unit))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(body) => body,
            None => json!({
                "written_by": "controller",
                "reason": "unknown",
                "note": "the unit is gone and left no exit file",
            }),
        }
    }

    fn remove_files(&self, session: &str, incarnation: u64) {
        let files = self.files(session, incarnation);
        let _ = std::fs::remove_file(files.key);
        let _ = std::fs::remove_file(files.record);
    }

    // -------------------------------------------------------------- the attach and its checks

    /// Connect with the key and read the `hello`, within `wait`. The client's own timeout is 5 s,
    /// so the call runs on a thread, and a late answer is dropped.
    fn connect(&self, socket: &Path, key: &str, wait: Duration) -> Result<(Conn, Hello), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (socket, key) = (socket.to_path_buf(), key.to_string());
        std::thread::spawn(move || {
            let _ = tx.send(client::connect(&socket, &key));
        });
        match rx.recv_timeout(wait) {
            Ok(Ok(pair)) => Ok(pair),
            Ok(Err(e)) => Err(match e {
                ClientError::Superseded => "another controller attached to the host".to_string(),
                other => other.to_string(),
            }),
            Err(_) => Err(format!("no valid hello within {} ms", wait.as_millis())),
        }
    }

    /// The controller's half of the attach check (Q4, session-trust). `Err` names the check.
    #[allow(clippy::too_many_arguments)]
    fn check_hello(
        &self,
        conn: &Conn,
        hello: &Hello,
        unit: &str,
        session: &str,
        incarnation: u64,
        record_sha256: &str,
        invocation_id: &str,
    ) -> Result<(), String> {
        if hello.session != session {
            return Err(format!(
                "session: the host names {} and the row names {session}",
                hello.session
            ));
        }
        if hello.incarnation != incarnation {
            return Err(format!(
                "incarnation: the host names {} and the row names {incarnation}",
                hello.incarnation
            ));
        }
        if hello.record_sha256 != record_sha256 {
            return Err("record_sha256: the host was launched from another record".to_string());
        }
        if self.launcher.name() == "systemd" {
            if hello.invocation_id != invocation_id {
                return Err("invocation_id: the unit's invocation is not the row's".to_string());
            }
            let expected = self
                .unit_cgroup(unit)
                .map_err(|why| format!("cgroup: {why}"))?;
            conn.peer_check(&expected)
                .map_err(|e| format!("cgroup: {e}"))?;
        }
        Ok(())
    }

    /// Wrap an attached connection as the port's handle.
    fn handle_of(
        &self,
        conn: Conn,
        hello: &Hello,
        unit: &str,
        session: &str,
        incarnation: u64,
        record: Value,
    ) -> HostHandle {
        let exit = hello.exit.as_ref().map(|e| HostExit {
            code: e.code,
            signal: e.signal,
            at: e.at,
        });
        let session_impl = Arc::new(ServiceSession {
            service: self.arc(),
            conn: Mutex::new(conn),
            unit: unit.to_string(),
            session: session.to_string(),
            incarnation,
            exit: Mutex::new(exit.clone()),
        });
        HostHandle::new(
            unit.to_string(),
            u32::try_from(incarnation).unwrap_or(u32::MAX),
            record,
            hello.child_pid,
            exit,
            session_impl,
        )
    }

    // -------------------------------------------------------------- Q9: never two copies

    /// Stop every older incarnation of this session and wait until each one ended. The hosts are
    /// the live rows of the session and the launcher's own list, so a host that started and never
    /// got a row ends too.
    fn end_older(
        &self,
        ctl: &Controller,
        kind: &str,
        key: &str,
        session: &str,
        wait: Duration,
    ) -> Result<(), String> {
        let rows: Vec<HostRow> = ctl
            .store()
            .live_hosts()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|r| r.session_kind == kind && r.session_id == key)
            .collect();
        let mut units: Vec<String> = rows.iter().map(|r| r.unit.clone()).collect();
        if let Ok(listed) = self.launcher.list(&self.server) {
            for h in listed {
                if h.session == session && h.state != HostState::Gone && !units.contains(&h.unit) {
                    units.push(h.unit);
                }
            }
        }
        for unit in units {
            let stop = self.launcher.stop(&unit);
            let end = Instant::now() + wait;
            let gone = loop {
                let exit_file = names::exit_path(&self.cfg.state_dir, &unit).exists();
                if exit_file || self.launcher.state(&unit) == HostState::Gone {
                    break true;
                }
                if Instant::now() >= end {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            if !gone {
                let why = match stop {
                    Err(e) => e.to_string(),
                    Ok(()) => "it is still running".to_string(),
                };
                return Err(format!("an older incarnation {unit} did not stop: {why}"));
            }
            if let Some(row) = rows.iter().find(|r| r.unit == unit) {
                let exit = self.what_it_left(&unit);
                self.end_row(ctl, row, &exit, "replaced by the next incarnation");
                if let Some((s, inc)) = names::parse_unit(&self.server, &unit) {
                    self.remove_files(&s, inc);
                }
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- launch

    fn launch_inner(&self, l: HostLaunch) -> Result<HostHandle, String> {
        if let Some(why) = &self.server_error {
            return Err(format!("no host server name can be made: {why}"));
        }
        let ctl = self.ctl().map_err(|e| e.0)?;
        if !SESSION_KINDS.contains(&l.session_kind.as_str()) {
            return Err(format!(
                "session kind {:?} is not one of {}",
                l.session_kind,
                SESSION_KINDS.join(", ")
            ));
        }
        if l.argv.first().is_none_or(String::is_empty) {
            return Err("the launch has no command".to_string());
        }
        let _one_at_a_time = locked(&self.launch_lock);
        let (kind, key) = (l.session_kind.as_str(), l.session_key.as_str());
        let session = session_name(kind, key);
        let project = l
            .run_binding
            .get("run_id")
            .and_then(Value::as_str)
            .and_then(|run| ctl.run_project_of(run));
        let settings = ctl.session_settings(project.as_deref());
        let attach_wait = Duration::from_millis(settings.attach_wait_ms.0);

        // 1. Never two copies (Q9).
        self.end_older(&ctl, kind, key, &session, attach_wait * 10)?;

        // 2. The incarnation: the first number with no launch record.
        let mut incarnation = 1u32;
        while ctl
            .store()
            .newest_launch_record(kind, key, incarnation)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            incarnation += 1;
        }
        let inc = u64::from(incarnation);
        let unit = names::unit_name(&self.server, &session, inc)?;
        let fill = |text: &str| text.replace("{unit}", &unit);

        // 3. The record body (G3).
        let mut env: BTreeMap<String, String> =
            l.env.iter().map(|(k, v)| (k.clone(), fill(v))).collect();
        // The hook waits for a controller as long as the launch's resolved `session.hook_wait_seconds`
        // (AC-8, Q8): the seat's `hook` reads it from its environment.
        if kind == "seat" {
            env.insert(
                "PF_HOOK_WAIT_SECONDS".to_string(),
                settings.hook_wait_seconds.0.to_string(),
            );
        }
        for (name, value) in &l.secrets {
            env.insert(name.clone(), secret_stand_in(value));
        }
        let mut body_settings = l.settings.clone();
        if !body_settings.is_object() {
            body_settings = json!({});
        }
        if let Some(map) = body_settings.as_object_mut() {
            for name in l.secrets.keys() {
                if let Some(v) = map.get_mut(&format!("env.{name}")) {
                    let plain = v.as_str().unwrap_or_default().to_string();
                    *v = Value::String(secret_stand_in(&plain));
                }
            }
            for (k, v) in settings.as_record() {
                map.insert(k.to_string(), v);
            }
        }
        let rules_sha256 = l
            .settings
            .get("rules_file")
            .and_then(Value::as_str)
            .and_then(|p| std::fs::read(p).ok())
            .map(|bytes| sha256::sha256_hex(&bytes))
            .unwrap_or_default();
        let transport = match l.transport {
            HostTransport::Pipes => json!({"kind": "pipes"}),
            HostTransport::Terminal { rows, cols } => {
                json!({"kind": "pty", "rows": rows, "cols": cols})
            }
        };
        let body = json!({
            "argv": l.argv.iter().map(|a| fill(a)).collect::<Vec<_>>(),
            "cwd": l.cwd,
            "env": env,
            "secret_names": l.secrets.keys().collect::<Vec<_>>(),
            "transport": transport,
            "runtime": l.runtime,
            "settings": body_settings,
            "plugins": [],
            "skills": [],
            "rules_sha256": rules_sha256,
            "run_binding": l.run_binding,
            "resume": l.resume,
        });
        let (canonical, record_sha256) =
            NewLaunchRecord::seal(&body.to_string()).map_err(|e| e.to_string())?;
        let socket = self.socket_of(&session, inc).map_err(|e| e.0)?;
        let record_cmd = format!("host-record-{unit}");
        self.admit(&ctl, Commit { kind: "host_record", command_id: &record_cmd, content: json!({"unit": unit, "record_sha256": record_sha256}).to_string(), effects: vec![Effect::LaunchRecordWrite(NewLaunchRecord {
                session_kind: kind.to_string(),
                session_id: key.to_string(),
                incarnation,
                revision: 1,
                launcher: self.launcher.name().to_string(),
                host_unit: unit.clone(),
                host_socket: socket.display().to_string(),
                record: canonical.clone(),
                record_sha256: record_sha256.clone(),
                created_at: now(),
            })], subject: &unit, line: format!(
                "host_record {unit} written session={} incarnation={incarnation} revision=1 sha256={record_sha256} command={record_cmd}",
                field(key)
            ) })?;

        // 4. The files the child expects, the record file, and 5. the attach key.
        for f in &l.files {
            let path = PathBuf::from(fill(f));
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
            }
            if !path.exists() {
                std::fs::write(&path, b"")
                    .map_err(|e| format!("cannot make {}: {e}", path.display()))?;
            }
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(self.files_dir())
            .map_err(|e| format!("cannot make {}: {e}", self.files_dir().display()))?;
        let files = self.files(&session, inc);
        write_private(&files.record, canonical.as_bytes())?;
        let attach_key = hex(&random_bytes(&self.cfg.random_device, 32)?);
        if let Err(e) = write_private(&files.key, attach_key.as_bytes()) {
            let _ = std::fs::remove_file(&files.record);
            return Err(e);
        }

        // 6. The start.
        let host_bin = match self.host_bin() {
            Ok(b) => b,
            Err(e) => {
                self.remove_files(&session, inc);
                return Err(e.0);
            }
        };
        let mut spec = StartSpec::new(
            &self.server,
            &session,
            inc,
            files.record.clone(),
            self.cfg.state_dir.clone(),
            host_bin.clone(),
            attach_key.clone(),
        );
        spec.secrets = l.secrets.clone();
        spec.replay_bytes = usize::try_from(settings.replay_bytes.0).unwrap_or(4 << 20);
        spec.attach_wait = attach_wait;
        spec.exit_hold = Duration::from_secs(settings.exit_hold_minutes.0 * 60);
        spec.controller_cgroup = if self.launcher.name() == "systemd" {
            self.own_cgroup()
        } else {
            None
        };
        let started = match self.launcher.start(&spec) {
            Ok(s) => s,
            Err(e) => {
                // The record row stays: it is write-once.
                self.remove_files(&session, inc);
                return Err(e.to_string());
            }
        };
        if self.faults.crash_after_start.load(Ordering::SeqCst) {
            return Err(format!(
                "the controller stopped after it started {} (a fault a test injected)",
                started.unit
            ));
        }

        // 7. Attach and check, then the registry row.
        let fail = |why: String, check: Option<&str>| -> String {
            let _ = self.launcher.stop(&started.unit);
            if let Some(check) = check {
                self.line_refused(&ctl, key, &started.unit, inc, check);
            }
            let _ = std::fs::remove_file(&files.key);
            why
        };
        let (conn, hello) = match self.connect(
            &started.socket,
            &attach_key,
            attach_wait.max(Duration::from_secs(1)),
        ) {
            Ok(pair) => pair,
            Err(why) => {
                return Err(fail(
                    format!("cannot attach to {}: {why}", started.unit),
                    Some(&format!("attach: {why}")),
                ));
            }
        };
        if let Err(check) = self.check_hello(
            &conn,
            &hello,
            &started.unit,
            &session,
            inc,
            &record_sha256,
            &started.invocation_id,
        ) {
            return Err(fail(
                format!("the host of {} was refused: {check}", started.unit),
                Some(&check),
            ));
        }
        let host_sha256 = sha256::sha256_hex(&std::fs::read(&host_bin).unwrap_or_default());
        let start_cmd = format!("host-start-{}", started.unit);
        let started_row = NewHost {
            unit: started.unit.clone(),
            session_kind: kind.to_string(),
            session_id: key.to_string(),
            incarnation,
            launched_revision: 1,
            launched_record_sha256: record_sha256.clone(),
            launcher: self.launcher.name().to_string(),
            socket: started.socket.display().to_string(),
            invocation_id: started.invocation_id.clone(),
            host_binary: host_bin.display().to_string(),
            host_sha256,
            protocol: hello.proto,
            started_at: now(),
        };
        // 8. One ledger line for the attach: the commit's own.
        if let Err(why) = self.admit(&ctl, Commit { kind: "host_attach", command_id: &start_cmd, content: json!({"unit": started.unit, "record_sha256": record_sha256}).to_string(), effects: vec![Effect::HostStarted(started_row)], subject: &started.unit, line: format!(
                "host_attach {} attached session={} incarnation={incarnation} launcher={} command={start_cmd}",
                started.unit,
                field(key),
                self.launcher.name()
            ) }) {
            return Err(fail(
                format!("cannot register the host {}: {why}", started.unit),
                None,
            ));
        }
        let record: Value = serde_json::from_str(&canonical).unwrap_or(Value::Null);
        Ok(self.handle_of(conn, &hello, &started.unit, &session, inc, record))
    }

    // -------------------------------------------------------------- rebind (tier 1)

    fn tier1_line(&self, ctl: &Controller, row: &HostRow, outcome: &str, reason: &str) {
        let id = format!("host-tier1-{}-{}", row.unit, unique());
        let line = format!(
            "host_tier1 {} {outcome} session={} incarnation={} reason={} command={id}",
            row.unit,
            field(&row.session_id),
            row.incarnation,
            field(reason)
        );
        let content = json!({"unit": row.unit, "outcome": outcome}).to_string();
        if let Err(e) = self.admit(
            ctl,
            Commit {
                kind: "host_tier1",
                command_id: &id,
                content,
                effects: Vec::new(),
                subject: &row.unit,
                line,
            },
        ) {
            eprintln!(
                "pulse-factory: cannot write the tier-1 line of {}: {e}",
                row.unit
            );
        }
    }

    fn rebind_inner(&self, unit: &str) -> Rebind {
        if let Some(why) = &self.server_error {
            return Rebind::Unreachable {
                why: format!("no host server name can be made: {why}"),
            };
        }
        let Ok(ctl) = self.ctl() else {
            return Rebind::Unreachable {
                why: "the controller is stopping".to_string(),
            };
        };
        let row = match ctl.store().host(unit) {
            Ok(Some(row)) => row,
            Ok(None) => {
                return Rebind::Lost {
                    exit: json!(format!("the registry has no host {unit}")),
                };
            }
            Err(e) => {
                return Rebind::Unreachable {
                    why: format!("cannot read the registry: {e}"),
                };
            }
        };
        if row.ended_at.is_some() {
            let exit = row
                .exit
                .as_deref()
                .and_then(|t| serde_json::from_str(t).ok())
                .unwrap_or(Value::Null);
            return Rebind::Lost { exit };
        }
        let state = self.launcher.state(unit);
        if state == HostState::Gone {
            let exit = self.what_it_left(unit);
            self.end_row(&ctl, &row, &exit, "the unit is gone");
            self.remove_files_of(unit);
            let why = exit
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("gone")
                .to_string();
            self.tier1_line(&ctl, &row, "lost", &why);
            return Rebind::Lost { exit };
        }
        let unreachable = |why: String| {
            self.tier1_line(&ctl, &row, "unreachable", &why);
            Rebind::Unreachable { why }
        };
        let Some((session, inc)) = names::parse_unit(&self.server, unit) else {
            return unreachable(format!("the unit {unit} is not of this server"));
        };
        let files = self.files(&session, inc);
        let Ok(key) = std::fs::read_to_string(&files.key) else {
            return unreachable(format!("no key file for {unit}"));
        };
        let settings = ctl.session_settings(
            row.session_id
                .split_once('/')
                .and_then(|(run, _)| ctl.run_project_of(run))
                .as_deref(),
        );
        let wait = Duration::from_millis(settings.attach_wait_ms.0);
        let (conn, hello) = match self.connect(Path::new(&row.socket), key.trim(), wait) {
            Ok(pair) => pair,
            Err(why) => return unreachable(format!("{state:?} unit, no attach: {why}")),
        };
        if let Err(check) = self.check_hello(
            &conn,
            &hello,
            unit,
            &session,
            inc,
            &row.launched_record_sha256,
            &row.invocation_id,
        ) {
            self.line_refused(&ctl, &row.session_id, unit, inc, &check);
            return unreachable(format!("refused: {check}"));
        }
        let record = ctl
            .store()
            .launch_record(
                &row.session_kind,
                &row.session_id,
                row.incarnation,
                row.launched_revision,
            )
            .ok()
            .flatten()
            .and_then(|r| serde_json::from_str::<Value>(&r.record).ok())
            .unwrap_or(Value::Null);
        // A rebind is an attach: one `host_attach` line, as the launch writes (the audit trail has
        // one line per attach), and then the tier-1 line for the reconcile outcome.
        let attach_cmd = format!("host-rebind-{unit}-{}", unique());
        let attach_line = format!(
            "host_attach {unit} attached session={} incarnation={} launcher={} command={attach_cmd}",
            field(&row.session_id),
            row.incarnation,
            row.launcher
        );
        if let Err(e) = self.admit(
            &ctl,
            Commit {
                kind: "host_attach",
                command_id: &attach_cmd,
                content: json!({"unit": unit, "record_sha256": row.launched_record_sha256})
                    .to_string(),
                effects: Vec::new(),
                subject: unit,
                line: attach_line,
            },
        ) {
            eprintln!("pulse-factory: cannot write the attach line of {unit}: {e}");
        }
        self.tier1_line(&ctl, &row, "survived", "the host answered its hello");
        Rebind::Survived(self.handle_of(conn, &hello, unit, &session, inc, record))
    }

    fn remove_files_of(&self, unit: &str) {
        if let Some((s, inc)) = names::parse_unit(&self.server, unit) {
            self.remove_files(&s, inc);
        }
    }

    // -------------------------------------------------------------- the orphan sweep

    /// The sweep, once, after every run's start-up and before the socket binds (Approach 6).
    pub fn sweep_orphans(&self) {
        let Ok(ctl) = self.ctl() else { return };
        if let Some(why) = &self.server_error {
            self.skip(&ctl, &format!("no host server name can be made: {why}"));
            return;
        }
        let rows = match ctl.store().live_hosts() {
            Ok(r) => r,
            Err(e) => {
                self.skip(&ctl, &format!("cannot read the registry: {e}"));
                return;
            }
        };
        // The hosts of the configured launcher, and the detached directory always.
        let mut listed = match self.launcher.list(&self.server) {
            Ok(l) => l,
            Err(e) => {
                self.skip(&ctl, &e.to_string());
                return;
            }
        };
        let detached = Detached::new(self.cfg.runtime_dir.clone());
        if self.launcher.name() != "detached"
            && let Ok(more) = detached.list(&self.server)
        {
            listed.extend(more);
        }

        // A live row whose session is no longer live is an orphan. Each orphan gets one line, so
        // the units handled here are not handled again below.
        let mut handled: Vec<String> = Vec::new();
        for row in &rows {
            // A failed read is not "no session claims the host": the sweep stops, it never guesses.
            match self.claimed(&ctl, row) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    self.skip(&ctl, &format!("cannot read the sessions: {e}"));
                    return;
                }
            }
            self.stop_orphan(
                &ctl,
                &row.unit,
                Some(row),
                "no live session claims the host",
            );
            handled.push(row.unit.clone());
        }
        // The rows after the orphans above ended. A failed read is not an empty registry: with an
        // empty list every listed host would stop as row-less and every key file would go.
        let live_after: Vec<String> = match ctl.store().live_hosts() {
            Ok(r) => r.into_iter().map(|r| r.unit).collect(),
            Err(e) => {
                self.skip(&ctl, &format!("cannot read the registry: {e}"));
                return;
            }
        };
        for host in &listed {
            if live_after.contains(&host.unit) || handled.contains(&host.unit) {
                continue;
            }
            match host.state {
                HostState::Gone => {
                    // A stale detached directory: nothing runs in it.
                    let dir = names::detached_dir(
                        &self.cfg.runtime_dir,
                        &self.server,
                        &host.session,
                        host.incarnation,
                    );
                    if dir.is_dir() {
                        let _ = std::fs::remove_dir_all(&dir);
                        self.orphan_line(
                            &ctl,
                            &host.unit,
                            &host.session,
                            host.incarnation,
                            "removed a stale detached directory",
                        );
                    }
                }
                _ => {
                    // A rows-less host stops with whichever launcher lists it.
                    self.stop_orphan(&ctl, &host.unit, None, "no live host row names the unit");
                }
            }
        }
        // Key files and record files that no live row names.
        if let Ok(entries) = std::fs::read_dir(self.files_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let stem = name
                    .strip_suffix(".key")
                    .or_else(|| name.strip_suffix(".record.json"));
                let Some(stem) = stem else { continue };
                let Some((session, inc)) = stem.rsplit_once('-') else {
                    continue;
                };
                let Ok(inc) = inc.parse::<u64>() else {
                    continue;
                };
                let Ok(unit) = names::unit_name(&self.server, session, inc) else {
                    continue;
                };
                if !live_after.contains(&unit) {
                    let _ = std::fs::remove_file(entry.path());
                    self.orphan_line(&ctl, &unit, session, inc, &format!("removed {name}"));
                }
            }
        }
    }

    fn skip(&self, ctl: &Controller, why: &str) {
        *locked(&self.sweep_skipped) = Some(why.to_string());
        let id = format!("host-sweep-skipped-{}", unique());
        let line = format!(
            "host_orphan sweep skipped reason={} command={id}",
            field(&format!("orphan sweep skipped: {why}"))
        );
        let _ = self.admit(
            ctl,
            Commit {
                kind: "host_orphan",
                command_id: &id,
                content: json!({"skipped": why}).to_string(),
                effects: Vec::new(),
                subject: "sweep",
                line,
            },
        );
        eprintln!("pulse-factory: orphan sweep skipped: {why}");
    }

    fn orphan_line(&self, ctl: &Controller, unit: &str, session: &str, inc: u64, reason: &str) {
        let id = format!("host-orphan-{unit}-{}", unique());
        let line = format!(
            "host_orphan {unit} orphan session={} incarnation={inc} reason={} command={id}",
            field(session),
            field(reason)
        );
        let _ = self.admit(
            ctl,
            Commit {
                kind: "host_orphan",
                command_id: &id,
                content: json!({"unit": unit, "reason": reason}).to_string(),
                effects: Vec::new(),
                subject: unit,
                line,
            },
        );
    }

    /// Whether a live live seat session (or chat session) claims the host of this row. A failed
    /// read is an `Err`, never `false`: `false` stops the host.
    fn claimed(&self, ctl: &Controller, row: &HostRow) -> Result<bool, String> {
        match row.session_kind.as_str() {
            "seat" => {
                let Some((run, unit_id)) = row.session_id.split_once('/') else {
                    return Ok(false);
                };
                let sessions = ctl.store().sessions(run).map_err(|e| e.to_string())?;
                Ok(sessions.iter().any(|s| {
                    !s.ended
                        && s.unit_id == unit_id
                        && s.process_ref.as_deref().is_some_and(|r| {
                            // The router tags a reference as `<runtime>:<unit>`.
                            r.split_once(':').map_or(r, |(_, inner)| inner) == row.unit
                        })
                }))
            }
            "chat" => {
                let chats = ctl
                    .store()
                    .live_chat_sessions()
                    .map_err(|e| e.to_string())?;
                Ok(chats.iter().any(|c| c.id == row.session_id))
            }
            _ => Ok(false),
        }
    }

    fn stop_orphan(&self, ctl: &Controller, unit: &str, row: Option<&HostRow>, why: &str) {
        let _ = self.launcher.stop(unit);
        // A detached host of another launcher's list stops with the detached launcher too.
        let _ = Detached::new(self.cfg.runtime_dir.clone()).stop(unit);
        let (session, inc) =
            names::parse_unit(&self.server, unit).unwrap_or_else(|| (unit.to_string(), 0));
        if let Some(row) = row {
            let end = Instant::now() + Duration::from_secs(5);
            while self.launcher.state(unit) != HostState::Gone && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
            let exit = self.what_it_left(unit);
            self.end_row(ctl, row, &exit, "orphan");
        }
        self.remove_files(&session, inc);
        self.orphan_line(ctl, unit, &session, inc, why);
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot make {}: {e}", path.display()))?;
    f.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A number that differs between two calls in one process and across restarts.
fn unique() -> String {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{}", N.fetch_add(1, Ordering::SeqCst))
}

impl SessionHosts for HostService {
    fn launch(&self, launch: HostLaunch) -> Result<HostHandle, HostError> {
        self.launch_inner(launch).map_err(HostError)
    }

    fn rebind(&self, unit: &str) -> Rebind {
        self.rebind_inner(unit)
    }

    fn launched_record(&self, unit: &str) -> Result<Option<Value>, HostError> {
        let ctl = self.ctl()?;
        let Some(row) = ctl.store().host(unit).map_err(|e| hold(e.to_string()))? else {
            return Ok(None);
        };
        Ok(ctl
            .store()
            .launch_record(
                &row.session_kind,
                &row.session_id,
                row.incarnation,
                row.launched_revision,
            )
            .map_err(|e| hold(e.to_string()))?
            .and_then(|r| serde_json::from_str(&r.record).ok()))
    }
}

// ------------------------------------------------------------------ one attached host

/// One attached host, as the port sees it. It ends the registry row when the adapter consumes the
/// exit or stops the host, and it removes the key and record files with the row.
struct ServiceSession {
    service: Arc<HostService>,
    conn: Mutex<Conn>,
    unit: String,
    session: String,
    incarnation: u64,
    /// The exit, once the host sent it.
    exit: Mutex<Option<HostExit>>,
}

fn host_err(e: ClientError) -> HostError {
    HostError(e.to_string())
}

impl ServiceSession {
    /// Commit the end of this host's row, if it is live.
    fn end_row(&self, exit: Value, why: &str) {
        let Ok(ctl) = self.service.ctl() else { return };
        if let Ok(Some(row)) = ctl.store().host(&self.unit) {
            self.service.end_row(&ctl, &row, &exit, why);
        }
    }
}

impl HostSession for ServiceSession {
    fn read(&self, from: u64) -> Result<(), HostError> {
        locked(&self.conn).read_from(from).map_err(host_err)
    }

    fn events(&self, wait: Duration) -> Result<Vec<HostEvent>, HostError> {
        let mut conn = locked(&self.conn);
        conn.set_timeout(wait.max(Duration::from_millis(1)));
        let mut out = Vec::new();
        loop {
            match conn.next_event() {
                Ok(Event::Out { at, data }) => out.push(HostEvent::Out { at, data }),
                Ok(Event::Gap { from, to }) => out.push(HostEvent::Gap { from, to }),
                Ok(Event::Exit(e)) => {
                    let exit = HostExit {
                        code: e.code,
                        signal: e.signal,
                        at: e.at,
                    };
                    *locked(&self.exit) = Some(exit.clone());
                    out.push(HostEvent::Exit(exit));
                }
                Ok(Event::Error { .. }) => {}
                Err(ClientError::Timeout) => return Ok(out),
                Err(e) if out.is_empty() => return Err(host_err(e)),
                Err(_) => return Ok(out),
            }
        }
    }

    fn ack(&self, to: u64) -> Result<(), HostError> {
        locked(&self.conn).ack(to).map_err(host_err)
    }

    fn input(&self, data: &[u8]) -> Result<(), HostError> {
        locked(&self.conn).input(data).map_err(host_err)
    }

    fn signal(&self, signal: HostSignal) -> Result<(), HostError> {
        let sig = match signal {
            HostSignal::Interrupt => SignalKind::Interrupt,
            HostSignal::Terminate => SignalKind::Terminate,
            HostSignal::Kill => SignalKind::Kill,
        };
        locked(&self.conn).signal(sig).map_err(host_err)
    }

    /// The exit was consumed: commit the row's end first, then tell the host, so the host never
    /// ends before the registry says it did (audit 7). The key and record files go with the row.
    fn exit_ack(&self) -> Result<(), HostError> {
        let exit = locked(&self.exit).clone();
        self.end_row(
            json!({
                "written_by": "controller",
                "reason": "exit_consumed",
                "exit": exit.map(|e| json!({"code": e.code, "signal": e.signal, "at": e.at})),
            }),
            "the exit was consumed",
        );
        let res = locked(&self.conn).exit_ack().map_err(host_err);
        self.service.remove_files(&self.session, self.incarnation);
        res
    }

    /// Stop the host through its launcher, which ends the child's group and waits. Safe to repeat.
    fn stop(&self) -> Result<(), HostError> {
        let stopped = self
            .service
            .launcher
            .stop(&self.unit)
            .map_err(|e| HostError(e.to_string()));
        let exit = self.service.what_it_left(&self.unit);
        self.end_row(exit, "the host was stopped");
        self.service.remove_files(&self.session, self.incarnation);
        stopped
    }
}
