//! The controller: one process, seats as children (ADR-0001). It owns the store, the queue and
//! the ledger (ADR-0003). It also owns the projects and devices and their own ledger,
//! `controller.ledger`, and, when the operator sets an address, the network API (ADR-0012).

pub mod agents;
pub mod api;
pub mod base_push;
pub mod chat_install;
pub mod chat_verbs;
pub mod chat_view;
pub mod chats;
pub mod client;
pub mod commands;
pub mod confirm;
pub mod device_acts;
pub mod devices;
pub mod driver;
pub mod files;
pub mod ports;
pub mod projects;
pub mod request_frames;
pub mod requests;
pub mod run_list;
pub mod seat;
pub mod sessions;
pub mod settings;
pub mod status;
pub mod worktrees;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use pf_adapter_claude::pinned::Pin;
use pf_engine::ledger::FsLedger;
use pf_engine::port::agent::{AgentPort, SessionRef};
use pf_engine::port::chat::ChatPort;
use pf_engine::port::store::{RunState, Store};
use pf_engine::{Engine, EngineConfig, system_clock};
use pf_ipc::{Handler, Peer, Request, Response};
use pf_store_sqlite::SqliteStore;
use pf_workflow::package::Package;
use pf_workflow::pin;
use serde_json::{Value, json};

use crate::arm::{ArmPlan, RunInfo, check_earlier};
use crate::cli::ControllerArgs;
use driver::{Driver, DriverConfig, system_secs};
use files::{Manifest, PluginCopy, RunJson};
use ports::{LivePorts, Ports, RunSpec};

pub struct Config {
    pub state_dir: PathBuf,
    pub tick_ms: u64,
    /// The binary's canonical path: the seats run `<binary> hook`.
    pub binary: PathBuf,
    /// The source of random bytes for session keys and claim tokens: `/dev/urandom`. A test
    /// points it at a path that cannot be read.
    pub random_device: PathBuf,
    /// The proc file system the caller check reads: `/proc`. A test points it elsewhere.
    pub proc_root: PathBuf,
    /// The address of the network API, such as `100.90.203.21:7443`. `None`: the API is off, and
    /// no key is made (ADR-0012).
    pub api_listen: Option<String>,
    /// How long a pairing code works: 10 minutes. A test sets a short time.
    pub pair_ttl: Duration,
    /// How long a bind that cannot happen yet (the address is not on this host) waits before it
    /// tries again: 5 seconds.
    pub api_retry: Duration,
    /// How long a device may make no call before the API refuses it and it must pair again: 30
    /// days (ADR-0012). A test sets a short time.
    pub device_idle: Duration,
    /// Variables added to every git call, after the controller's own `GIT_*` variables are removed.
    /// Empty. A test sets one, such as `GIT_TEST_ASSUME_DIFFERENT_OWNER=1`.
    pub git_env: Vec<(String, String)>,
    /// The chat runtimes of this server, by runtime ID (`r1.s3.w3`). `controller::run` puts one
    /// `ClaudeChat` under `claude-code`. A test puts a `FakeChat`, or a `ClaudeChat` over the
    /// replay, under the same ID. An agent whose runtime has no entry is refused at its start.
    pub chat_runtimes: BTreeMap<String, Arc<dyn ChatPort>>,
    /// How long a chat process may be idle before the controller releases it: 30 minutes.
    pub chat_idle: Duration,
    /// The cap on live chat processes (`PF_CHAT_MAX_LIVE`): 4.
    pub chat_max_live: usize,
    /// The registry the managed Claude Code copy is fetched from. `None` in `Config::new`, so a test
    /// never fetches. `run` sets it.
    pub chat_registry: Option<String>,
    /// The pin the start-time check of the managed copy uses.
    pub chat_pin: Pin,
    /// The share directory that holds the managed copy.
    pub chat_share_dir: PathBuf,
    /// `PF_AGENT_ENV_<AGENT>`: the agent's server-side environment, by the upper-cased agent key.
    pub chat_agent_env: BTreeMap<String, BTreeMap<String, String>>,
    /// The server's name, as every API answer names it (`PF_SERVER_NAME`).
    pub server_name: String,
    /// True when the chat runs the managed copy, so a runtime list can say whether the copy is
    /// there.
    pub chat_managed: bool,
    /// The window in which the refusals of one caller and one settings key coalesce into one
    /// more ledger line: 60 seconds (ADR-0016). A test sets a short time.
    pub refusal_window: Duration,
    /// How long a confirm code works: 120 seconds. `PF_CONFIRM_TTL` can only shorten it. A test sets a
    /// short time.
    pub confirm_ttl: Duration,
    /// How long a used, voided or expired confirm code stays in the table as a tombstone, past its
    /// expiry: 10 minutes. A test sets a short time.
    pub confirm_tombstone: Duration,
}

