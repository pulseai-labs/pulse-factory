//! The driver: one tick of one run (Approach 4).
//!
//! `Driver::tick` is a plain function over the run's engine, ports, clock and paths. A tick goes
//! through ten steps in a fixed order. One unit's error is a line in the tick's notes and the
//! tick goes on to the next unit. Only a store failure stops the engine, on its own degraded path
//! (ADR-0006).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pf_engine::admission::{EngineError, PushOutcome, RunView};
use pf_engine::capacity::Slot;
use pf_engine::launch::LaunchStep;
use pf_engine::merge::MergeStep;
use pf_engine::port::agent::{
    AgentPort, SessionEvent, SessionRef, SessionState as AdapterState, SessionStatus, Usage,
};
use pf_engine::port::store::{NodeRow, RunState, SessionRow};
use pf_engine::render::{RenderContext, UnitView, render};
use pf_engine::status::Status;
use pf_engine::unit::{StallDecision, UnitDetail, stall_decision};
use pf_engine::{EndStatus, Engine};
use pf_workflow::commands::{HookOutcome, run_after_merge, run_unit_hook};
use pf_workflow::file::{SeedDef, StageSpec};
use pf_workflow::package::Package;
use pf_workflow::pin::{self, Pin};
use pf_workflow::queue::{QueuePoll, QueueSource, ingest_queue};
use pf_workflow::seeds::{SeedAction, advance_seed};
use pf_workflow::signals::{
    ApplyResult, SessionState as SignalState, StageVerdict, SystemProbe, apply, evaluate,
};

/// Seconds on the controller's clock. Tests inject their own.
pub type ClockSecs = Arc<dyn Fn() -> u64 + Send + Sync>;

pub fn system_secs() -> ClockSecs {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    })
}

/// The most entries a walk for the stall rule reads. A bigger tree counts as changed now.
pub const WALK_LIMIT: usize = 100_000;

/// A decisions file over this size parks a running unit.
pub const DECISIONS_MAX_BYTES: u64 = 1 << 20;

/// The reason of a stall that left no screen text.
pub const NO_SCREEN_TEXT: &str = "no screen text";

/// What a driver runs on.
pub struct DriverConfig {
    pub engine: Arc<Engine>,
    pub agent: Arc<dyn AgentPort>,
    pub package: Package,
    pub pins: Vec<Pin>,
    pub params: BTreeMap<String, String>,
    pub run_dir: PathBuf,
    pub target_dir: String,
    pub clock: ClockSecs,
    /// The most entries one walk reads for the stall rule. Use [`WALK_LIMIT`].
    pub walk_limit: usize,
}

/// What the driver saw of one session, for the stall rule.
#[derive(Clone, Copy)]
struct TurnTrack {
    first_turn_started: bool,
    turn_ended: bool,
    changed_at: u64,
}

#[derive(Default)]
struct DState {
    queues: Vec<QueueSource>,
    /// Slots held for a session until its row is ended.
    held: BTreeMap<String, Vec<Slot>>,
    /// Sessions whose `after_first_turn` hook ran.
    first_turn_done: BTreeSet<String>,
    turns: BTreeMap<String, TurnTrack>,
    stall_tail: BTreeMap<String, String>,
}

pub struct Driver {
    cfg: DriverConfig,
    state: Mutex<DState>,
    notes: Mutex<Vec<String>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What one tick collects.
struct Tick {
    notes: Vec<String>,
    now: u64,
}

impl Tick {
    fn note(&mut self, text: impl Into<String>) {
        self.notes
            .push(pf_engine::status::one_line_reason(&text.into()));
    }

    /// Absorb an engine error as a note, except a store failure, which has its own path.
    fn absorb<T>(&mut self, what: &str, r: Result<T, EngineError>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.note(format!("{what}: {e}"));
                None
            }
        }
    }
}

impl Driver {
    pub fn new(cfg: DriverConfig) -> Driver {
        Driver {
            cfg,
            state: Mutex::new(DState::default()),
            notes: Mutex::new(Vec::new()),
        }
    }

    /// The notes of the last tick.
    pub fn notes(&self) -> Vec<String> {
        locked(&self.notes).clone()
    }

    fn run_dir_text(&self) -> String {
        self.cfg.run_dir.display().to_string()
    }

