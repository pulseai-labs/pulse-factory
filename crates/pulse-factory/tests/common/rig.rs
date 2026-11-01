//! A driver on a fake adapter and a bare forge, with a clock the test moves.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pf_adapter_fake::{FakeAdapter, Scenario, Step};
use pf_engine::port::agent::{AgentPort, SessionRef};
use pf_engine::port::store::Store;
use pf_engine::port::unit_source::UnitRecord;
use pf_engine::status::Status;
use pf_engine::{Engine, EngineConfig, RunView, system_clock};
use pf_forge_bare::{BareForge, PrChecks};
use pf_store_sqlite::SqliteStore;
use pf_workflow::package::Package;
use pf_workflow::pin;
use pulse_factory::controller::driver::{Driver, DriverConfig};

use super::{Options, git, write_package};

pub const BASE: &str = "spine";

pub struct Rig {
    pub root: tempfile::TempDir,
    pub package_dir: PathBuf,
    pub run_dir: PathBuf,
    pub spine_dir: PathBuf,
    pub bare: PathBuf,
    pub work: PathBuf,
    pub ledger: PathBuf,
    pub store: Arc<SqliteStore>,
    pub fake: Arc<FakeAdapter>,
    pub forge: BareForge,
    pub engine: Arc<Engine>,
    pub driver: Driver,
    pub now: Arc<AtomicU64>,
    pub plugin_dir: PathBuf,
    pub package: Package,
    pub pins: Vec<pin::Pin>,
    pub params: BTreeMap<String, String>,
    pub agent: Arc<dyn AgentPort>,
    pub cfg: EngineConfig,
}

