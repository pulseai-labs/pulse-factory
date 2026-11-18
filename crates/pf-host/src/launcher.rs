//! The two launchers that start a host: a transient systemd user unit, and a detached process.
//!
//! Both write the host's secrets file first (`<XDG_RUNTIME_DIR>/pulse-factory/secrets/<unit>.json`,
//! directory 0700, file 0600) and pass its path. The host reads the file and deletes it before it
//! starts the child. A launch that fails deletes it too. No secret ever reaches an argument, a
//! unit property or the environment of the host.
//!
//! **The detached launcher is a fake** (`host-launcher-detached`, SPINE.md Q6, audit 10). It exists
//! for tests, for fenced seats and for a controller that runs as a plain process. It lacks:
//! - **a cgroup of its own.** The host, the controller and the seats share one cgroup, so a
//!   controller held by a group-kill unit (`KillMode=control-group`) takes its detached hosts down
//!   with it, and the host's cgroup peer check is vacuous. The attach key is the only peer check;
//! - **a stop-post step.** A host killed with `SIGKILL` leaves no exit file, so liveness is the PID
//!   and its start time from `/proc`.
//!
//! A failed `systemd-run` is an error naming its stderr. There is no fallback to the detached
//! launcher (SPINE.md Q6): a fallback would hide the failure.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::names;
use crate::peer;

/// The longest path a Unix socket can bind, `sun_path` less its terminator.
const SOCKET_PATH_MAX: usize = 107;
/// How long `start` waits for the host's socket.
const READY_WAIT: Duration = Duration::from_secs(10);
/// How long `stop` waits for a detached host to end before it kills it.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// A launch error, in words an operator can read.
#[derive(Debug)]
pub struct LaunchError(pub String);

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LaunchError {}

impl From<std::io::Error> for LaunchError {
    fn from(e: std::io::Error) -> LaunchError {
        LaunchError(e.to_string())
    }
}

/// What a launcher needs to start one host. `Debug` hides the attach key and the secret values, so
/// a logged spec leaks neither.
#[derive(Clone)]
pub struct StartSpec {
    /// The server's name in the unit (G9). The unit, the slice and the socket path carry it, so
    /// the caller picks one short enough for the socket path to fit 107 bytes.
    pub server: String,
    pub session: String,
    pub incarnation: u64,
    /// The launch record file the host reads.
    pub record_path: PathBuf,
    /// The persistent state directory: the exit file goes in `<state>/hosts/exits/`.
    pub state_dir: PathBuf,
    /// The cgroup of the controller. `None` skips the host's cgroup check.
    pub controller_cgroup: Option<String>,
    /// 32 random bytes as hex, made by the controller for this incarnation.
    pub attach_key: String,
    /// The child's secret variables. They go through the secrets file to the child's environment.
    pub secrets: BTreeMap<String, String>,
    pub replay_bytes: usize,
    pub attach_wait: Duration,
    pub exit_hold: Duration,
    /// The `pf-host` binary.
    pub host_bin: PathBuf,
}

impl fmt::Debug for StartSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartSpec")
            .field("server", &self.server)
            .field("session", &self.session)
            .field("incarnation", &self.incarnation)
            .field("record_path", &self.record_path)
            .field("state_dir", &self.state_dir)
            .field("controller_cgroup", &self.controller_cgroup)
            .field("attach_key", &"<hidden>")
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("replay_bytes", &self.replay_bytes)
            .field("attach_wait", &self.attach_wait)
            .field("exit_hold", &self.exit_hold)
            .field("host_bin", &self.host_bin)
            .finish()
    }
}

