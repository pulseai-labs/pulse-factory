//! The controller: one process, seats as children (ADR-0001). It owns the store, the queue and
//! the ledger (ADR-0003).

pub mod client;
pub mod driver;
pub mod files;
pub mod ports;
pub mod seat;
pub mod status;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pf_engine::ledger::FsLedger;
use pf_engine::port::agent::{AgentPort, SessionRef};
use pf_engine::port::store::{RunState, Store};
use pf_engine::{Engine, EngineConfig, system_clock};
use pf_ipc::{Handler, Request, Response};
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
}

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
        let controller = Arc::new(Controller {
            cfg,
            store,
            ports,
            runs: Mutex::new(Vec::new()),
            seats: Arc::new(seat::SeatTable::default()),
        });
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
            clock: system_secs(),
            walk_limit: driver::WALK_LIMIT,
        }));
        let mut driven = !ended;
        if !ended {
            match engine.start_up() {
                Ok(()) => driver.sweep(),
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
            binary: self.cfg.binary.clone(),
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

        let (run_id, dir) = files::create_run_dir(&self.cfg.state_dir, &plan.spine)?;
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
        if !handle.driven.load(Ordering::SeqCst) {
            return Err("the new run did not start up".to_string());
        }
        // The replaced run ends after the new run's pins and start-up succeeded.
        if let Some(old) = &plan.replace {
            let old = registry
                .iter()
                .find(|r| &r.info.run_id == old)
                .ok_or_else(|| format!("run {old} is unknown"))?;
            old.engine
                .end_run(&format!("replaced by {run_id}"))
                .map_err(|e| format!("cannot end run {}: {e}", old.info.run_id))?;
            old.driven.store(false, Ordering::SeqCst);
            old.driver.sweep();
        }
        files::write_json(&dir.join("run.json"), &info)?;
        Ok(handle)
    }
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

impl Handler for Controller {
    fn handle(&self, request: Request) -> Response {
        match request.verb.as_str() {
            "runs" => Response::ok(json!({ "runs": self.run_infos() })),
            "arm" => match serde_json::from_value::<ArmPlan>(request.args) {
                Ok(plan) => match self.arm(plan) {
                    Ok(id) => Response::ok(json!({ "run_id": id })),
                    Err(why) => Response::err(why),
                },
                Err(e) => Response::err(format!("the plan is not readable: {e}")),
            },
            "hook" => Response::ok(self.seats.answer(&request.args)),
            "status" => self.verb_status(&request.args),
            "halt" => self.verb_halt(&request.args),
            other => Response::err(format!("unknown verb {other:?}")),
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
        let handle = match self.find(run) {
            Ok(h) => h,
            Err(why) => return Response::err(why),
        };
        match handle.engine.halt(reason) {
            Ok(_) => Response::ok(
                json!({ "schema_version": 1, "run": handle.info.run_id, "halted": true, "already_halted": false }),
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
                    json!({ "schema_version": 1, "run": handle.info.run_id, "halted": true, "already_halted": true, "reason": first }),
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
    let controller = match Controller::open(
        Config {
            state_dir: state,
            tick_ms: args.tick_ms,
            binary: std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .unwrap_or_else(|_| PathBuf::from("pulse-factory")),
        },
        Arc::new(LivePorts),
    ) {
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
        tick_task.abort();
    });
    0
}
