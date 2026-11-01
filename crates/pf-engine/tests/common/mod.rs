//! Fake ports and helpers shared by the engine tests. All data here is synthetic.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pf_engine::capacity::Settings;
use pf_engine::ledger::{FsLedger, LedgerFile};
use pf_engine::port::agent::*;
use pf_engine::port::forge::*;
use pf_engine::port::store::*;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::workflow::{MergeDef, Track};
use pf_engine::{Clock, Engine, EngineConfig, MergeRule, StageDef, Workflow};

// ---------------------------------------------------------------- store

#[derive(Default)]
struct StoreState {
    runs: BTreeMap<String, RunRow>,
    nodes: Vec<NodeRow>,
    sessions: Vec<SessionRow>,
    admissions: Vec<AdmissionRow>,
    outbox: Vec<LedgerLine>,
}

/// An in-memory store that keeps the port's promises: one unique command ID per run, content
/// compared byte for byte, and a commit that applies all of its writes or none.
#[derive(Default)]
pub struct FakeStore {
    state: Mutex<StoreState>,
    pub fail_writes: AtomicBool,
}

impl FakeStore {
    pub fn new() -> Arc<FakeStore> {
        Arc::new(FakeStore::default())
    }

    pub fn outbox(&self) -> Vec<LedgerLine> {
        self.state.lock().unwrap().outbox.clone()
    }

    /// Forget that a line was written, to simulate a stop before the mark.
    pub fn unmark(&self, seq: u64) {
        for line in &mut self.state.lock().unwrap().outbox {
            if line.seq == seq {
                line.written = false;
            }
        }
    }

    pub fn node(&self, run: &str, id: &str) -> Option<NodeRow> {
        self.state
            .lock()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.run_id == run && n.id == id)
            .cloned()
    }
}