impl Config {
    /// A config with the system's random source and `/proc`.
    pub fn new(state_dir: PathBuf, tick_ms: u64, binary: PathBuf) -> Config {
        Config {
            state_dir,
            tick_ms,
            binary,
            random_device: PathBuf::from(seat::RANDOM_DEVICE),
            proc_root: PathBuf::from("/proc"),
            api_listen: None,
            pair_ttl: Duration::from_secs(600),
            api_retry: api::DEFAULT_RETRY,
            device_idle: DEFAULT_DEVICE_IDLE,
            git_env: Vec::new(),
            chat_runtimes: BTreeMap::new(),
            chat_idle: chats::DEFAULT_IDLE,
            chat_max_live: chats::DEFAULT_MAX_LIVE,
            chat_registry: None,
            chat_pin: Pin::release(),
            chat_share_dir: crate::arm::share_dir(),
            chat_agent_env: BTreeMap::new(),
            server_name: "server".to_string(),
            chat_managed: false,
            refusal_window: settings::DEFAULT_REFUSAL_WINDOW,
            confirm_ttl: DEFAULT_CONFIRM_TTL,
            confirm_tombstone: DEFAULT_CONFIRM_TOMBSTONE,
        }
    }

    /// Take the chat settings that the controller's environment gave.
    pub fn apply_chat_env(&mut self, env: &chats::ChatEnv) {
        self.chat_share_dir = env.share_dir.clone();
        self.chat_max_live = env.max_live;
        self.chat_agent_env = env.agent_env.clone();
        self.server_name = env.server_name.clone();
        self.chat_registry = Some(env.registry.clone());
        self.confirm_ttl = env.confirm_ttl;
    }
}

/// How long a confirm code works: 2 minutes.
pub const DEFAULT_CONFIRM_TTL: Duration = Duration::from_secs(120);

/// How long a confirm code stays as a tombstone past its expiry: 10 minutes.
pub const DEFAULT_CONFIRM_TOMBSTONE: Duration = Duration::from_secs(600);

/// How long a device may be idle: 30 days.
pub const DEFAULT_DEVICE_IDLE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// One run the controller knows.
pub struct RunHandle {
    pub info: RunJson,
    pub dir: PathBuf,
    pub engine: Arc<Engine>,
    pub agent: Arc<dyn AgentPort>,
    pub driver: Arc<Driver>,
    /// False for a run at an end state, a replaced run and a run that start-up could not read.
    pub driven: AtomicBool,
}

