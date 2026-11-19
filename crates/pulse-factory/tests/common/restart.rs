//! The restart rig of `r2.s2.w3` (`tier1_reconcile.rs`, `seat_key_rebuild.rs`, `time_to_ready.rs`):
//! the real `pulse-factory controller` binary on a paper run whose seats stay alive in detached
//! session hosts, a way to `SIGKILL` the controller and start it again, and a guard that stops every
//! host of the rig's server when a test ends. Nothing here starts a model, spends a token or reaches
//! beyond the temp directories.
#![allow(dead_code)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pf_host::launcher::{Detached, HostState, Launcher};
use serde_json::Value;

use super::BIN;

static N: AtomicUsize = AtomicUsize::new(0);

/// The `pf-host` binary: `PF_HOST_BIN`, else `pf-host` in the test binary's target directory.
pub fn host_bin() -> PathBuf {
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

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub struct Restart {
    /// A short temp directory: socket paths stay under 107 bytes.
    pub root: PathBuf,
    pub state: PathBuf,
    pub share: PathBuf,
    /// The runtime directory the detached launcher uses.
    pub rt: PathBuf,
    pub paper: PathBuf,
    pub spine_dir: PathBuf,
    pub worktrees: PathBuf,
    pub ledger: PathBuf,
    pub log: PathBuf,
    pub controller: Option<Child>,
    pub run: Option<String>,
}

impl Restart {
    pub fn new() -> Restart {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("pfr-{}-{n}", std::process::id()));
        let rig = Restart {
            state: root.join("st"),
            share: root.join("sh"),
            rt: root.join("rt"),
            paper: root.join("paper"),
            spine_dir: root.join("spine"),
            worktrees: root.join("wt"),
            ledger: root.join("ledger.md"),
            log: root.join("controller.log"),
            root,
            controller: None,
            run: None,
        };
        for d in [
            &rig.state,
            &rig.share,
            &rig.rt,
            &rig.spine_dir,
            &rig.worktrees,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let support = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/restart_support/paper");
        copy_dir(&support, &rig.paper);
        // The copy keeps the mode of the seat script, but a checkout may not.
        let seat = rig.paper.join("seats/seat.sh");
        let mut perm = std::fs::metadata(&seat).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        std::fs::set_permissions(&seat, perm).unwrap();
        std::fs::write(&rig.ledger, "").unwrap();
        // The paper forge: a bare repo with `main`.
        let forge = rig.paper.join("forge.git");
        git(
            &rig.root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                forge.to_str().unwrap(),
            ],
        );
        rig
    }

    /// The environment of the controller and of the CLI: nothing of the test's own.
    fn base(&self, cmd: &mut Command) {
        cmd.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("PF_STATE_DIR", &self.state)
            .env("PF_SHARE_DIR", &self.share)
            .env("XDG_RUNTIME_DIR", &self.rt)
            .env("PF_HOST_BIN", host_bin())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
    }

    pub fn cli_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        self.base(&mut cmd);
        cmd.args(args);
        cmd
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        self.cli_cmd(args).output().expect("run pulse-factory")
    }

    /// Start the controller and do not wait for it.
    pub fn spawn_controller(&mut self) {
        assert!(self.controller.is_none(), "a controller is running");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        let mut cmd = self.cli_cmd(&["controller", "--tick-ms", "100"]);
        cmd.stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        self.controller = Some(cmd.spawn().expect("start the controller"));
    }

    /// Whether something answers on the controller socket.
    pub fn socket_answers(&self) -> bool {
        UnixStream::connect(self.state.join("controller.sock")).is_ok()
    }

    /// Start the controller and wait until its socket answers.
    pub fn start_controller(&mut self) {
        self.spawn_controller();
        self.wait_for("the controller socket", Duration::from_secs(60), || {
            self.socket_answers()
        });
    }

    /// `SIGKILL` the controller and wait until it is gone.
    pub fn kill9(&mut self) {
        let mut child = self.controller.take().expect("a controller is running");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    pub fn controller_pid(&self) -> u32 {
        self.controller.as_ref().expect("a controller").id()
    }

    pub fn controller_exited(&mut self) -> bool {
        self.controller
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_some())
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Poll `f` until it is true, or fail with `what` and the controller's log.
    pub fn wait_for(&self, what: &str, within: Duration, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if f() {
            return;
        }
        panic!(
            "timed out after {within:?} waiting for {what}\n--- controller log\n{}\n--- ledger\n{}",
            self.log_text(),
            self.controller_ledger()
        );
    }

    /// Arm the paper run. The units come later, with [`Restart::queue`].
    pub fn arm(&mut self) -> String {
        let out = self
            .cli_cmd(&[
                "arm",
                "restart",
                "--workflow",
                self.paper.join("workflow").to_str().unwrap(),
                "--ledger",
                self.ledger.to_str().unwrap(),
                "--paper",
                self.paper.to_str().unwrap(),
                "--param",
                &format!("spine_dir={}", self.spine_dir.display()),
                "--param",
                &format!("worktree_root={}", self.worktrees.display()),
                "--yes",
            ])
            .output()
            .expect("run arm");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "arm refused: {}{}",
            text,
            String::from_utf8_lossy(&out.stderr)
        );
        let run = text
            .lines()
            .find_map(|l| l.strip_prefix("run: "))
            .unwrap_or_else(|| panic!("arm printed no run ID: {text}"))
            .trim()
            .to_string();
        self.run = Some(run.clone());
        run
    }

    pub fn run_id(&self) -> &str {
        self.run.as_deref().expect("a run is armed")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.state.join("runs").join(self.run_id())
    }

    /// Put a unit in the run's queue.
    pub fn queue(&self, id: &str, order: u32) {
        let wt = self.worktrees.join(id);
        std::fs::create_dir_all(&wt).unwrap();
        let queue = self.run_dir().join("queue");
        std::fs::create_dir_all(&queue).unwrap();
        let body = format!(
            "{{\"version\":1,\"id\":\"{id}\",\"group\":\"g1\",\"order\":{order},\"vars\":{{\"worktree_path\":\"{}\",\"branch\":\"work/{id}\"}}}}\n",
            wt.display()
        );
        let tmp = queue.join(format!(".{id}.json.tmp"));
        std::fs::write(&tmp, body).unwrap();
        std::fs::rename(&tmp, queue.join(format!("{id}.json"))).unwrap();
    }

    /// The directory of a unit's seat files: `seat.pid`, `hook.in`, `hook.out`, `go`.
    pub fn out(&self, unit: &str) -> PathBuf {
        self.run_dir().join("out").join(unit)
    }

    /// The seat's own PID, once it wrote it.
    pub fn seat_pid(&self, unit: &str) -> Option<u32> {
        std::fs::read_to_string(self.out(unit).join("seat.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn wait_seat(&self, unit: &str) -> u32 {
        self.wait_for(
            &format!("the seat of {unit}"),
            Duration::from_secs(60),
            || self.seat_pid(unit).is_some(),
        );
        self.seat_pid(unit).unwrap()
    }

    pub fn status(&self) -> Value {
        let out = self.cli(&["status", self.run_id(), "--json"]);
        assert!(
            out.status.success(),
            "status failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("status is JSON")
    }

    /// `(status, reason)` of a unit, or `None` when the status does not name it.
    pub fn unit(&self, unit: &str) -> Option<(String, String)> {
        let status = self.status();
        let u = status["units"]
            .as_array()?
            .iter()
            .find(|u| u["id"] == unit)?
            .clone();
        Some((
            u["status"].as_str().unwrap_or_default().to_string(),
            u["reason"].as_str().unwrap_or_default().to_string(),
        ))
    }

    pub fn wait_unit(&self, unit: &str, status: &str) -> String {
        self.wait_for(
            &format!("unit {unit} to be {status}"),
            Duration::from_secs(60),
            || self.unit(unit).is_some_and(|(s, _)| s == status),
        );
        self.unit(unit).unwrap().1
    }

    pub fn controller_ledger(&self) -> String {
        std::fs::read_to_string(self.state.join("controller.ledger")).unwrap_or_default()
    }

    pub fn ledger_lines(&self, needle: &str) -> Vec<String> {
        self.controller_ledger()
            .lines()
            .filter(|l| l.contains(needle))
            .map(str::to_string)
            .collect()
    }

    // ------------------------------------------------------------------ the hosts

    pub fn server(&self) -> String {
        pulse_factory::controller::hosts::read_host_server_name(&self.state)
            .unwrap()
            .expect("the controller made a host server name")
    }

    pub fn detached(&self) -> Detached {
        Detached::new(self.rt.clone())
    }

    /// The directory of a server's hosts under the runtime directory.
    pub fn hosts_dir(&self, server: &str) -> PathBuf {
        self.rt.join("pf-hosts").join(server)
    }

    /// The hosts of this rig's server that still run.
    pub fn live_hosts(&self) -> Vec<pf_host::launcher::HostRef> {
        self.detached()
            .list(&self.server())
            .unwrap_or_default()
            .into_iter()
            .filter(|h| h.state != HostState::Gone)
            .collect()
    }

    /// `(unit, ended_at, exit, session_id)` of every host row, oldest first.
    pub fn host_rows(&self) -> Vec<(String, Option<String>, Option<String>, String)> {
        let db = rusqlite::Connection::open_with_flags(
            self.state.join("store.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut stmt = db
            .prepare("SELECT unit, ended_at, exit, session_id FROM hosts ORDER BY rowid")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The unit of the host that runs a unit's seat: the newest row of the session. The seat can
    /// write its PID before the controller commits the row, so it waits for the row.
    pub fn unit_of_seat(&self, unit: &str) -> String {
        let key = format!("{}/{unit}", self.run_id());
        self.wait_for(
            &format!("the host row of {key}"),
            Duration::from_secs(30),
            || self.host_rows().iter().any(|r| r.3 == key),
        );
        self.host_rows()
            .into_iter()
            .rfind(|r| r.3 == key)
            .unwrap()
            .0
    }

    /// Make one hook call from below the seat of `unit`: the seat script runs `$PF_HOOK_CMD` on the
    /// call with its own environment, so the controller sees a call from the seat's process.
    pub fn hook(&self, unit: &str, call: &Value) -> Value {
        let out = self.out(unit);
        let _ = std::fs::remove_file(out.join("hook.out"));
        let tmp = out.join("hook.in.tmp");
        std::fs::write(&tmp, call.to_string()).unwrap();
        std::fs::rename(&tmp, out.join("hook.in")).unwrap();
        self.wait_for(
            &format!("the hook answer of {unit}"),
            Duration::from_secs(30),
            || out.join("hook.out").exists(),
        );
        let text = std::fs::read_to_string(out.join("hook.out")).unwrap();
        let _ = std::fs::remove_file(out.join("hook.out"));
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("hook said {text:?}: {e}"))
    }

    /// Whether the engine recorded the seat's host unit as the process of an open session of
    /// `unit`: until it did, a controller killed now has a host and no session that names it.
    pub fn session_attached(&self, unit: &str) -> bool {
        let db = rusqlite::Connection::open_with_flags(
            self.state.join("store.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.query_row(
            "SELECT count(*) FROM sessions WHERE unit_id = ?1 AND ended = 0 AND process_ref IS NOT NULL",
            [unit],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    pub fn wait_attached(&self, unit: &str) {
        self.wait_for(
            &format!("the session of {unit} to name its host"),
            Duration::from_secs(30),
            || self.session_attached(unit),
        );
    }

    /// The host process of a unit and the seat's process group, killed without a trace: the host
    /// leaves no exit file.
    pub fn sigkill_host(&self, unit: &str) {
        let dir = self.hosts_dir(&self.server());
        let (session, inc) = pf_host::names::parse_unit(&self.server(), unit).unwrap();
        let pidfile = dir.join(format!("{session}-{inc}")).join("host.pid");
        let text = std::fs::read_to_string(&pidfile).unwrap();
        let pid: u32 = text.split_whitespace().next().unwrap().parse().unwrap();
        for child in pf_host::peer::children_of(pid) {
            pf_host::peer::kill_group(child, 9);
        }
        pf_host::peer::kill_pid(pid, 9);
        self.wait_for(
            &format!("host {unit} to die"),
            Duration::from_secs(10),
            || {
                !Path::new(&format!("/proc/{pid}")).exists()
                    || std::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .map(|s| s.contains(") Z "))
                        .unwrap_or(true)
            },
        );
    }
}

impl Drop for Restart {
    fn drop(&mut self) {
        if let Some(mut c) = self.controller.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // Stop every host under the runtime directory, of this rig's server or of another.
        let detached = Detached::new(self.rt.clone());
        if let Ok(servers) = std::fs::read_dir(self.rt.join("pf-hosts")) {
            for server in servers.flatten() {
                let name = server.file_name().to_string_lossy().into_owned();
                for h in detached.list(&name).unwrap_or_default() {
                    let _ = detached.stop(&h.unit);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