/// The time every test starts at, in seconds: the real time, so that the modified time of a file
/// the test makes is not after the test clock.
pub fn t0() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl Rig {
    pub fn new() -> Rig {
        Rig::with(&Options::default(), |_| {})
    }

    pub fn with(o: &Options, tweak: impl FnOnce(&mut BTreeMap<String, String>)) -> Rig {
        Rig::build(o, tweak, |fake| fake)
    }

    /// Build a rig whose engine and driver reach the fake through `wrap`.
    pub fn build(
        o: &Options,
        tweak: impl FnOnce(&mut BTreeMap<String, String>),
        wrap: impl FnOnce(Arc<FakeAdapter>) -> Arc<dyn AgentPort>,
    ) -> Rig {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().to_path_buf();
        let package_dir = p.join("run/package");
        let run_dir = p.join("run");
        let spine_dir = p.join("spine");
        let bare = p.join("forge.git");
        let work = p.join("work");
        let ledger = p.join("ledger.md");
        let plugin_dir = p.join("run/plugins/methodkit");
        for d in [
            &run_dir,
            &spine_dir,
            &run_dir.join("queue"),
            &run_dir.join("out"),
            &plugin_dir,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(plugin_dir.join("SKILL.md"), "skill\n").unwrap();
        write_package(&package_dir, o);

        std::fs::create_dir_all(&bare).unwrap();
        git(
            &bare,
            &["init", "--quiet", "--bare", "--initial-branch=spine"],
        );
        git(
            &p,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        git(&work, &["config", "user.name", "Test"]);
        git(&work, &["config", "user.email", "test@example.invalid"]);
        git(&work, &["checkout", "--quiet", "-b", BASE]);
        std::fs::write(work.join("README"), "start\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "start"]);
        git(&work, &["push", "--quiet", "origin", BASE]);
        let forge = BareForge::open(&bare).unwrap();

        let package = Package::load(&package_dir).unwrap();
        let pins = vec![
            pin::pin_package(&package).unwrap(),
            pin::pin_dir("methodkit", "1.26.1", Some("1.26.0"), &plugin_dir).unwrap(),
        ];
        let mut params = BTreeMap::new();
        params.insert("spine".to_string(), "s1".to_string());
        params.insert("spine_branch".to_string(), BASE.to_string());
        params.insert("spine_dir".to_string(), spine_dir.display().to_string());
        params.insert("repo".to_string(), work.display().to_string());
        params.insert("forge_repo".to_string(), forge.repo().to_string());
        params.insert("count".to_string(), "2".to_string());
        params.insert(
            "briefs".to_string(),
            package_dir.join("briefs").display().to_string(),
        );
        tweak(&mut params);

        let store = Arc::new(SqliteStore::open(p.join("store.sqlite")).unwrap());
        let fake = Arc::new(FakeAdapter::new());
        // A lane ends its turn when the test finishes its session.
        for lane in ["lane", "lane.2", "lane.3"] {
            fake.script(lane, Scenario::Script(vec![Step::TurnEnd]));
        }
        let agent = wrap(fake.clone());
        let cfg = EngineConfig {
            run_id: "r1".into(),
            workflow: package.file.to_workflow(),
            ledger_path: ledger.clone(),
            state_dir: p.join("state"),
            run_dir: run_dir.clone(),
            params: params.clone(),
        };
        let engine = Arc::new(
            Engine::new(
                cfg.clone(),
                store.clone() as Arc<dyn Store>,
                agent.clone(),
                Arc::new(forge.clone()),
                system_clock(),
                Box::new(pf_engine::ledger::FsLedger::new(ledger.clone())),
            )
            .unwrap(),
        );
        engine.start_up().unwrap();
        let now = Arc::new(AtomicU64::new(t0()));
        let clock_now = now.clone();
        let driver = Driver::new(DriverConfig {
            engine: engine.clone(),
            agent: agent.clone(),
            package: package.clone(),
            pins: pins.clone(),
            params: params.clone(),
            run_dir: run_dir.clone(),
            target_dir: String::new(),
            clock: Arc::new(move || clock_now.load(Ordering::SeqCst)),
            walk_limit: pulse_factory::controller::driver::WALK_LIMIT,
        });
        Rig {
            root,
            package_dir,
            run_dir,
            spine_dir,
            bare,
            work,
            ledger,
            store,
            fake,
            forge,
            engine,
            driver,
            now,
            plugin_dir,
            package,
            pins,
            params,
            agent,
            cfg,
        }
    }

    pub fn tick(&self) -> Vec<String> {
        self.driver.tick();
        self.driver.notes()
    }

    pub fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }

    pub fn view(&self) -> RunView {
        self.engine.view().unwrap()
    }

    pub fn unit(&self, id: &str) -> (Status, Option<String>) {
        let v = self.view();
        let n = v.node(id).unwrap_or_else(|| panic!("no unit {id}"));
        (n.status, n.reason.clone())
    }

    pub fn has_unit(&self, id: &str) -> bool {
        self.view().node(id).is_some()
    }

    pub fn ledger_text(&self) -> String {
        std::fs::read_to_string(&self.ledger).unwrap_or_default()
    }

    pub fn ledger_lines(&self, needle: &str) -> Vec<String> {
        self.ledger_text()
            .lines()
            .filter(|l| l.contains(needle))
            .map(str::to_string)
            .collect()
    }

    /// Make an item's worktree (a git repo under the spine directory) and publish its record.
    pub fn queue_item(&self, id: &str, order: u32) -> PathBuf {
        let wt = self.spine_dir.join(format!("wt-{id}"));
        std::fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q", "-b", &format!("work/{id}")]);
        let mut vars = BTreeMap::new();
        vars.insert("worktree_path".to_string(), wt.display().to_string());
        vars.insert("branch".to_string(), format!("work/{id}"));
        let rec = UnitRecord::new(id, "g1", order, vars);
        self.publish(&format!("{id}.json"), &rec.to_json());
        wt
    }

    pub fn publish(&self, name: &str, text: &str) {
        let q = self.run_dir.join("queue");
        let tmp = q.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, q.join(name)).unwrap();
    }

    pub fn script(&self, unit: &str, steps: Vec<Step>) {
        self.fake.script(unit, Scenario::Script(steps));
    }

    /// The adapter reference of the open session of a unit.
    pub fn session_ref(&self, unit: &str) -> Option<SessionRef> {
        self.view()
            .sessions
            .iter()
            .rfind(|s| s.unit_id == unit && !s.ended)
            .and_then(|s| s.process_ref.clone())
            .map(SessionRef)
    }

    pub fn open_sessions(&self, unit: &str) -> usize {
        self.view()
            .sessions
            .iter()
            .filter(|s| s.unit_id == unit && !s.ended)
            .count()
    }

    pub fn all_sessions(&self, unit: &str) -> usize {
        self.view()
            .sessions
            .iter()
            .filter(|s| s.unit_id == unit)
            .count()
    }

    pub fn session_cost(&self, id: &str) -> (u64, u64) {
        let v = self.view();
        let s = v.session(id).unwrap();
        (s.input_tokens, s.cost_micros)
    }

    pub fn stage_node(&self, unit: &str, stage: &str) -> Vec<(String, Status, Option<String>)> {
        self.view()
            .stages_of(unit)
            .iter()
            .filter(|n| n.detail == stage)
            .map(|n| (n.id.clone(), n.status, n.reason.clone()))
            .collect()
    }

    /// Commit on a branch in the bare repo and write a PR for it. Returns the head.
    pub fn pr(&self, number: u64, branch: &str, checks: PrChecks) -> String {
        git(&self.work, &["fetch", "--quiet", "origin"]);
        git(
            &self.work,
            &[
                "checkout",
                "--quiet",
                "-B",
                branch,
                &format!("origin/{BASE}"),
            ],
        );
        std::fs::write(
            self.work.join(format!("{branch}.txt").replace('/', "_")),
            "x\n",
        )
        .unwrap();
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "--quiet", "-m", branch]);
        git(&self.work, &["push", "--quiet", "origin", branch]);
        self.forge.write_pr(number, branch, BASE, checks).unwrap();
        git(&self.bare, &["rev-parse", &format!("refs/heads/{branch}")])
    }

    /// Write the file that a unit's close stage prints as its PR.
    pub fn pr_url(&self, unit: &str, repo: &str, number: u64) {
        let d = self.run_dir.join("out").join(unit);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("pr.url"), format!("file://{repo}#{number}\n")).unwrap();
    }

    pub fn touch(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "x\n").unwrap();
    }

    pub fn out(&self, unit: &str, name: &str) -> PathBuf {
        self.run_dir.join("out").join(unit).join(name)
    }

    pub fn report(&self, unit: &str) -> PathBuf {
        self.spine_dir
            .join(format!("work-{unit}"))
            .join("report.md")
    }
}