impl Store for FakeStore {
    fn commit(&self, commit: Commit) -> Result<Committed, StoreError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(StoreError("forced write failure".to_string()));
        }
        let mut s = self.state.lock().unwrap();
        if let Some(existing) = s
            .admissions
            .iter()
            .find(|a| a.run_id == commit.run_id && a.command_id == commit.command_id)
        {
            return Ok(if existing.content == commit.content {
                Committed::Replayed(existing.clone())
            } else {
                Committed::Conflict(existing.clone())
            });
        }
        let row = AdmissionRow {
            seq: i64::try_from(s.admissions.len()).unwrap() + 1,
            run_id: commit.run_id.clone(),
            command_id: commit.command_id.clone(),
            kind: commit.admission.kind.clone(),
            content: commit.content.clone(),
            outcome: commit.admission.outcome,
            reason: commit.admission.reason.clone(),
            unit_id: commit.admission.unit_id.clone(),
            role: commit.admission.role.clone(),
            model: commit.admission.model.clone(),
            attempt: commit.admission.attempt,
            admitted_at: commit.admission.admitted_at.clone(),
        };
        s.admissions.push(row.clone());
        for effect in commit.effects {
            match effect {
                Effect::PutRun(r) => {
                    s.runs.insert(r.id.clone(), r);
                }
                Effect::PutNode(n) => {
                    match s
                        .nodes
                        .iter_mut()
                        .find(|x| x.run_id == n.run_id && x.id == n.id)
                    {
                        Some(slot) => *slot = n,
                        None => s.nodes.push(n),
                    }
                }
                Effect::PutSession(n) => {
                    match s
                        .sessions
                        .iter_mut()
                        .find(|x| x.run_id == n.run_id && x.id == n.id)
                    {
                        Some(slot) => *slot = n,
                        None => s.sessions.push(n),
                    }
                }
            }
        }
        let ledger = commit.ledger.map(|entry| {
            let seq = s
                .outbox
                .iter()
                .filter(|l| l.ledger_file == entry.ledger_file)
                .map(|l| l.seq)
                .max()
                .unwrap_or(0)
                + 1;
            let line = LedgerLine {
                ledger_file: entry.ledger_file,
                seq,
                run_id: commit.run_id.clone(),
                body: entry.body,
                written: false,
            };
            s.outbox.push(line.clone());
            line
        });
        Ok(Committed::Applied {
            admission: row,
            ledger,
        })
    }

    fn run(&self, run_id: &str) -> Result<Option<RunRow>, StoreError> {
        Ok(self.state.lock().unwrap().runs.get(run_id).cloned())
    }

    fn nodes(&self, run_id: &str) -> Result<Vec<NodeRow>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .nodes
            .iter()
            .filter(|n| n.run_id == run_id)
            .cloned()
            .collect())
    }

    fn sessions(&self, run_id: &str) -> Result<Vec<SessionRow>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .sessions
            .iter()
            .filter(|n| n.run_id == run_id)
            .cloned()
            .collect())
    }

    fn admissions(&self, run_id: &str) -> Result<Vec<AdmissionRow>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .admissions
            .iter()
            .filter(|n| n.run_id == run_id)
            .cloned()
            .collect())
    }

    fn admission(
        &self,
        run_id: &str,
        command_id: &str,
    ) -> Result<Option<AdmissionRow>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .admissions
            .iter()
            .find(|a| a.run_id == run_id && a.command_id == command_id)
            .cloned())
    }

    fn unwritten_ledger_lines(&self, ledger_file: &str) -> Result<Vec<LedgerLine>, StoreError> {
        let mut lines: Vec<LedgerLine> = self
            .state
            .lock()
            .unwrap()
            .outbox
            .iter()
            .filter(|l| l.ledger_file == ledger_file && !l.written)
            .cloned()
            .collect();
        lines.sort_by_key(|l| l.seq);
        Ok(lines)
    }

    fn mark_ledger_written(&self, ledger_file: &str, seq: u64) -> Result<(), StoreError> {
        for line in &mut self.state.lock().unwrap().outbox {
            if line.ledger_file == ledger_file && line.seq == seq {
                line.written = true;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- agent

#[derive(Default)]
pub struct FakeAgent {
    pub launches: Mutex<Vec<LaunchPlan>>,
    pub refuse_units: Mutex<HashSet<String>>,
    pub dead: Mutex<HashSet<String>>,
    pub interrupts: AtomicU64,
    next: AtomicU64,
}

impl FakeAgent {
    pub fn new() -> Arc<FakeAgent> {
        Arc::new(FakeAgent::default())
    }

    pub fn refuse(&self, unit: &str) {
        self.refuse_units.lock().unwrap().insert(unit.to_string());
    }

    pub fn kill(&self, process_ref: &str) {
        self.dead.lock().unwrap().insert(process_ref.to_string());
    }

    pub fn launch_count(&self) -> usize {
        self.launches.lock().unwrap().len()
    }
}

impl AgentPort for FakeAgent {
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        if self.refuse_units.lock().unwrap().contains(&plan.unit_id) {
            return Err(AgentError::Refused("agent version is not qualified".into()));
        }
        self.launches.lock().unwrap().push(plan.clone());
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(SessionRef(format!("proc-{n}")))
    }
    fn status(&self, _s: &SessionRef) -> Result<SessionStatus, AgentError> {
        Ok(SessionStatus {
            state: SessionState::Working,
            turn_ended: false,
            first_turn_started: true,
        })
    }
    fn deliver(&self, _s: &SessionRef, _m: &str) -> Result<(), AgentError> {
        Ok(())
    }
    fn interrupt(&self, _s: &SessionRef) -> Result<(), AgentError> {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn usage(&self, _s: &SessionRef) -> Result<Usage, AgentError> {
        Ok(Usage::default())
    }
    fn answer_dialog(&self, _s: &SessionRef, _d: &Dialog) -> Result<DialogAnswer, AgentError> {
        Ok(DialogAnswer::Unknown)
    }
    fn apply_rules(&self, plan: &mut LaunchPlan, rules: &RuleSet) -> Result<(), AgentError> {
        plan.settings
            .insert("rules".to_string(), rules.name.clone());
        Ok(())
    }
    fn remove_ask_tool(&self, plan: &mut LaunchPlan) -> Result<(), AgentError> {
        plan.settings.insert("ask".to_string(), "off".to_string());
        Ok(())
    }
    fn hand_off(&self, _s: &SessionRef, successor: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.launch(successor)
    }
    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError> {
        Ok(!self.dead.lock().unwrap().contains(&session.0))
    }
    fn resolve_plugin(&self, name: &str, _min: &str) -> Result<ResolvedPlugin, AgentError> {
        Err(AgentError::Refused(format!("no plugin {name}")))
    }
    fn drain_events(&self, _s: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------- forge

#[derive(Clone)]
pub struct PrScript {
    pub branch: String,
    pub sha: String,
    pub checks: Checks,
    pub mergeable: Mergeable,
    pub merge: ForgeOutcome,
    pub landed: bool,
}

impl PrScript {
    pub fn green(branch: &str, sha: &str) -> PrScript {
        PrScript {
            branch: branch.to_string(),
            sha: sha.to_string(),
            checks: Checks::Green,
            mergeable: Mergeable::Clean,
            merge: ForgeOutcome::Merged,
            landed: true,
        }
    }
}

#[derive(Default)]
pub struct FakeForge {
    pub scripts: Mutex<HashMap<u64, PrScript>>,
    pub merges: Mutex<Vec<u64>>,
}

impl FakeForge {
    pub fn new() -> Arc<FakeForge> {
        Arc::new(FakeForge::default())
    }

    pub fn script(&self, number: u64, script: PrScript) {
        self.scripts.lock().unwrap().insert(number, script);
    }

    pub fn merged(&self) -> Vec<u64> {
        self.merges.lock().unwrap().clone()
    }

    fn get(&self, pr: &PrRef) -> Result<PrScript, ForgeError> {
        self.scripts
            .lock()
            .unwrap()
            .get(&pr.number)
            .cloned()
            .ok_or_else(|| ForgeError(format!("no PR {}", pr.number)))
    }
}

impl ForgePort for FakeForge {
    fn pr_status(&self, pr: &PrRef) -> Result<PrStatus, ForgeError> {
        let s = self.get(pr)?;
        Ok(PrStatus {
            repo: pr.repo.clone(),
            branch: s.branch,
            sha: s.sha,
            checks: s.checks,
            mergeable: s.mergeable,
        })
    }
    fn merge(&self, pr: &PrRef) -> Result<ForgeEffect, ForgeError> {
        let s = self.get(pr)?;
        self.merges.lock().unwrap().push(pr.number);
        Ok(ForgeEffect {
            repo: pr.repo.clone(),
            branch: s.branch,
            sha: s.sha,
            outcome: s.merge,
        })
    }
    fn merge_landed(&self, pr: &PrRef) -> Result<ForgeEffect, ForgeError> {
        let s = self.get(pr)?;
        Ok(ForgeEffect {
            repo: pr.repo.clone(),
            branch: s.branch,
            sha: s.sha,
            outcome: if s.landed {
                ForgeOutcome::Landed
            } else {
                ForgeOutcome::NotLanded
            },
        })
    }
}

// ---------------------------------------------------------------- unit source

pub struct FakeUnitSource {
    pub records: Vec<UnitRecord>,
}

impl UnitSource for FakeUnitSource {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        Ok(self.records.clone())
    }
}

pub fn unit(id: &str, group: &str, order: u32) -> UnitRecord {
    let mut vars = BTreeMap::new();
    vars.insert("brief".to_string(), "do the work".to_string());
    UnitRecord::new(id, group, order, vars)
}

// ---------------------------------------------------------------- engine

pub fn workflow(stages: &[&str], merge: bool) -> Workflow {
    Workflow::single_track(
        "test-flow",
        stages
            .iter()
            .map(|s| StageDef::new(s, &format!("{s}-role"), "model-a"))
            .collect(),
        merge.then_some(MergeRule::GreenChecksInDeclaredOrder),
        Settings::default(),
    )
}

/// Like [`workflow`], with the one track named `item`.
pub fn item_workflow(stages: &[&str], merge: bool) -> Workflow {
    let mut wf = workflow(stages, merge);
    wf.tracks[0].name = "item".to_string();
    wf
}

/// A workflow with two tracks. `lane` has the stage `fill` and no merge rule. `item` has the
/// stages `build` and `check` and merges by rule.
pub fn two_track_workflow() -> Workflow {
    let stage = |s: &str| StageDef::new(s, &format!("{s}-role"), "model-a");
    Workflow {
        name: "two-tracks".to_string(),
        tracks: vec![
            Track {
                name: "lane".to_string(),
                stages: vec![stage("fill")],
                merge: None,
            },
            Track {
                name: "item".to_string(),
                stages: vec![stage("build"), stage("check")],
                merge: Some(MergeDef::new(MergeRule::GreenChecksInDeclaredOrder)),
            },
        ],
        settings: Settings::default(),
        roots: Vec::new(),
    }
}

/// A unit source that names its track.
pub struct TrackedSource {
    pub track: String,
    pub records: Vec<UnitRecord>,
}

impl UnitSource for TrackedSource {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        Ok(self.records.clone())
    }
    fn track(&self) -> Option<&str> {
        Some(&self.track)
    }
}

/// A clock that counts up, so every timestamp differs and is stable.
pub fn counting_clock() -> Clock {
    let n = Arc::new(AtomicU64::new(0));
    Arc::new(move || {
        let i = n.fetch_add(1, Ordering::SeqCst);
        format!("2026-01-01T00:00:{:02}.{:03}Z", (i / 1000) % 60, i % 1000)
    })
}

pub struct Env {
    pub engine: Engine,
    pub store: Arc<FakeStore>,
    pub agent: Arc<FakeAgent>,
    pub forge: Arc<FakeForge>,
    pub dir: tempfile::TempDir,
}

impl Env {
    pub fn ledger_path(&self) -> PathBuf {
        self.dir.path().join("ledger.md")
    }

    pub fn ledger_text(&self) -> String {
        std::fs::read_to_string(self.ledger_path()).unwrap_or_default()
    }

    pub fn status_of(&self, id: &str) -> (pf_engine::Status, Option<String>) {
        let n = self.store.node(self.engine.run_id(), id).unwrap();
        (n.status, n.reason)
    }
}

pub fn config(dir: &std::path::Path, wf: Workflow) -> EngineConfig {
    config_with(dir, wf, BTreeMap::new())
}

pub fn config_with(
    dir: &std::path::Path,
    wf: Workflow,
    params: BTreeMap<String, String>,
) -> EngineConfig {
    EngineConfig {
        run_id: "run-1".to_string(),
        workflow: wf,
        ledger_path: dir.join("ledger.md"),
        state_dir: dir.join("state"),
        run_dir: dir.join("run"),
        params,
    }
}

/// A started engine on fake ports.
pub fn env(wf: Workflow) -> Env {
    env_with_params(wf, BTreeMap::new())
}

/// A started engine on fake ports, with run parameters.
pub fn env_with_params(wf: Workflow, params: BTreeMap<String, String>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    let agent = FakeAgent::new();
    let forge = FakeForge::new();
    let ledger: Box<dyn LedgerFile> = Box::new(FsLedger::new(dir.path().join("ledger.md")));
    let engine = Engine::new(
        config_with(dir.path(), wf, params),
        store.clone(),
        agent.clone(),
        forge.clone(),
        counting_clock(),
        ledger,
    )
    .unwrap();
    engine.start_up().unwrap();
    Env {
        engine,
        store,
        agent,
        forge,
        dir,
    }
}