impl StartSpec {
    /// A spec with the built-in settings: a 4 MiB replay buffer, a 3 s attach wait and a 24 h hold.
    pub fn new(
        server: &str,
        session: &str,
        incarnation: u64,
        record_path: PathBuf,
        state_dir: PathBuf,
        host_bin: PathBuf,
        attach_key: String,
    ) -> StartSpec {
        StartSpec {
            server: server.to_string(),
            session: session.to_string(),
            incarnation,
            record_path,
            state_dir,
            controller_cgroup: None,
            attach_key,
            secrets: BTreeMap::new(),
            replay_bytes: 4 << 20,
            attach_wait: Duration::from_millis(3000),
            exit_hold: Duration::from_secs(24 * 3600),
            host_bin,
        }
    }
}

/// A host that started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    pub unit: String,
    pub socket: PathBuf,
    /// The host process.
    pub pid: u32,
    /// The unit's invocation ID. Empty under the detached launcher.
    pub invocation_id: String,
}

/// Whether a host exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostState {
    Live,
    Gone,
    /// The launcher could not tell: no user manager answered, or a host's files are half written.
    Unknown,
}

/// A host of one server, as `list` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRef {
    pub unit: String,
    pub session: String,
    pub incarnation: u64,
    pub state: HostState,
}

/// A way to start, find and stop hosts.
pub trait Launcher {
    /// The launcher's name, as a launch record and `status` write it.
    fn name(&self) -> &'static str;
    fn start(&self, spec: &StartSpec) -> Result<Started, LaunchError>;
    fn state(&self, unit: &str) -> HostState;
    /// Stop a host and its child. A host that is already gone is not an error.
    fn stop(&self, unit: &str) -> Result<(), LaunchError>;
    /// Every host of `server` that exists, for the orphan sweep.
    fn list(&self, server: &str) -> Result<Vec<HostRef>, LaunchError>;
}

/// The arguments of `pf-host run` for a spec. Both launchers build them here.
fn run_args(spec: &StartSpec, unit: &str, dir: &Path, secrets: &Path, kind: &str) -> Vec<String> {
    let mut a = vec![
        "run".to_string(),
        "--record".into(),
        spec.record_path.display().to_string(),
        "--session".into(),
        spec.session.clone(),
        "--incarnation".into(),
        spec.incarnation.to_string(),
        "--unit".into(),
        unit.to_string(),
        "--state-dir".into(),
        spec.state_dir.display().to_string(),
        "--dir".into(),
        dir.display().to_string(),
        "--secrets".into(),
        secrets.display().to_string(),
        "--replay-bytes".into(),
        spec.replay_bytes.to_string(),
        "--attach-wait-ms".into(),
        spec.attach_wait.as_millis().to_string(),
        "--exit-hold-ms".into(),
        spec.exit_hold.as_millis().to_string(),
        "--launcher".into(),
        kind.to_string(),
    ];
    if let Some(cg) = &spec.controller_cgroup {
        a.push("--controller-cgroup".into());
        a.push(cg.clone());
    }
    a
}

/// Write the secrets file for a start: the attach key and the child's secret variables.
fn write_secrets(runtime: &Path, unit: &str, spec: &StartSpec) -> Result<PathBuf, LaunchError> {
    let path = names::secrets_path(runtime, unit);
    let dir = path.parent().expect("a secrets path has a parent");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let _ = std::fs::remove_file(&path);
    let body = json!({"attach_key": spec.attach_key, "env": spec.secrets});
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    f.write_all(body.to_string().as_bytes())?;
    Ok(path)
}

/// `SIGKILL` for a host that did not end. A host killed this way cannot end its child, so the
/// child's group goes first (each child of the host leads a group of its own).
fn kill_host_and_its_child(pid: u32) {
    for child in peer::children_of(pid) {
        peer::kill_group(child, libc::SIGKILL);
    }
    peer::kill_pid(pid, libc::SIGKILL);
}

fn check_socket_len(socket: &Path) -> Result<(), LaunchError> {
    let len = socket.as_os_str().len();
    if len > SOCKET_PATH_MAX {
        return Err(LaunchError(format!(
            "the socket path {} is {len} bytes; a Unix socket path holds at most {SOCKET_PATH_MAX}. \
             Use a shorter server name or runtime directory",
            socket.display()
        )));
    }
    Ok(())
}