    fn ctx<'a>(
        &'a self,
        run_dir: &'a str,
        unit: Option<(&'a str, &'a BTreeMap<String, String>)>,
    ) -> RenderContext<'a> {
        RenderContext {
            run_id: self.cfg.engine.run_id(),
            run_dir,
            params: &self.cfg.params,
            unit: unit.map(|(id, vars)| UnitView { id, vars }),
        }
    }

    fn stage_spec(&self, track: &str, stage: &str) -> Option<&StageSpec> {
        self.cfg
            .package
            .file
            .track(track)?
            .stages
            .iter()
            .find(|s| s.name == stage)
    }

    // ------------------------------------------------------------------ the tick

    pub fn tick(&self) {
        let mut state = locked(&self.state);
        let mut t = Tick {
            notes: Vec::new(),
            now: (self.cfg.clock)(),
        };
        self.tick_inner(&mut state, &mut t);
        *locked(&self.notes) = t.notes;
    }

    fn tick_inner(&self, st: &mut DState, t: &mut Tick) {
        let engine = &self.cfg.engine;
        match engine.view() {
            Ok(v) if v.run.as_ref().is_some_and(|r| r.state == RunState::Ended) => return,
            Err(e) => {
                t.note(format!("cannot read the run: {e}"));
                return;
            }
            Ok(_) => {}
        }
        let run_dir = self.run_dir_text();

        self.step1_queue(st, t, &run_dir);
        self.step2_seeds(t, &run_dir);
        let statuses = self.step3_sessions(st, t);
        self.step4_first_turn(st, t, &run_dir, &statuses);
        self.step5_stall(st, t, &run_dir, &statuses);
        self.step6_signals(st, t, &run_dir, &statuses);
        self.step7_decisions(t, &run_dir);
        self.release_slots(st, t);
        self.step8_merges(t, &run_dir);
        self.step9_launches(st, t, &run_dir);
        self.step10_finish(t, &run_dir);
    }

    fn view(&self, t: &mut Tick) -> Option<RunView> {
        let r = self.cfg.engine.view();
        t.absorb("cannot read the run", r)
    }

    // 1. The queue.
    fn step1_queue(&self, st: &mut DState, t: &mut Tick, run_dir: &str) {
        if st.queues.is_empty() {
            for def in &self.cfg.package.file.unit_sources {
                let ctx = self.ctx(run_dir, None);
                match render(&def.dir, &ctx) {
                    Ok(dir) => st
                        .queues
                        .push(QueueSource::new(dir.text, def.track.clone())),
                    Err(e) => t.note(format!("queue directory cannot render: {e}")),
                }
            }
        }
        for source in &mut st.queues {
            match ingest_queue(&self.cfg.engine, source) {
                Ok(QueuePoll::Ingested(_)) => {}
                Ok(QueuePoll::Halted(reason)) => t.note(format!("queue halted the run: {reason}")),
                Err(e) => t.note(format!("queue: {e}")),
            }
            for s in source.skipped() {
                t.note(format!("queue skipped {}: {}", s.file, s.why));
            }
        }
    }

    // 2. The seeds.
    fn step2_seeds(&self, t: &mut Tick, run_dir: &str) {
        for seed in &self.cfg.package.file.seeds {
            let exists = match self.until_file_exists(seed, run_dir) {
                Ok(e) => e,
                Err(why) => {
                    t.note(why);
                    continue;
                }
            };
            match advance_seed(&self.cfg.engine, seed, exists) {
                Ok(SeedAction::Halted(reason)) => {
                    t.note(format!("seed {} halted the run: {reason}", seed.id))
                }
                Ok(_) => {}
                Err(e) => t.note(format!("seed {}: {e}", seed.id)),
            }
        }
    }

    fn until_file_exists(&self, seed: &SeedDef, run_dir: &str) -> Result<bool, String> {
        let Some(repeat) = &seed.repeat else {
            return Ok(false);
        };
        let path = render(&repeat.until_file, &self.ctx(run_dir, None))
            .map_err(|e| format!("seed {}: until_file cannot render: {e}", seed.id))?;
        Ok(Path::new(&path.text).exists())
    }

    // 3. Read each running session. End a session that is open under a unit that is not running.
    fn step3_sessions(&self, st: &mut DState, t: &mut Tick) -> BTreeMap<String, SessionStatus> {
        let mut statuses = BTreeMap::new();
        let Some(view) = self.view(t) else {
            return statuses;
        };
        for session in view.sessions.iter().filter(|s| !s.ended) {
            let Some(process_ref) = &session.process_ref else {
                continue;
            };
            let r = SessionRef(process_ref.clone());
            let unit_running = view
                .node(&session.unit_id)
                .is_some_and(|u| u.status == Status::Running);
            if !unit_running {
                let why = view
                    .node(&session.unit_id)
                    .map_or("unknown".to_string(), |u| u.status.as_str().to_string());
                self.end_session(
                    st,
                    t,
                    session,
                    &r,
                    &format!("seat ended: its unit is {why}"),
                    false,
                );
                continue;
            }
            self.read(st, t, session, &r, true);
            if let Ok(status) = self.cfg.agent.status(&r) {
                self.track_turns(st, t, &session.id, &status);
                statuses.insert(session.id.clone(), status);
            }
        }
        statuses
    }

    fn track_turns(&self, st: &mut DState, t: &Tick, id: &str, status: &SessionStatus) {
        let entry = st.turns.entry(id.to_string()).or_insert(TurnTrack {
            first_turn_started: false,
            turn_ended: false,
            changed_at: t.now,
        });
        if entry.first_turn_started != status.first_turn_started
            || entry.turn_ended != status.turn_ended
        {
            entry.first_turn_started = status.first_turn_started;
            entry.turn_ended = status.turn_ended;
            entry.changed_at = t.now;
        }
    }

    /// A read is a drain, applied in order, and a usage reading.
    fn read(
        &self,
        st: &mut DState,
        t: &mut Tick,
        session: &SessionRow,
        r: &SessionRef,
        unit_running: bool,
    ) {
        let engine = &self.cfg.engine;
        match self.cfg.agent.drain_events(r) {
            Ok(events) => {
                for event in events {
                    match event {
                        SessionEvent::Activity => {}
                        SessionEvent::Stalled { screen_tail } => {
                            st.stall_tail.insert(session.id.clone(), screen_tail);
                        }
                        SessionEvent::Push {
                            id,
                            repo,
                            branch,
                            sha,
                            outcome,
                        } => {
                            let outcome: PushOutcome = outcome;
                            let res = engine.record_push(
                                &session.unit_id,
                                &id,
                                &repo,
                                &branch,
                                &sha,
                                outcome,
                            );
                            t.absorb("push", res);
                        }
                        SessionEvent::DefaultBranchMoved {
                            remote,
                            branch,
                            before,
                            after,
                        } => {
                            if unit_running {
                                let reason = format!(
                                    "the default branch {remote}/{branch} moved from {before} to {after} during a push"
                                );
                                self.park(t, &session.unit_id, &reason);
                            }
                        }
                    }
                }
            }
            Err(e) => t.note(format!("session {}: cannot drain events: {e}", session.id)),
        }
        match self.cfg.agent.usage(r) {
            Ok(u) => self.record_usage(t, session, u),
            Err(e) => t.note(format!("session {}: cannot read usage: {e}", session.id)),
        }
    }

    fn record_usage(&self, t: &mut Tick, session: &SessionRow, u: Usage) {
        // The store holds what was recorded. Record the positive difference.
        let row = self
            .cfg
            .engine
            .view()
            .ok()
            .and_then(|v| v.session(&session.id).cloned())
            .unwrap_or_else(|| session.clone());
        let diff = Usage {
            input_tokens: u.input_tokens.saturating_sub(row.input_tokens),
            output_tokens: u.output_tokens.saturating_sub(row.output_tokens),
            cache_read_tokens: u.cache_read_tokens.saturating_sub(row.cache_read_tokens),
            cache_write_tokens: u.cache_write_tokens.saturating_sub(row.cache_write_tokens),
            cost_micros: u.cost_micros.saturating_sub(row.cost_micros),
        };
        if diff == Usage::default() {
            return;
        }
        let reading_id = u
            .input_tokens
            .saturating_add(u.output_tokens)
            .saturating_add(u.cache_read_tokens)
            .saturating_add(u.cache_write_tokens)
            .saturating_add(u.cost_micros)
            .to_string();
        let res = self.cfg.engine.record_usage(&session.id, diff, &reading_id);
        t.absorb("usage", res);
    }

    fn park(&self, t: &mut Tick, unit_id: &str, reason: &str) -> bool {
        match self.cfg.engine.park(unit_id, reason) {
            Ok(_) => true,
            Err(e) => {
                t.note(format!("park of {unit_id} not recorded: {e}"));
                false
            }
        }
    }

    /// Stop the seat, read what it wrote, and end its session.
    fn end_session(
        &self,
        st: &mut DState,
        t: &mut Tick,
        session: &SessionRow,
        r: &SessionRef,
        reason: &str,
        unit_running: bool,
    ) {
        if let Err(e) = self.cfg.agent.stop(r) {
            t.note(format!("session {}: cannot stop: {e}", session.id));
        }
        self.read(st, t, session, r, unit_running);
        let res = self.cfg.engine.end(&session.id, EndStatus::Failed, reason);
        t.absorb("end of session", res);
    }

    /// Park a unit whose stage is running: park, stop, read, end the session.
    fn park_running(
        &self,
        st: &mut DState,
        t: &mut Tick,
        session: &SessionRow,
        r: &SessionRef,
        reason: &str,
    ) {
        let running = self
            .cfg
            .engine
            .view()
            .ok()
            .and_then(|v| {
                v.node(&session.unit_id)
                    .map(|n| n.status == Status::Running)
            })
            .unwrap_or(false);
        if !running {
            return;
        }
        self.park(t, &session.unit_id, reason);
        self.end_session(st, t, session, r, reason, false);
    }

    // 4. `after_first_turn`, once for each session.
    fn step4_first_turn(
        &self,
        st: &mut DState,
        t: &mut Tick,
        run_dir: &str,
        statuses: &BTreeMap<String, SessionStatus>,
    ) {
        let Some(view) = self.view(t) else { return };
        for session in view.sessions.iter().filter(|s| !s.ended) {
            let Some(status) = statuses.get(&session.id) else {
                continue;
            };
            if !status.first_turn_started || st.first_turn_done.contains(&session.id) {
                continue;
            }
            let Some(unit) = view
                .node(&session.unit_id)
                .filter(|u| u.status == Status::Running)
            else {
                continue;
            };
            let Some((detail, stage)) = self.unit_stage(&view, unit, &session.node_id) else {
                continue;
            };
            // Mark first: a hook that fails must not run again.
            st.first_turn_done.insert(session.id.clone());
            if stage.after_first_turn.is_empty() {
                continue;
            }
            let ctx = self.ctx(run_dir, Some((&unit.id, &detail.record.vars)));
            match run_unit_hook(
                &self.cfg.engine,
                &unit.id,
                "after_first_turn",
                &stage.after_first_turn,
                &ctx,
            ) {
                Ok(HookOutcome::Passed) => {}
                Ok(HookOutcome::Failed(why)) => t.note(format!("unit {}: {why}", unit.id)),
                Err(e) => t.note(format!("unit {}: after_first_turn: {e}", unit.id)),
            }
        }
    }

    /// The unit's detail and the stage spec of a stage node.
    fn unit_stage(
        &self,
        view: &RunView,
        unit: &NodeRow,
        stage_node: &str,
    ) -> Option<(UnitDetail, &StageSpec)> {
        let detail = view.unit_detail(unit).ok()?;
        let stage_name = view.node(stage_node)?.detail.clone();
        let spec = self.stage_spec(&detail.track, &stage_name)?;
        Some((detail, spec))
    }

    // 5. The stall rule.
    fn step5_stall(
        &self,
        st: &mut DState,
        t: &mut Tick,
        run_dir: &str,
        statuses: &BTreeMap<String, SessionStatus>,
    ) {
        let Some(view) = self.view(t) else { return };
        for session in view.sessions.iter().filter(|s| !s.ended) {
            let Some(process_ref) = &session.process_ref else {
                continue;
            };
            if !statuses.contains_key(&session.id) {
                continue;
            }
            let Some(unit) = view
                .node(&session.unit_id)
                .filter(|u| u.status == Status::Running)
            else {
                continue;
            };
            let Some((detail, stage)) = self.unit_stage(&view, unit, &session.node_id) else {
                continue;
            };
            let ctx = self.ctx(run_dir, Some((&unit.id, &detail.record.vars)));
            let setting = |key: &str| -> String {
                stage
                    .settings
                    .get(key)
                    .and_then(|tmpl| render(tmpl, &ctx).ok())
                    .map(|r| r.text)
                    .unwrap_or_default()
            };
            let worktree = setting("worktree");
            let report_dir = setting("report_dir");
            let out_dir = self.cfg.run_dir.join("out").join(&unit.id);
            let last_turn = st.turns.get(&session.id).map(|x| x.changed_at);
            let last_file = self.last_file_change(t.now, &worktree, &out_dir, &report_dir);
            let last_commit = if worktree.is_empty() {
                None
            } else {
                last_commit(&worktree)
            };
            let stage_name = &stage.name;
            let prior = view
                .stages_of(&unit.id)
                .iter()
                .filter(|n| &n.detail == stage_name && n.status == Status::Failed)
                .filter(|n| {
                    n.reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with("stalled:"))
                })
                .count() as u32;
            let limit = u64::from(stage.stall_minutes) * 60;
            let decision =
                stall_decision(last_turn, last_file, last_commit, None, prior, limit, t.now);
            if decision == StallDecision::Active {
                continue;
            }
            let tail = st
                .stall_tail
                .get(&session.id)
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| NO_SCREEN_TEXT.to_string());
            let reason = pf_engine::status::one_line_reason(&format!("stalled: {tail}"));
            let r = SessionRef(process_ref.clone());
            match decision {
                StallDecision::Relaunch => self.end_session(st, t, session, &r, &reason, true),
                StallDecision::Park => self.park_running(st, t, session, &r, &reason),
                StallDecision::Active => {}
            }
        }
    }

    fn last_file_change(
        &self,
        now: u64,
        worktree: &str,
        out_dir: &Path,
        report_dir: &str,
    ) -> Option<u64> {
        let mut newest: Option<u64> = None;
        let mut seen = 0usize;
        for root in [Path::new(worktree), out_dir, Path::new(report_dir)] {
            if root.as_os_str().is_empty() {
                continue;
            }
            match newest_file_mtime(root, &mut seen, self.cfg.walk_limit) {
                Walk::TooBig => return Some(now),
                Walk::Newest(m) => newest = newest.max(m),
            }
        }
        if !self.cfg.target_dir.is_empty() {
            let deps = Path::new(&self.cfg.target_dir).join("debug/deps");
            if let Some(m) = mtime_secs(&deps) {
                newest = newest.max(Some(m));
            }
        }
        newest
    }

    // 6. The stage's signals.
    fn step6_signals(
        &self,
        st: &mut DState,
        t: &mut Tick,
        run_dir: &str,
        statuses: &BTreeMap<String, SessionStatus>,
    ) {
        let Some(view) = self.view(t) else { return };
        for session in view.sessions.iter().filter(|s| !s.ended) {
            let Some(process_ref) = &session.process_ref else {
                continue;
            };
            let Some(status) = statuses.get(&session.id) else {
                continue;
            };
            let Some(unit) = view
                .node(&session.unit_id)
                .filter(|u| u.status == Status::Running)
            else {
                continue;
            };
            // Only a session whose stage node still runs.
            if view
                .node(&session.node_id)
                .is_none_or(|n| n.status != Status::Running)
            {
                continue;
            }
            let Some((detail, stage)) = self.unit_stage(&view, unit, &session.node_id) else {
                continue;
            };
            let r = SessionRef(process_ref.clone());
            let ctx = self.ctx(run_dir, Some((&unit.id, &detail.record.vars)));
            let ended =
                status.state == AdapterState::Ended || !self.cfg.agent.is_alive(&r).unwrap_or(true);
            let verdict = evaluate(
                stage,
                &ctx,
                &SystemProbe,
                &SignalState {
                    turn_ended: status.turn_ended,
                    ended,
                },
            );
            match &verdict {
                StageVerdict::Wait => {}
                StageVerdict::Park { reason } => self.park_running(st, t, session, &r, reason),
                StageVerdict::Done { .. } => {
                    // A done signal seen mid-turn can come before the push it belongs to.
                    if status.turn_ended || ended {
                        if let Err(e) = self.cfg.agent.stop(&r) {
                            t.note(format!("session {}: cannot stop: {e}", session.id));
                        }
                        self.read(st, t, session, &r, true);
                        match apply(&self.cfg.engine, &unit.id, &stage.name, &verdict) {
                            Ok(ApplyResult::Parked(why)) => {
                                t.note(format!("unit {} parked: {why}", unit.id))
                            }
                            Ok(_) => {}
                            Err(e) => t.note(format!("signal of {}: {e}", unit.id)),
                        }
                    }
                }
            }
        }
    }

    // 7. A seat's decisions.
    fn step7_decisions(&self, t: &mut Tick, run_dir: &str) {
        let Some(view) = self.view(t) else { return };
        for unit in view.units() {
            let Ok(detail) = view.unit_detail(unit) else {
                continue;
            };
            let Some(track) = self.cfg.package.file.track(&detail.track) else {
                continue;
            };
            let mut paths: BTreeSet<String> = BTreeSet::new();
            let ctx = self.ctx(run_dir, Some((&unit.id, &detail.record.vars)));
            for stage in &track.stages {
                if let Some(tmpl) = stage.settings.get("decisions_file")
                    && let Ok(p) = render(tmpl, &ctx)
                    && !p.text.is_empty()
                {
                    paths.insert(p.text);
                }
            }
            for path in paths {
                self.read_decisions(t, unit, &path);
            }
        }
    }

    fn read_decisions(&self, t: &mut Tick, unit: &NodeRow, path: &str) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        if meta.len() > DECISIONS_MAX_BYTES {
            let reason = format!("the decisions file {path} is over 1 MiB");
            if unit.status == Status::Running {
                self.park(t, &unit.id, &reason);
            } else {
                t.note(format!("unit {}: {reason}", unit.id));
            }
            return;
        }
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        // Whole lines only: a partial last line waits for its newline.
        let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        for (i, raw) in bytes[..end].split(|b| *b == b'\n').enumerate() {
            let line = String::from_utf8_lossy(raw);
            if line.trim().is_empty() {
                continue;
            }
            let res = self
                .cfg
                .engine
                .record_decision(&unit.id, i as u64 + 1, &line);
            t.absorb(&format!("decision line {} of {}", i + 1, unit.id), res);
        }
    }

    /// Free the slots of a session whose row is ended.
    fn release_slots(&self, st: &mut DState, t: &mut Tick) {
        let Some(view) = self.view(t) else { return };
        st.held
            .retain(|id, _| view.session(id).is_some_and(|s| !s.ended));
    }

    // 8. Merges, in the declared order.
    fn step8_merges(&self, t: &mut Tick, run_dir: &str) {
        let engine = &self.cfg.engine;
        let limit = engine.view().map_or(8, |v| v.units().count() + 2);
        for _ in 0..limit {
            if engine.view().is_ok_and(|v| v.is_halted()) {
                return;
            }
            match engine.merge_next() {
                Ok(MergeStep::Merged { unit, .. }) => self.after_merge(t, run_dir, &unit),
                Ok(MergeStep::Parked { unit, reason }) => {
                    t.note(format!("unit {unit} parked at merge: {reason}"))
                }
                Ok(_) => return,
                Err(e) => {
                    t.note(format!("merge: {e}"));
                    return;
                }
            }
        }
    }

    fn after_merge(&self, t: &mut Tick, run_dir: &str, unit_id: &str) {
        let Some(view) = self.view(t) else { return };
        let Some(unit) = view.node(unit_id) else {
            return;
        };
        let Ok(detail) = view.unit_detail(unit) else {
            return;
        };
        let Some(merge) = self
            .cfg
            .package
            .file
            .track(&detail.track)
            .and_then(|tr| tr.merge.as_ref())
        else {
            return;
        };
        let ctx = self.ctx(run_dir, Some((unit_id, &detail.record.vars)));
        match run_after_merge(&self.cfg.engine, unit_id, &merge.after_merge, &ctx) {
            Ok(HookOutcome::Passed) => {}
            Ok(HookOutcome::Failed(why)) => t.note(format!("after_merge: {why}")),
            Err(e) => t.note(format!("after_merge of {unit_id}: {e}")),
        }
    }

    // 9. Launches.
    fn step9_launches(&self, st: &mut DState, t: &mut Tick, run_dir: &str) {
        let engine = &self.cfg.engine;
        let Some(view) = self.view(t) else { return };
        if view.is_halted() {
            return;
        }
        for unit in view.units().filter(|u| u.status == Status::Running) {
            let Ok(detail) = view.unit_detail(unit) else {
                continue;
            };
            let Some(track) = self.cfg.package.file.track(&detail.track) else {
                continue;
            };
            let Some(stage) = track.stages.get(detail.stages_done as usize) else {
                continue;
            };
            if view.running_stage_of(&unit.id).is_some() {
                continue;
            }
            // Slots: a model session always, a cold build when the stage lists it.
            let mut slots = Vec::new();
            match engine.capacity().try_acquire_model_session() {
                Some(s) => slots.push(s),
                None => continue,
            }
            if stage.slots.iter().any(|s| s == "build") {
                match engine.capacity().try_acquire_cold_build() {
                    Some(s) => slots.push(s),
                    None => continue,
                }
            }
            // The run's copies must be as they were at arm.
            if let Some(mismatch) = self.cfg.pins.iter().find_map(|p| pin::check(p).err()) {
                self.park(t, &unit.id, &mismatch.to_string());
                continue;
            }
            let ctx = self.ctx(run_dir, Some((&unit.id, &detail.record.vars)));
            match run_unit_hook(engine, &unit.id, "before", &stage.before, &ctx) {
                Ok(HookOutcome::Passed) => {}
                Ok(HookOutcome::Failed(why)) => {
                    t.note(format!("unit {}: {why}", unit.id));
                    continue;
                }
                Err(e) => {
                    t.note(format!("unit {}: before: {e}", unit.id));
                    continue;
                }
            }
            match engine.start_stage(&unit.id) {
                Ok(LaunchStep::Launched { session_id, .. }) => {
                    st.held.insert(session_id.clone(), slots);
                    st.turns.insert(
                        session_id,
                        TurnTrack {
                            first_turn_started: false,
                            turn_ended: false,
                            changed_at: t.now,
                        },
                    );
                }
                Ok(LaunchStep::Parked { reason }) => {
                    t.note(format!("unit {} parked at launch: {reason}", unit.id))
                }
                Ok(LaunchStep::Refused { reason }) => {
                    t.note(format!("launch of {} refused: {reason}", unit.id))
                }
                Ok(_) => {}
                Err(e) => t.note(format!("launch of {}: {e}", unit.id)),
            }
        }
    }

    // 10. The end of the run.
    fn step10_finish(&self, t: &mut Tick, run_dir: &str) {
        let Some(view) = self.view(t) else { return };
        let seeds_done = self
            .cfg
            .package
            .file
            .seeds
            .iter()
            .all(|s| s.repeat.is_none() || self.until_file_exists(s, run_dir).unwrap_or(false));
        let units: Vec<&NodeRow> = view.units().collect();
        if seeds_done && !units.is_empty() && units.iter().all(|u| u.status == Status::Done) {
            let res = self.cfg.engine.end_run("every item merged");
            t.absorb("end of run", res);
        }
    }

    /// End each session still open under a unit that is not running (start-up, and a replaced run).
    pub fn sweep(&self) {
        let mut state = locked(&self.state);
        let mut t = Tick {
            notes: Vec::new(),
            now: (self.cfg.clock)(),
        };
        self.step3_sessions(&mut state, &mut t);
        *locked(&self.notes) = t.notes;
    }
}

// ------------------------------------------------------------------ files

fn mtime_secs(p: &Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

enum Walk {
    Newest(Option<u64>),
    TooBig,
}

/// The newest modified time among the files under `root`, leaving out `.git` and any `target`.
/// `seen` counts entries over all roots of one call; more than [`WALK_LIMIT`] is too big.
fn newest_file_mtime(root: &Path, seen: &mut usize, limit: usize) -> Walk {
    let mut newest: Option<u64> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            *seen += 1;
            if *seen > limit {
                return Walk::TooBig;
            }
            let name = entry.file_name();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if name == ".git" || name == "target" {
                    continue;
                }
                stack.push(entry.path());
            } else if kind.is_file()
                && let Some(m) = mtime_secs(&entry.path())
            {
                newest = newest.max(Some(m));
            }
        }
    }
    Walk::Newest(newest)
}

fn last_commit(worktree: &str) -> Option<u64> {
    let out = std::process::Command::new("git")
        .args(["-C", worktree, "log", "-1", "--format=%ct"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}