impl Rig {
    /// An item with a PR, a script that pushes the PR's head and ends the turn, and a queue
    /// record. It returns the head.
    pub fn item(&self, id: &str, order: u32, number: u64, checks: PrChecks) -> String {
        let head = self.pr(number, &format!("work/{id}"), checks);
        self.script(
            id,
            vec![
                Step::Push {
                    branch: format!("work/{id}"),
                    sha: head.clone(),
                    outcome: pf_engine::admission::PushOutcome::Pushed,
                },
                Step::TurnEnd,
            ],
        );
        self.queue_item(id, order);
        self.pr_url(id, self.forge.repo(), number);
        // The PR URL file must not exist before the close stage: the `pr` condition would be met
        // early. Remove it; `reach_close` writes it back.
        let _ = std::fs::remove_file(self.out(id, "pr.url"));
        head
    }

    /// Pass a stage: write its done file, end the turn, tick.
    pub fn pass(&self, unit: &str, done_file: &Path) {
        self.touch(done_file);
        self.fake
            .finish(&self.session_ref(unit).expect("an open session"));
        self.tick();
    }

    /// Pass `implement` and `verify`, so the unit's close session is open.
    pub fn reach_close(&self, unit: &str) {
        self.tick();
        self.pass(unit, &self.report(unit));
        self.pass(unit, &self.out(unit, "verdict.json"));
        assert_eq!(
            self.stage_node(unit, "close").len(),
            1,
            "close must be running"
        );
    }

    /// Print the PR and end the close turn: the unit merges in this tick.
    pub fn close(&self, unit: &str, number: u64) {
        self.pr_url(unit, self.forge.repo(), number);
        self.fake.finish(&self.session_ref(unit).unwrap());
        self.tick();
    }
}

impl Rig {
    /// A restart: a new engine and a new driver over the same store, ledger and adapter. They
    /// remember nothing of the first driver, and hold none of its slots.
    pub fn fresh_driver(&self, target_dir: &str, walk_limit: usize) -> Driver {
        let now = self.now.clone();
        let engine = Arc::new(
            Engine::new(
                self.cfg.clone(),
                self.store.clone() as Arc<dyn Store>,
                self.agent.clone(),
                Arc::new(self.forge.clone()),
                system_clock(),
                Box::new(pf_engine::ledger::FsLedger::new(self.ledger.clone())),
            )
            .unwrap(),
        );
        engine.start_up().unwrap();
        Driver::new(DriverConfig {
            engine,
            agent: self.agent.clone(),
            package: self.package.clone(),
            pins: self.pins.clone(),
            params: self.params.clone(),
            run_dir: self.run_dir.clone(),
            target_dir: target_dir.to_string(),
            clock: Arc::new(move || now.load(Ordering::SeqCst)),
            walk_limit,
        })
    }

    /// The test clock now.
    pub fn clock(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

/// Set the modified time of a file or directory to `secs` since the epoch.
pub fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::open(path).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}