pub struct Controller {
    cfg: Config,
    store: Arc<SqliteStore>,
    ports: Arc<dyn Ports>,
    runs: Mutex<Vec<Arc<RunHandle>>>,
    seats: Arc<seat::SeatTable>,
    /// `claim`, `answer`, `clear` and `ack` run one at a time, so a claim cannot fence between an
    /// answer's check of the generation and its commit.
    claim_lock: Mutex<()>,
    /// The controller ledger, and the one writer of it. A project or device command holds this lock
    /// from its commit to the append of its line.
    controller_ledger: Mutex<commands::ControllerLedger>,
    /// The network API: its key, its state and the one live pairing code.
    api: api::ApiRuntime,
    /// The confirm codes of the paired devices, in memory only (`r1.s4.w1`).
    confirm: confirm::ConfirmCodes,
    /// The chat processes, their pumps, `chat.ledger` and the stream's frames (`r1.s3.w3`).
    chats: chats::Chats,
    /// The open windows of refused settings callers (ADR-0016).
    settings_refusals: settings::Refusals,
    /// This controller, for the threads that outlive a call.
    me: Weak<Controller>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Controller {
    /// Open the store and read the runs on disk. Each run not at an end state is started up
    /// (reconcile parks a unit whose seat is gone) and its stale sessions are ended.
    pub fn open(cfg: Config, ports: Arc<dyn Ports>) -> Result<Arc<Controller>, String> {
        pf_ipc::prepare_state_dir(&cfg.state_dir)
            .map_err(|e| format!("cannot prepare {}: {e}", cfg.state_dir.display()))?;
        let store = Arc::new(
            SqliteStore::open(cfg.state_dir.join("store.sqlite"))
                .map_err(|e| format!("cannot open the store: {e}"))?,
        );
        // The controller ledger is recovered before any command: a stop between a commit and the
        // append of its line loses no line, and a restart adds no second copy.
        let controller_ledger = commands::ControllerLedger::start(
            store.as_ref(),
            cfg.state_dir.join(commands::LEDGER_FILE),
        );
        // With an address set, the key is made now, so `api status` and `device pair` have it
        // before the listener binds.
        let api = api::ApiRuntime::start(&cfg.state_dir, cfg.api_listen.as_deref());
        let chats = chats::Chats::new(&cfg.state_dir);
        let controller = Arc::new_cyclic(|me| Controller {
            cfg,
            store,
            ports,
            runs: Mutex::new(Vec::new()),
            seats: Arc::new(seat::SeatTable::default()),
            claim_lock: Mutex::new(()),
            controller_ledger: Mutex::new(controller_ledger),
            api,
            confirm: confirm::ConfirmCodes::default(),
            chats,
            settings_refusals: settings::Refusals::default(),
            me: me.clone(),
        });
        // The project hierarchy (ADR-0015): a project that has no kind gets one, and a live chat
        // session is set to wait for `--resume`, as the restart ended its process.
        controller.backfill_project_kinds();
        controller.settle_live_chat_sessions();
        controller.check_managed_copy();
        let mut found: Vec<RunJson> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(files::runs_dir(&controller.cfg.state_dir)) {
            for entry in entries.flatten() {
                // A directory with no run.json is a half-made run. It is not a run.
                if let Ok(info) = files::read_json::<RunJson>(&entry.path().join("run.json")) {
                    found.push(info);
                }
            }
        }
        found.sort_by(|a, b| (&a.created, &a.run_id).cmp(&(&b.created, &b.run_id)));
        for info in found {
            let dir = files::runs_dir(&controller.cfg.state_dir).join(&info.run_id);
            match controller.build_run(&dir, info.clone(), None) {
                Ok(handle) => lock(&controller.runs).push(handle),
                Err(why) => eprintln!("pulse-factory: run {} is not loaded: {why}", info.run_id),
            }
        }
        controller.prime_request_watch();
        Ok(controller)
    }

    pub fn state_dir(&self) -> &Path {
        &self.cfg.state_dir
    }

    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.store
    }

    pub fn handles(&self) -> Vec<Arc<RunHandle>> {
        lock(&self.runs).clone()
    }

    /// Build the engine and driver of a run from its directory. `ports` is given when `arm` has
    /// built them already.
    fn build_run(
        &self,
        dir: &Path,
        info: RunJson,
        built: Option<ports::RunPorts>,
    ) -> Result<Arc<RunHandle>, String> {
        let package = Package::load(&dir.join("package")).map_err(|e| e.to_string())?;
        let manifest: Manifest = files::read_json(&dir.join("manifest.json"))?;
        let run_ports = match built {
            Some(p) => p,
            None => self.ports.build(&RunSpec {
                run_id: info.run_id.clone(),
                run_dir: dir.to_path_buf(),
                runtimes: package
                    .file
                    .roles
                    .iter()
                    .map(|r| r.runtime.clone())
                    .collect(),
                plugin_roots: info.plugin_roots.clone(),
                paper: info.paper.clone(),
                forge_repo: info.forge_repo.clone(),
            })?,
        };
        let run_ports = self.wrap_seats(run_ports, &package, &info, dir);
        let cfg = EngineConfig {
            run_id: info.run_id.clone(),
            workflow: package.file.to_workflow(),
            ledger_path: info.ledger.clone(),
            state_dir: dir.to_path_buf(),
            run_dir: dir.to_path_buf(),
            params: info.params.clone(),
        };
        let engine = Arc::new(
            Engine::new(
                cfg,
                self.store.clone() as Arc<dyn Store>,
                run_ports.agent.clone(),
                run_ports.forge.clone(),
                system_clock(),
                Box::new(FsLedger::new(info.ledger.clone())),
            )
            .map_err(|e| e.to_string())?,
        );
        let ended = self
            .store
            .run(&info.run_id)
            .map_err(|e| e.to_string())?
            .is_some_and(|r| r.state == RunState::Ended);
        let driver = Arc::new(Driver::new(DriverConfig {
            engine: engine.clone(),
            agent: run_ports.agent.clone(),
            package,
            pins: manifest.all(),
            params: info.params.clone(),
            run_dir: dir.to_path_buf(),
            target_dir: info.target_dir.clone(),
            forge_repo: info.forge_repo.clone(),
            default_branch: info.default_branch.clone(),
            paper: info.paper.is_some(),
            clock: system_secs(),
            walk_limit: driver::WALK_LIMIT,
        }));
        let mut driven = !ended;
        if !ended {
            match engine.start_up() {
                Ok(report) => {
                    driver.note_startup(report.notes());
                    driver.sweep();
                }
                Err(e) => {
                    eprintln!("pulse-factory: run {} did not start up: {e}", info.run_id);
                    driven = false;
                }
            }
        }
        Ok(Arc::new(RunHandle {
            info,
            dir: dir.to_path_buf(),
            engine,
            agent: run_ports.agent,
            driver,
            driven: AtomicBool::new(driven),
        }))
    }