// ----------------------------------------------------------------------------------- detached

/// The detached launcher: a double-forked host in its own session. See the module comment for
/// what it lacks.
#[derive(Clone, Debug)]
pub struct Detached {
    runtime_dir: PathBuf,
}

impl Detached {
    pub fn new(runtime_dir: PathBuf) -> Detached {
        Detached { runtime_dir }
    }

    /// The runtime directory from `XDG_RUNTIME_DIR`.
    pub fn from_env() -> Result<Detached, LaunchError> {
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(|v| Detached::new(PathBuf::from(v)))
            .ok_or_else(|| LaunchError("XDG_RUNTIME_DIR is not set".to_string()))
    }

    fn servers_dir(&self) -> PathBuf {
        self.runtime_dir.join("pf-hosts")
    }

    /// The directory of a unit, found by name: the unit does not say where its server name ends.
    fn find_dir(&self, unit: &str) -> Option<PathBuf> {
        for server in std::fs::read_dir(self.servers_dir()).ok()?.flatten() {
            let server_name = server.file_name().to_string_lossy().into_owned();
            let Some(rest) = unit
                .strip_prefix(names::UNIT_PREFIX)
                .and_then(|r| r.strip_prefix(server_name.as_str()))
                .and_then(|r| r.strip_prefix('-'))
            else {
                continue;
            };
            let dir = server.path().join(rest);
            if dir.is_dir() {
                return Some(dir);
            }
        }
        None
    }

    /// The PID and start time in `host.pid`.
    fn read_pid(dir: &Path) -> Option<(u32, u64)> {
        let text = std::fs::read_to_string(dir.join("host.pid")).ok()?;
        let mut it = text.split_whitespace();
        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    }

    fn state_of(dir: &Path) -> HostState {
        match Self::read_pid(dir) {
            None => HostState::Unknown,
            Some((pid, start)) => {
                if peer::start_time(pid) == Some(start) {
                    HostState::Live
                } else {
                    HostState::Gone
                }
            }
        }
    }

    fn spawn(&self, spec: &StartSpec, args: &[String]) -> Result<(), LaunchError> {
        let mut cmd = Command::new(&spec.host_bin);
        cmd.args(args)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // A shell can inherit another unit's variables. The host must not mistake them for its own.
            .env_remove("INVOCATION_ID")
            .env_remove("NOTIFY_SOCKET");
        // SAFETY: the closure runs between `fork` and `exec` in the child and calls only
        // async-signal-safe functions: `setsid`, `fork` and `_exit`. The first child leaves the
        // caller's process group and session and exits; the second is the host, which `exec`
        // starts, with no session leader above it and a parent that is not the caller.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                match libc::fork() {
                    -1 => Err(std::io::Error::last_os_error()),
                    0 => Ok(()),
                    _ => libc::_exit(0),
                }
            });
        }
        let mut first = cmd
            .spawn()
            .map_err(|e| LaunchError(format!("cannot start {}: {e}", spec.host_bin.display())))?;
        let _ = first.wait();
        Ok(())
    }
}