    /// Put `SeatPort` over the run's adapters.
    fn wrap_seats(
        &self,
        run_ports: ports::RunPorts,
        package: &Package,
        info: &RunJson,
        dir: &Path,
    ) -> ports::RunPorts {
        let mut rules = std::collections::BTreeMap::new();
        for role in &package.file.roles {
            if let Some(parsed) = package.rules.get(&role.name) {
                rules.insert(
                    role.name.clone(),
                    (dir.join("package").join(&role.rules), parsed.clone()),
                );
            }
        }
        let facts = seat::SeatFacts {
            repo: info.forge_repo.clone(),
            remote: "origin".to_string(),
            default_branch: info.default_branch.clone(),
            forge_api_hosts: run_ports.forge_api_hosts.clone(),
            run_dir: dir.to_path_buf(),
            state_dir: self.cfg.state_dir.clone(),
            target_dir: info.target_dir.clone(),
            plugins: info.plugins.iter().map(|p| p.path.clone()).collect(),
            rules,
            repeat_seeds: seat::repeat_seeds(&package.file),
            binary: self.cfg.binary.clone(),
            random_device: self.cfg.random_device.clone(),
        };
        ports::RunPorts {
            agent: Arc::new(seat::SeatPort::new(
                run_ports.agent,
                facts,
                self.seats.clone(),
            )),
            forge: run_ports.forge,
            forge_api_hosts: run_ports.forge_api_hosts,
        }
    }

    fn find(&self, run: Option<&str>) -> Result<Arc<RunHandle>, String> {
        let runs = lock(&self.runs);
        match run {
            Some(id) => runs
                .iter()
                .find(|r| r.info.run_id == id)
                .cloned()
                .ok_or_else(|| format!("no run named {id}")),
            None => runs
                .last()
                .cloned()
                .ok_or_else(|| "there is no run yet".to_string()),
        }
    }

    /// How many sessions of a run are running: the row is open and the process answers alive.
    fn running_sessions(&self, handle: &RunHandle) -> u64 {
        let Ok(sessions) = self.store.sessions(&handle.info.run_id) else {
            return 0;
        };
        sessions
            .iter()
            .filter(|s| !s.ended)
            .filter_map(|s| s.process_ref.as_ref())
            .filter(|r| {
                handle
                    .agent
                    .is_alive(&SessionRef((*r).clone()))
                    .unwrap_or(false)
            })
            .count() as u64
    }

    fn run_infos(&self) -> Vec<RunInfo> {
        self.handles()
            .iter()
            .map(|h| RunInfo {
                id: h.info.run_id.clone(),
                spine: h.info.spine.clone(),
                state: self
                    .store
                    .run(&h.info.run_id)
                    .ok()
                    .flatten()
                    .map_or("active", |r| r.state.as_str())
                    .to_string(),
                running_sessions: self.running_sessions(h),
            })
            .collect()
    }

    /// Run one tick of every driven run. A panic in one run's tick stops that tick only.
    pub fn tick_all(&self) {
        // A chat process idle for the window is released. Nothing here opens one.
        self.sweep_chats();
        // A window of refused settings writes that has ended gets its closing line.
        self.close_refusal_windows();
        for handle in self.handles() {
            if !handle.driven.load(Ordering::SeqCst) {
                continue;
            }
            let driver = handle.driver.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| driver.tick()));
            if result.is_err() {
                eprintln!(
                    "pulse-factory: a tick of run {} panicked",
                    handle.info.run_id
                );
            }
        }
        self.publish_request_frames();
    }

    // ------------------------------------------------------------------ arm

    /// Pin, record and start a run. On any failure the new directory is removed and a replaced
    /// run stays as it was.
    pub fn arm(&self, plan: ArmPlan) -> Result<String, String> {
        // One arm at a time, and none beside a start-up.
        let mut registry = lock(&self.runs);
        if !valid_spine(&plan.spine) {
            return Err(format!(
                "spine name {:?} must match [A-Za-z0-9._-]+",
                plan.spine
            ));
        }
        let infos: Vec<RunInfo> =
            { drop_guard_infos(&registry, &self.store, |h| self.running_sessions(h)) };
        check_earlier(&infos, plan.replace.as_deref()).map_err(strip_prefix)?;

        ledger_is_writable(&plan.ledger)?;

        // A run ID that the store knows is never reused. A store that cannot answer counts as
        // knowing it.
        let (run_id, dir) = files::create_run_dir(&self.cfg.state_dir, &plan.spine, &|id| {
            self.store.run(id).map_or(true, |row| row.is_some())
        })?;
        match self.arm_into(&plan, &run_id, &dir, &registry) {
            Ok(handle) => {
                registry.push(handle);
                Ok(run_id)
            }
            Err(why) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(why)
            }
        }
    }

    fn arm_into(
        &self,
        plan: &ArmPlan,
        run_id: &str,
        dir: &Path,
        registry: &[Arc<RunHandle>],
    ) -> Result<Arc<RunHandle>, String> {
        for sub in ["queue", "out", "package", "plugins"] {
            std::fs::create_dir_all(dir.join(sub))
                .map_err(|e| format!("cannot make {sub}: {e}"))?;
        }
        // The package: only the pinned files, from the source, into the run.
        let source = Package::load(&plan.workflow_dir).map_err(|e| e.to_string())?;
        files::copy_files(
            &plan.workflow_dir,
            &dir.join("package"),
            &source.pinned_files(),
        )?;
        let package = Package::load(&dir.join("package")).map_err(|e| e.to_string())?;
        let package_pin =
            pin::pin_package(&package).map_err(|e| format!("cannot pin the package: {e}"))?;

        // The ports resolve each plugin; the copy is what the seats launch from.
        let spec = RunSpec {
            run_id: run_id.to_string(),
            run_dir: dir.to_path_buf(),
            runtimes: package
                .file
                .roles
                .iter()
                .map(|r| r.runtime.clone())
                .collect(),
            plugin_roots: plan.plugin_roots.clone(),
            paper: plan.paper.clone(),
            forge_repo: plan.forge_repo.clone(),
        };
        let run_ports = self.ports.build(&spec)?;
        let mut copies = Vec::new();
        let mut plugin_pins = Vec::new();
        for req in &package.file.plugins {
            let found = run_ports
                .agent
                .resolve_plugin(&req.name, &req.min_version)
                .map_err(|e| format!("plugin {}: {e}", req.name))?;
            let copy = dir.join("plugins").join(&req.name);
            files::copy_tree(Path::new(&found.path), &copy)?;
            let p = pin::pin_dir(&req.name, &found.version, Some(&req.min_version), &copy)
                .map_err(|e| format!("cannot pin plugin {}: {e}", req.name))?;
            plugin_pins.push(p);
            copies.push(PluginCopy {
                name: req.name.clone(),
                version: found.version,
                min_version: req.min_version.clone(),
                path: copy,
            });
        }
        let manifest = Manifest {
            package: package_pin,
            plugins: plugin_pins,
        };
        files::write_json(&dir.join("manifest.json"), &manifest)?;

        let mut params = plan.params.clone();
        params.insert(
            "briefs".to_string(),
            dir.join("package/briefs").display().to_string(),
        );
        params.insert("forge_repo".to_string(), plan.forge_repo.clone());
        let info = RunJson {
            run_id: run_id.to_string(),
            spine: plan.spine.clone(),
            workflow: package.file.name.clone(),
            params,
            env: if plan.paper.is_some() {
                "paper"
            } else {
                "live"
            }
            .to_string(),
            ledger: plan.ledger.clone(),
            plugin_roots: plan.plugin_roots.clone(),
            paper: plan.paper.clone(),
            forge_repo: plan.forge_repo.clone(),
            default_branch: plan.default_branch.clone(),
            target_dir: plan.target_dir.clone(),
            plugins: copies,
            created: pf_engine::engine::format_utc(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs()),
                0,
            ),
        };
        let handle = self.build_run(dir, info.clone(), Some(run_ports))?;
        if let Err(why) = self.finish_arm(plan, run_id, dir, registry, &info, &handle) {
            // Start-up committed the run to the store, which outlives the directory that `arm`
            // removes. End the run, so no open run is left without a directory. An engine that
            // degraded cannot end it: the ID stays taken, and no later arm reuses it.
            let _ = handle.engine.end_run(&format!("arm failed: {why}"));
            return Err(why);
        }
        Ok(handle)
    }

    /// The steps of an arm after the new run started. A failure here leaves the replaced run as
    /// it was: it ends last, after everything that can fail has passed.
    fn finish_arm(
        &self,
        plan: &ArmPlan,
        run_id: &str,
        dir: &Path,
        registry: &[Arc<RunHandle>],
        info: &RunJson,
        handle: &RunHandle,
    ) -> Result<(), String> {
        if !handle.driven.load(Ordering::SeqCst) {
            return Err("the new run did not start up".to_string());
        }
        let old = match &plan.replace {
            Some(old) => Some(
                registry
                    .iter()
                    .find(|r| &r.info.run_id == old)
                    .ok_or_else(|| format!("run {old} is unknown"))?,
            ),
            None => None,
        };
        files::write_json(&dir.join("run.json"), info)?;
        // The replaced run ends after the new run's pins, start-up and record succeeded.
        if let Some(old) = old {
            old.engine
                .end_run(&format!("replaced by {run_id}"))
                .map_err(|e| format!("cannot end run {}: {e}", old.info.run_id))?;
            old.driven.store(false, Ordering::SeqCst);
            old.driver.sweep();
        }
        Ok(())
    }
}