impl Launcher for Detached {
    fn name(&self) -> &'static str {
        "detached"
    }

    fn start(&self, spec: &StartSpec) -> Result<Started, LaunchError> {
        let unit =
            names::unit_name(&spec.server, &spec.session, spec.incarnation).map_err(LaunchError)?;
        let dir = names::detached_dir(
            &self.runtime_dir,
            &spec.server,
            &spec.session,
            spec.incarnation,
        );
        let socket = dir.join("host.sock");
        check_socket_len(&socket)?;
        if dir.exists() {
            return Err(LaunchError(format!(
                "a host directory exists for {unit}: {}",
                dir.display()
            )));
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        let secrets = match write_secrets(&self.runtime_dir, &unit, spec) {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let args = run_args(spec, &unit, &dir, &secrets, "detached");
        if let Err(e) = self.spawn(spec, &args) {
            let _ = std::fs::remove_file(&secrets);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        let end = Instant::now() + READY_WAIT;
        loop {
            if socket.exists()
                && let Some((pid, _)) = Self::read_pid(&dir)
            {
                return Ok(Started {
                    unit,
                    socket,
                    pid,
                    invocation_id: String::new(),
                });
            }
            // The host failed to start: it wrote an exit file and removed its directory.
            let exit = names::exit_path(&spec.state_dir, &unit);
            if exit.exists() {
                let why = std::fs::read_to_string(&exit).unwrap_or_default();
                let _ = std::fs::remove_file(&secrets);
                return Err(LaunchError(format!("the host did not start: {why}")));
            }
            if Instant::now() >= end {
                let _ = std::fs::remove_file(&secrets);
                if let Some((pid, start)) = Self::read_pid(&dir)
                    && peer::start_time(pid) == Some(start)
                {
                    kill_host_and_its_child(pid);
                }
                let _ = std::fs::remove_dir_all(&dir);
                return Err(LaunchError(format!(
                    "the host of {unit} was not ready after {READY_WAIT:?}"
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn state(&self, unit: &str) -> HostState {
        match self.find_dir(unit) {
            None => HostState::Gone,
            Some(dir) => Self::state_of(&dir),
        }
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        let Some(dir) = self.find_dir(unit) else {
            return Ok(());
        };
        let Some((pid, start)) = Self::read_pid(&dir) else {
            return Ok(());
        };
        let live = || peer::start_time(pid) == Some(start);
        if !live() {
            return Ok(());
        }
        // The host ends its child's group, writes its exit file and removes its directory.
        peer::kill_pid(pid, libc::SIGTERM);
        let end = Instant::now() + STOP_WAIT;
        while live() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        if live() {
            kill_host_and_its_child(pid);
            let end = Instant::now() + Duration::from_secs(2);
            while live() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if live() {
            return Err(LaunchError(format!(
                "the host of {unit} (pid {pid}) did not end"
            )));
        }
        Ok(())
    }

    fn list(&self, server: &str) -> Result<Vec<HostRef>, LaunchError> {
        let dir = self.servers_dir().join(server);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((session, inc)) = name.rsplit_once('-') else {
                continue;
            };
            let Ok(incarnation) = inc.parse::<u64>() else {
                continue;
            };
            let Ok(unit) = names::unit_name(server, session, incarnation) else {
                continue;
            };
            out.push(HostRef {
                unit,
                session: session.to_string(),
                incarnation,
                state: Self::state_of(&entry.path()),
            });
        }
        out.sort_by(|a, b| a.unit.cmp(&b.unit));
        Ok(out)
    }
}

// ------------------------------------------------------------------------------------ systemd

/// The systemd launcher: one transient user unit per host, in a slice of its own, with Spike A's
/// settings (F1 to F5): `Type=notify` (ready after the socket is bound), `KillMode=mixed`, a
/// `RuntimeDirectory=` at mode 0700 for the socket, an `ExecStopPost=` step that writes the exit
/// file, and `--collect`. It calls the `systemd-run` and `systemctl` programs: no D-Bus crate
/// (ADR-0008).
#[derive(Clone, Debug)]
pub struct Systemd {
    runtime_dir: PathBuf,
    systemd_run: String,
    systemctl: String,
}

/// Quote one word of an `Exec` line the way systemd reads it: double quotes, with the backslash,
/// the double quote, `$` and `%` escaped. A word of plain characters stays as it is.
pub fn quote_exec_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-=:,+@".contains(c));
    if plain {
        return word.to_string();
    }
    let mut q = String::from("\"");
    for c in word.chars() {
        match c {
            '\\' => q.push_str("\\\\"),
            '"' => q.push_str("\\\""),
            '$' => q.push_str("$$"),
            '%' => q.push_str("%%"),
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

/// The properties `systemctl show` printed, as `Name=Value` lines.
pub fn parse_show(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// The hosts of `server` in the output of `systemctl list-units --all --plain --no-legend`.
pub fn parse_list_units(server: &str, text: &str) -> Vec<HostRef> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut cols = line.split_whitespace();
        // A unit that failed or is not found is marked with a bullet before its name.
        let Some(mut unit) = cols.next() else {
            continue;
        };
        if unit == "●" || unit == "*" {
            let Some(u) = cols.next() else { continue };
            unit = u;
        }
        let Some((session, incarnation)) = names::parse_unit(server, unit) else {
            continue;
        };
        let _load = cols.next();
        let active = cols.next().unwrap_or("");
        out.push(HostRef {
            unit: unit.trim_end_matches(".service").to_string(),
            session,
            incarnation,
            state: active_state(active),
        });
    }
    out
}

/// Map systemd's `ActiveState` to a host's state.
pub fn active_state(active: &str) -> HostState {
    match active {
        "active" | "activating" | "reloading" | "deactivating" | "refreshing" => HostState::Live,
        "inactive" | "failed" | "maintenance" => HostState::Gone,
        _ => HostState::Unknown,
    }
}

impl Systemd {
    pub fn new(runtime_dir: PathBuf) -> Systemd {
        Systemd {
            runtime_dir,
            systemd_run: "systemd-run".to_string(),
            systemctl: "systemctl".to_string(),
        }
    }

    /// The same launcher with other programs in place of `systemd-run` and `systemctl`: a test
    /// double that records its arguments, or a copy at a full path.
    pub fn with_programs(mut self, systemd_run: &str, systemctl: &str) -> Systemd {
        self.systemd_run = systemd_run.to_string();
        self.systemctl = systemctl.to_string();
        self
    }

    /// The runtime directory from `XDG_RUNTIME_DIR`.
    pub fn from_env() -> Result<Systemd, LaunchError> {
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(|v| Systemd::new(PathBuf::from(v)))
            .ok_or_else(|| LaunchError("XDG_RUNTIME_DIR is not set".to_string()))
    }

    /// The host's directory: its `RuntimeDirectory=`, named for the unit.
    pub fn host_dir(&self, unit: &str) -> PathBuf {
        self.runtime_dir.join(unit)
    }

    /// The arguments of `systemd-run` for a spec: the unit properties, then `--` and the host's
    /// command. It carries no `--setenv` and no `-E`, and no secret value and no attach key: the
    /// host reads them from the secrets file at `secrets`.
    pub fn systemd_run_args(
        &self,
        spec: &StartSpec,
        secrets: &Path,
    ) -> Result<Vec<String>, LaunchError> {
        let unit =
            names::unit_name(&spec.server, &spec.session, spec.incarnation).map_err(LaunchError)?;
        let slice = names::slice_name(&spec.server).map_err(LaunchError)?;
        let host = spec.host_bin.display().to_string();
        let stop_post = [
            host.clone(),
            "stop-post".to_string(),
            "--unit".to_string(),
            unit.clone(),
            "--state-dir".to_string(),
            spec.state_dir.display().to_string(),
            "--session".to_string(),
            spec.session.clone(),
            "--incarnation".to_string(),
            spec.incarnation.to_string(),
        ]
        .iter()
        .map(|w| quote_exec_word(w))
        .collect::<Vec<_>>()
        .join(" ");
        let mut a: Vec<String> = vec![
            "--user".into(),
            format!("--unit={unit}"),
            format!("--slice={slice}"),
            "--collect".into(),
            "--quiet".into(),
            "-p".into(),
            "Type=notify".into(),
            "-p".into(),
            "KillMode=mixed".into(),
            "-p".into(),
            "TimeoutStopSec=10".into(),
            "-p".into(),
            format!("RuntimeDirectory={unit}"),
            "-p".into(),
            "RuntimeDirectoryMode=0700".into(),
            "-p".into(),
            format!("ExecStopPost={stop_post}"),
            "--".into(),
            host,
        ];
        a.extend(run_args(
            spec,
            &unit,
            &self.host_dir(&unit),
            secrets,
            "systemd",
        ));
        Ok(a)
    }

    fn systemctl(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new(&self.systemctl)
            .arg("--user")
            .args(args)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::null())
            .output()
    }

    fn show(&self, unit: &str) -> Option<HashMap<String, String>> {
        let out = self
            .systemctl(&[
                "show",
                "-p",
                "ActiveState,InvocationID,MainPID",
                &format!("{unit}.service"),
            ])
            .ok()?;
        out.status
            .success()
            .then(|| parse_show(&String::from_utf8_lossy(&out.stdout)))
    }
}

impl Launcher for Systemd {
    fn name(&self) -> &'static str {
        "systemd"
    }

    fn start(&self, spec: &StartSpec) -> Result<Started, LaunchError> {
        let unit =
            names::unit_name(&spec.server, &spec.session, spec.incarnation).map_err(LaunchError)?;
        let socket = self.host_dir(&unit).join("host.sock");
        check_socket_len(&socket)?;
        let secrets = write_secrets(&self.runtime_dir, &unit, spec)?;
        let args = match self.systemd_run_args(spec, &secrets) {
            Ok(a) => a,
            Err(e) => {
                let _ = std::fs::remove_file(&secrets);
                return Err(e);
            }
        };
        let out = Command::new(&self.systemd_run)
            .args(&args)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::null())
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                let _ = std::fs::remove_file(&secrets);
                return Err(LaunchError(format!("cannot run {}: {e}", self.systemd_run)));
            }
        };
        if !out.status.success() {
            let _ = std::fs::remove_file(&secrets);
            return Err(LaunchError(format!(
                "systemd-run failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        // From here a unit runs: an `Err` must not leave it behind, with nothing to find it.
        let unit_runs = |e: LaunchError| {
            let _ = self.stop(&unit);
            let _ = std::fs::remove_file(&secrets);
            e
        };
        let props = self.show(&unit).ok_or_else(|| {
            unit_runs(LaunchError(format!(
                "systemd-run started {unit}, but systemctl show cannot read it"
            )))
        })?;
        let pid = props
            .get("MainPID")
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| unit_runs(LaunchError(format!("{unit} has no main process"))))?;
        Ok(Started {
            unit,
            socket,
            pid,
            invocation_id: props.get("InvocationID").cloned().unwrap_or_default(),
        })
    }

    fn state(&self, unit: &str) -> HostState {
        match self.show(unit) {
            Some(props) => active_state(props.get("ActiveState").map_or("", String::as_str)),
            None => HostState::Unknown,
        }
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        let out = self
            .systemctl(&["stop", &format!("{unit}.service")])
            .map_err(|e| LaunchError(format!("cannot run {}: {e}", self.systemctl)))?;
        if out.status.success() || self.state(unit) == HostState::Gone {
            return Ok(());
        }
        Err(LaunchError(format!(
            "systemctl stop {unit} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }

    fn list(&self, server: &str) -> Result<Vec<HostRef>, LaunchError> {
        names::check(server, "x").map_err(LaunchError)?;
        let pattern = format!("{}{server}-*.service", names::UNIT_PREFIX);
        let out = self
            .systemctl(&[
                "list-units",
                "--all",
                "--plain",
                "--no-legend",
                "--no-pager",
                &pattern,
            ])
            .map_err(|e| LaunchError(format!("cannot run {}: {e}", self.systemctl)))?;
        if !out.status.success() {
            return Err(LaunchError(format!(
                "systemctl list-units failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(parse_list_units(
            server,
            &String::from_utf8_lossy(&out.stdout),
        ))
    }
}