/// The ledger can be appended to. The check runs before start-up commits anything, so a ledger
/// that cannot be written leaves no run in the store.
fn ledger_is_writable(path: &Path) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|e| format!("the ledger {} cannot be written: {e}", path.display()))
}

fn valid_spine(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn strip_prefix(message: String) -> String {
    message
        .strip_prefix("pulse-factory: arm refused: ")
        .unwrap_or(&message)
        .to_string()
}

fn drop_guard_infos(
    registry: &[Arc<RunHandle>],
    store: &SqliteStore,
    running: impl Fn(&RunHandle) -> u64,
) -> Vec<RunInfo> {
    registry
        .iter()
        .map(|h| RunInfo {
            id: h.info.run_id.clone(),
            spine: h.info.spine.clone(),
            state: store
                .run(&h.info.run_id)
                .ok()
                .flatten()
                .map_or("active", |r| r.state.as_str())
                .to_string(),
            running_sessions: running(h),
        })
        .collect()
}

/// The refusal of a confirm code on the socket.
const SOCKET_CODE_REFUSAL: &str =
    "{verb} refused: confirm codes are for paired devices; on this host use pulse-factory claim";

impl Handler for Controller {
    fn handle(&self, request: Request) -> Response {
        match request.verb.as_str() {
            "runs" => Response::ok(json!({ "runs": self.run_infos() })),
            // The Needs you page's read of the runs (`r1.s4.w3`): device auth only, read-only.
            "run_list" => self.verb_run_list(),
            "hook" => Response::ok(self.seats.answer(&request.args)),
            "status" => self.verb_status(&request.args),
            "halt" => self.verb_halt(&request.args),
            "requests" => self.verb_requests(&request.args),
            "show" => self.verb_show(&request.args),
            "project_add" => self.verb_project_add(&request.args),
            "project_list" => self.verb_project_list(),
            "project_remove" => self.verb_project_remove(&request.args),
            "session_start" => self.verb_session_start(&request.args),
            "session_send" => self.verb_session_send(&request.args),
            "session_answer" => self.verb_session_answer(&request.args),
            "session_interrupt" => self.verb_session_interrupt(&request.args),
            "session_set" => self.verb_session_set(&request.args),
            "session_end" => self.verb_session_end(&request.args),
            "session_redact" => self.verb_session_redact(&request.args),
            "session_list" => self.verb_session_list(&request.args),
            "session_items" => self.verb_session_items(&request.args),
            "settings_get" => self.verb_settings_get(&request.args),
            "device_list" => self.verb_device_list(),
            "api_status" => self.verb_api_status(),
            // A confirm code is for a paired device, on the API (`r1.s4.w1`).
            "confirm_code" => Response::err(SOCKET_CODE_REFUSAL.replace("{verb}", "confirm_code")),
            // These change who acts and what runs. They need to know who calls.
            "arm" | "claim" | "answer" | "clear" | "ack" | "device_pair" | "device_revoke"
            | "settings_set" | "settings_unset" => Response::err(format!(
                "{} refused: the controller cannot tell which process called",
                request.verb
            )),
            other => Response::err(format!("unknown verb {other:?}")),
        }
    }

    fn handle_from(&self, request: Request, peer: Peer) -> Response {
        let verb = request.verb.clone();
        let response = self.dispatch_from(request, peer);
        // The verbs that change a request or the claim tell the open streams at once.
        if matches!(verb.as_str(), "claim" | "answer" | "clear" | "ack") {
            self.publish_request_frames();
        }
        response
    }
}

impl Controller {
    fn dispatch_from(&self, request: Request, peer: Peer) -> Response {
        // A confirm code is for a paired device: the socket takes a claim token and nothing else.
        if matches!(request.verb.as_str(), "answer" | "clear" | "ack")
            && request.args.get("code").is_some()
            && request.args.get("token").is_none()
        {
            return Response::err(SOCKET_CODE_REFUSAL.replace("{verb}", &request.verb));
        }
        match request.verb.as_str() {
            "claim" => self.verb_claim(&request.args, peer),
            "answer" => self.verb_answer(&request.args, peer),
            "clear" => self.verb_clear(&request.args, peer),
            "ack" => self.verb_ack(&request.args, peer),
            // Neither can a chat, or a process below one (ADR-0005; `r1.s4.w1`).
            "device_pair" => match self.check_chat_caller("device_pair", peer) {
                Ok(()) => self.verb_device_pair(),
                Err(why) => Response::err(why),
            },
            "device_revoke" => match self.check_chat_caller("device_revoke", peer) {
                Ok(()) => self.verb_device_revoke(&request.args),
                Err(why) => Response::err(why),
            },
            // A seat cannot change a setting, and neither can a chat. The refusal is audited, with a
            // bound (ADR-0016).
            verb @ ("settings_set" | "settings_unset") => {
                self.settings_write_from(verb, &request.args, peer)
            }
            "arm" => match self.check_chat_caller("arm", peer) {
                Ok(()) => match serde_json::from_value::<ArmPlan>(request.args) {
                    Ok(plan) => match self.arm(plan) {
                        Ok(id) => Response::ok(json!({ "run_id": id })),
                        Err(why) => Response::err(why),
                    },
                    Err(e) => Response::err(format!("the plan is not readable: {e}")),
                },
                Err(why) => Response::err(why),
            },
            // A seat cannot drive a chat, and neither can a chat (ADR-0005, Q2 rule 2).
            verb @ ("session_start" | "session_send" | "session_answer" | "session_interrupt"
            | "session_set" | "session_end" | "session_redact") => {
                match self.check_chat_caller(verb, peer) {
                    Ok(()) => self.handle(request),
                    Err(why) => Response::err(why),
                }
            }
            _ => self.handle(request),
        }
    }
}

impl Controller {
    fn verb_status(&self, args: &Value) -> Response {
        let run = args.get("run").and_then(Value::as_str);
        match self.find(run) {
            Ok(handle) => Response::ok(status::build(self, &handle)),
            Err(why) => Response::err(why),
        }
    }

    fn verb_halt(&self, args: &Value) -> Response {
        let run = args.get("run").and_then(Value::as_str);
        let reason = args
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // The kill switch stops every live chat process first (Q2 rule 3).
        let chats_stopped = self.halt_chat_json();
        let handle = match self.find(run) {
            Ok(h) => h,
            // With no run at all, `halt` still stopped the chats.
            Err(_) if run.is_none() && lock(&self.runs).is_empty() => {
                return Response::ok(
                    json!({ "schema_version": 1, "run": null, "halted": false, "already_halted": false, "chats_stopped": chats_stopped }),
                );
            }
            Err(why) => return Response::err(why),
        };
        match handle.engine.halt(reason) {
            Ok(_) => Response::ok(
                json!({ "schema_version": 1, "run": handle.info.run_id, "halted": true, "already_halted": false, "chats_stopped": chats_stopped }),
            ),
            // The first reason stays.
            Err(pf_engine::EngineError::Conflict { .. }) => {
                let first = self
                    .store
                    .run(&handle.info.run_id)
                    .ok()
                    .flatten()
                    .and_then(|r| r.halted_reason)
                    .unwrap_or_default();
                Response::ok(
                    json!({ "schema_version": 1, "run": handle.info.run_id, "halted": true, "already_halted": true, "reason": first, "chats_stopped": chats_stopped }),
                )
            }
            Err(e) => Response::err(e.to_string()),
        }
    }
}

/// The `controller` verb: run in the foreground until SIGTERM or SIGINT.
pub fn run(args: &ControllerArgs) -> i32 {
    let state = pf_ipc::state_dir();
    if let Err(e) = pf_ipc::prepare_state_dir(&state) {
        eprintln!(
            "pulse-factory: controller: cannot prepare {}: {e}",
            state.display()
        );
        return 1;
    }
    let bound = match pf_ipc::bind(&state) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pulse-factory: controller: {e}");
            return 1;
        }
    };
    let mut cfg = Config::new(
        state,
        args.tick_ms,
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .unwrap_or_else(|_| PathBuf::from("pulse-factory")),
    );
    // The flag wins over the environment variable. Neither set: the API is off.
    cfg.api_listen = args
        .api_listen
        .clone()
        .or_else(|| std::env::var("PF_API_LISTEN").ok())
        .filter(|a| !a.trim().is_empty());
    // The chat settings: the unit's `controller.env` (`PF_SHARE_DIR`, `PF_CHAT_CLAUDE_BIN`, ...).
    let vars: BTreeMap<String, String> = std::env::vars().collect();
    let (chat_env, warnings) = chats::parse_chat_env(&vars, chats::read_hostname().as_deref());
    for warning in warnings {
        eprintln!("{warning}");
    }
    cfg.apply_chat_env(&chat_env);
    if let Some(bin) = &chat_env.claude_bin {
        eprintln!(
            "pulse-factory: the chat runs an unqualified Claude Code binary: {}",
            bin.display()
        );
    }
    let mut claude = pf_adapter_claude::ClaudeConfig::new(chat_env.share_dir.clone());
    claude.claude_bin_override = chat_env.claude_bin.clone();
    cfg.chat_managed = chat_env.claude_bin.is_none();
    cfg.chat_runtimes.insert(
        pf_adapter_claude::CLAUDE_CODE_RUNTIME.to_string(),
        Arc::new(pf_adapter_claude::ClaudeChat::new(claude)),
    );
    let controller = match Controller::open(cfg, Arc::new(LivePorts)) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("pulse-factory: controller: {why}");
            return 1;
        }
    };
    serve_forever(controller, bound)
}

/// Serve the socket and tick until a signal ends the process.
pub fn serve_forever(controller: Arc<Controller>, bound: pf_ipc::Bound) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pulse-factory: controller: cannot start the runtime: {e}");
            return 1;
        }
    };
    let period = Duration::from_millis(controller.cfg.tick_ms.max(10));
    rt.block_on(async move {
        // The API listens beside the socket, on this runtime. It is off with no address.
        let api_task = api::spawn_api(&controller);
        let ticker = controller.clone();
        let tick_task = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let c = ticker.clone();
                let _ = tokio::task::spawn_blocking(move || c.tick_all()).await;
            }
        });
        let shutdown = async {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut int)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
            ) else {
                std::future::pending::<()>().await;
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        };
        pf_ipc::serve(bound, controller.clone(), shutdown).await;
        // A controller stop ends every chat process and every stream (Q2 rule 3).
        let stopping = controller.clone();
        let _ = tokio::task::spawn_blocking(move || stopping.shutdown_chats()).await;
        tick_task.abort();
        if let Some(task) = api_task {
            task.abort();
        }
    });
    0
}
