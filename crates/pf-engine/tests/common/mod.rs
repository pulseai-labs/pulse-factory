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

#[derive(Clone, Default)]
struct StoreState {
    runs: BTreeMap<String, RunRow>,
    nodes: Vec<NodeRow>,
    sessions: Vec<SessionRow>,
    admissions: Vec<AdmissionRow>,
    outbox: Vec<LedgerLine>,
    requests: Vec<RequestRow>,
    claims: Vec<ClaimRow>,
    /// The projects a test added with [`FakeStore::add_project`]: the ID, and whether it is removed.
    projects: BTreeMap<String, bool>,
    clear_grants: Vec<ClearGrantRow>,
}

/// An in-memory store that keeps the port's promises: one unique command ID per run, content
/// compared byte for byte, and a commit that applies all of its writes or none.
#[derive(Default)]
pub struct FakeStore {
    state: Mutex<StoreState>,
    pub fail_writes: AtomicBool,
    /// Fail a commit at its `OpenRequest` effect, after the effects before it were applied. The
    /// commit then restores the state it found, as a database rolls back.
    pub fail_on_open_request: AtomicBool,
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

    pub fn all_requests(&self) -> Vec<RequestRow> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn all_claims(&self) -> Vec<ClaimRow> {
        self.state.lock().unwrap().claims.clone()
    }

    /// Make a live project known to the fake, as the real store knows one. The engine tests carry no
    /// other controller entity, but the clear grant's rules read the project.
    pub fn add_project(&self, id: &str) {
        self.state
            .lock()
            .unwrap()
            .projects
            .insert(id.to_string(), false);
    }

    /// Mark a project the fake knows as removed.
    pub fn remove_project(&self, id: &str) {
        self.state
            .lock()
            .unwrap()
            .projects
            .insert(id.to_string(), true);
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
        let snapshot = s.clone();
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
                Effect::PutRun(mut r) => {
                    // `PutRun` never writes the launch count, as the real store's does not.
                    if let Some(held) = s.runs.get(&r.id) {
                        r.launch_bound = held.launch_bound;
                    }
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
                Effect::OpenRequest(n) => {
                    let taken = n.unit_id.is_some()
                        && s.requests.iter().any(|r| {
                            r.run_id == n.run_id
                                && r.unit_id == n.unit_id
                                && r.status == RequestStatus::Open
                        });
                    if taken || self.fail_on_open_request.load(Ordering::SeqCst) {
                        *s = snapshot;
                        return Err(StoreError("request write failed".to_string()));
                    }
                    let next = s
                        .requests
                        .iter()
                        .filter_map(|r| r.id.strip_prefix("R-")?.parse::<u64>().ok())
                        .max()
                        .unwrap_or(0)
                        + 1;
                    s.requests.push(RequestRow {
                        id: format!("R-{next}"),
                        revision: 1,
                        run_id: n.run_id,
                        unit_id: n.unit_id,
                        stage: n.stage,
                        kind: n.kind,
                        reason: n.reason,
                        body: n.body,
                        status: RequestStatus::Open,
                        opened_at: n.opened_at,
                        answered_at: None,
                        answer: None,
                        answer_generation: None,
                    });
                }
                Effect::PutRequest(r) => match s.requests.iter_mut().find(|x| x.id == r.id) {
                    Some(slot) => *slot = r,
                    None => {
                        *s = snapshot;
                        return Err(StoreError("no such request".to_string()));
                    }
                },
                Effect::PutClaim(c) => {
                    match s.claims.iter_mut().find(|x| x.generation == c.generation) {
                        Some(slot) => *slot = c,
                        None => s.claims.push(c),
                    }
                }
                // The engine tests carry no controller entities: the engine makes no such effect.
                Effect::AddProject(_)
                | Effect::RemoveProject { .. }
                | Effect::AddDevice(_)
                | Effect::RevokeDevice { .. }
                | Effect::SetProjectKind { .. }
                | Effect::SetProjectRemoteKey { .. }
                | Effect::StartChatSession(_)
                | Effect::EndChatSession { .. }
                | Effect::SetChatSessionRuntime { .. }
                | Effect::RedactChatItem { .. }
                | Effect::SetSetting(_)
                | Effect::UnsetSetting { .. }
                | Effect::LaunchRecordWrite(_)
                | Effect::HostStarted(_)
                | Effect::HostEnded { .. }
                | Effect::HostPositionWrite { .. }
                | Effect::NoticeAdd(_)
                | Effect::NoticeSupersede { .. }
                | Effect::NoticeSend { .. }
                | Effect::NoticeAck { .. } => {}
                Effect::GrantClear(_) | Effect::RevokeClearGrant { .. }
                    if commit.run_id != CONTROLLER_SCOPE =>
                {
                    *s = snapshot;
                    return Ok(Committed::Rejected(
                        "a clear grant and its revoke are written only in an admission under @controller"
                            .to_string(),
                    ));
                }
                // The same rules as the real store: one live row of a project, generations 1 to n,
                // a revoke set once, and a precondition that writes nothing.
                Effect::GrantClear(n) => {
                    let why = match s.projects.get(&n.project_id) {
                        None => Some(format!("no project {}", n.project_id)),
                        Some(true) => Some(format!("project {} is removed", n.project_id)),
                        Some(false) => {
                            let mine: Vec<&ClearGrantRow> = s
                                .clear_grants
                                .iter()
                                .filter(|g| g.project_id == n.project_id)
                                .collect();
                            let next = mine.iter().map(|g| g.generation).max().unwrap_or(0) + 1;
                            match mine.iter().find(|g| g.revoked_at.is_none()) {
                                Some(open) => Some(format!(
                                    "project {} already holds the clear grant (generation {})",
                                    n.project_id, open.generation
                                )),
                                None if n.generation != next => Some(format!(
                                    "generation {} of project {} is wrong: {next} is next",
                                    n.generation, n.project_id
                                )),
                                None => None,
                            }
                        }
                    };
                    if let Some(why) = why {
                        *s = snapshot;
                        return Ok(Committed::Rejected(why));
                    }
                    s.clear_grants.push(ClearGrantRow {
                        project_id: n.project_id,
                        generation: n.generation,
                        agent_id: n.agent_id,
                        granted_by: n.granted_by,
                        granted_at: n.granted_at,
                        revoked_by: None,
                        revoked_at: None,
                        command_id: commit.command_id.clone(),
                    });
                }
                Effect::RevokeClearGrant {
                    project_id,
                    generation,
                    revoked_by,
                    revoked_at,
                } => {
                    let found = s
                        .clear_grants
                        .iter_mut()
                        .find(|g| g.project_id == project_id && g.generation == generation);
                    match found {
                        Some(g) if g.revoked_at.is_none() => {
                            g.revoked_by = Some(revoked_by);
                            g.revoked_at = Some(revoked_at);
                        }
                        other => {
                            let why = if other.is_some() {
                                format!(
                                    "generation {generation} of project {project_id} is revoked already"
                                )
                            } else {
                                format!(
                                    "project {project_id} holds no clear grant of generation {generation}"
                                )
                            };
                            *s = snapshot;
                            return Ok(Committed::Rejected(why));
                        }
                    }
                }
                Effect::RequireLiveGrant {
                    project_id,
                    generation,
                } => {
                    let live = s
                        .clear_grants
                        .iter()
                        .find(|g| g.project_id == project_id && g.revoked_at.is_none())
                        .map(|g| g.generation);
                    let why = match (s.projects.get(&project_id), live) {
                        (None, _) => Some(format!("no project {project_id}")),
                        (Some(true), _) => Some(format!("project {project_id} is removed")),
                        (Some(false), None) => {
                            Some(format!("project {project_id} holds no live clear grant"))
                        }
                        (Some(false), Some(g)) if g != generation => Some(format!(
                            "the clear grant of project {project_id} is live at generation {g}, not {generation}"
                        )),
                        (Some(false), Some(_)) => None,
                    };
                    if let Some(why) = why {
                        *s = snapshot;
                        return Ok(Committed::Rejected(why));
                    }
                }
                Effect::SetRunProject { run_id, project_id } => {
                    if let Some(run) = s.runs.get_mut(&run_id) {
                        run.project_id = Some(project_id);
                    }
                }
                // The same rules as the real store: the controller's scope only, an unknown run
                // refused, a set value kept, the same value again a no-op.
                Effect::SetRunLaunchBound { run_id, bound } => {
                    let why = if commit.run_id != CONTROLLER_SCOPE {
                        Some(
                            "a run's launch count is set only in an admission under @controller"
                                .to_string(),
                        )
                    } else {
                        match s.runs.get_mut(&run_id) {
                            None => Some(format!("run {run_id} is unknown")),
                            Some(run) => match run.launch_bound {
                                Some(held) if held == bound => None,
                                Some(held) => {
                                    Some(format!("run {run_id} keeps its launch count {held}"))
                                }
                                None => {
                                    run.launch_bound = Some(bound);
                                    None
                                }
                            },
                        }
                    };
                    if let Some(why) = why {
                        *s = snapshot;
                        return Ok(Committed::Rejected(why));
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

    fn requests(&self, run_id: &str) -> Result<Vec<RequestRow>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.run_id == run_id)
            .cloned()
            .collect())
    }

    fn request(&self, id: &str) -> Result<Option<RequestRow>, StoreError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .find(|r| r.id == id)
            .cloned())
    }

    fn claims(&self) -> Result<Vec<ClaimRow>, StoreError> {
        Ok(self.state.lock().unwrap().claims.clone())
    }

    fn projects(&self) -> Result<Vec<ProjectRow>, StoreError> {
        Ok(Vec::new())
    }

    fn project(&self, _id: &str) -> Result<Option<ProjectRow>, StoreError> {
        Ok(None)
    }

    fn devices(&self) -> Result<Vec<DeviceRow>, StoreError> {
        Ok(Vec::new())
    }

    fn device_by_token(&self, _token_sha256: &str) -> Result<Option<DeviceRow>, StoreError> {
        Ok(None)
    }

    fn touch_device(&self, _id: &str, _at: &str, _peer: &str) -> Result<bool, StoreError> {
        Ok(false)
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

    // The engine tests carry no hierarchy: the engine reads none of it, so these reads are empty
    // and these writes change nothing.
    fn worktree(&self, _id: &str) -> Result<Option<WorktreeRow>, StoreError> {
        Ok(None)
    }

    fn worktrees(&self, _project_id: &str) -> Result<Vec<WorktreeRow>, StoreError> {
        Ok(Vec::new())
    }

    fn live_worktree_at(&self, _path: &str) -> Result<Option<WorktreeRow>, StoreError> {
        Ok(None)
    }

    fn agents(&self) -> Result<Vec<AgentRow>, StoreError> {
        Ok(Vec::new())
    }

    fn agent(&self, _id: &str) -> Result<Option<AgentRow>, StoreError> {
        Ok(None)
    }

    fn chat_session(&self, _id: &str) -> Result<Option<ChatSessionRow>, StoreError> {
        Ok(None)
    }

    fn chat_sessions(&self, _project_id: &str) -> Result<Vec<ChatSessionRow>, StoreError> {
        Ok(Vec::new())
    }

    fn live_chat_sessions(&self) -> Result<Vec<ChatSessionRow>, StoreError> {
        Ok(Vec::new())
    }

    fn chat_items_after(
        &self,
        _session_id: &str,
        _after_seq: u64,
        _limit: u32,
    ) -> Result<Vec<ChatItemRow>, StoreError> {
        Ok(Vec::new())
    }

    fn chat_item(&self, _session_id: &str, _seq: u64) -> Result<Option<ChatItemRow>, StoreError> {
        Ok(None)
    }

    fn pending_requests(&self, _session_id: &str) -> Result<Vec<ChatItemRow>, StoreError> {
        Ok(Vec::new())
    }

    fn setting_rows(&self) -> Result<Vec<SettingRow>, StoreError> {
        Ok(Vec::new())
    }

    fn setting_chain(
        &self,
        _key: &str,
        _project_id: Option<&str>,
        _agent_id: Option<&str>,
    ) -> Result<Vec<SettingRow>, StoreError> {
        Ok(Vec::new())
    }

    fn mark_worktree_gone(&self, _id: &str, _at: &str) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn set_chat_session_state(
        &self,
        _id: &str,
        _state: ChatSessionState,
        _at: &str,
    ) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn set_chat_runtime_ref(
        &self,
        _id: &str,
        _runtime_session_ref: &str,
        _at: &str,
    ) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn append_chat_item(&self, _item: NewChatItem) -> Result<ChatItemRow, StoreError> {
        Err(StoreError(
            "the engine's test store holds no chat items".to_string(),
        ))
    }

    fn set_chat_host_mark(
        &self,
        _session_id: &str,
        _mark: pf_engine::port::store::ChatHostMark,
    ) -> Result<(), StoreError> {
        Err(StoreError(
            "the engine's test store holds no chat sessions".to_string(),
        ))
    }

    // The engine tests carry no launch records and no hosts: the engine writes none.
    fn launch_record(
        &self,
        _session_kind: &str,
        _session_id: &str,
        _incarnation: u32,
        _revision: u32,
    ) -> Result<Option<LaunchRecordRow>, StoreError> {
        Ok(None)
    }

    fn newest_launch_record(
        &self,
        _session_kind: &str,
        _session_id: &str,
        _incarnation: u32,
    ) -> Result<Option<LaunchRecordRow>, StoreError> {
        Ok(None)
    }

    fn live_hosts(&self) -> Result<Vec<HostRow>, StoreError> {
        Ok(Vec::new())
    }

    fn host(&self, _unit: &str) -> Result<Option<HostRow>, StoreError> {
        Ok(None)
    }

    fn host_position(&self, _unit: &str) -> Result<Option<HostPositionRow>, StoreError> {
        Ok(None)
    }

    fn notice(&self, _position: u64) -> Result<Option<NoticeRow>, StoreError> {
        Ok(None)
    }

    fn notices_after(&self, _position: u64, _limit: u32) -> Result<Vec<NoticeRow>, StoreError> {
        Ok(Vec::new())
    }

    fn newest_notice(&self) -> Result<u64, StoreError> {
        Ok(0)
    }

    fn notice_deliveries(&self, _position: u64) -> Result<Vec<DeliveryRow>, StoreError> {
        Ok(Vec::new())
    }

    fn waiting_deliveries(&self, _channel: &str) -> Result<Vec<DeliveryRow>, StoreError> {
        Ok(Vec::new())
    }

    fn notice_targets(&self) -> Result<Vec<NoticeTargetRow>, StoreError> {
        Ok(Vec::new())
    }

    fn notice_sends(&self, _position: u64) -> Result<Vec<NoticeSendRow>, StoreError> {
        Ok(Vec::new())
    }

    fn clear_grants(&self) -> Result<Vec<ClearGrantRow>, StoreError> {
        Ok(self.state.lock().unwrap().clear_grants.clone())
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
    /// What `rebind` answers, by session reference. A reference with no entry gets the default
    /// of an adapter with no `rebind` of its own.
    pub rebinds: Mutex<HashMap<String, RebindOutcome>>,
    /// Every `rebind` and `is_alive` call, in order, as `rebind:<ref>` and `alive:<ref>`.
    pub calls: Mutex<Vec<String>>,
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

    /// Script the answer of `rebind` for one session reference.
    pub fn script_rebind(&self, process_ref: &str, outcome: RebindOutcome) {
        self.rebinds
            .lock()
            .unwrap()
            .insert(process_ref.to_string(), outcome);
    }

    pub fn call_log(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
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
        self.calls
            .lock()
            .unwrap()
            .push(format!("alive:{}", session.0));
        Ok(!self.dead.lock().unwrap().contains(&session.0))
    }
    fn rebind(&self, session: &SessionRef) -> Result<RebindOutcome, AgentError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("rebind:{}", session.0));
        Ok(self
            .rebinds
            .lock()
            .unwrap()
            .get(&session.0)
            .cloned()
            .unwrap_or(RebindOutcome::Lost {
                cause: NO_SESSION_HOST.to_string(),
            }))
    }
    fn stop(&self, _session: &SessionRef) -> Result<(), AgentError> {
        Ok(())
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
    pub base: String,
    pub checks: Checks,
    pub mergeable: Mergeable,
    pub merge: ForgeOutcome,
    pub landed: bool,
    /// After the next `pr_status` read, the head becomes this SHA: a push between read and merge.
    pub head_after_read: Option<String>,
    /// The repo `pr_status` reports, when it differs from the repo the PR names.
    pub reported_repo: Option<String>,
    /// The merge effect leaves its branch empty, as a forge that does not read the PR may.
    pub blank_effect_branch: bool,
}

impl PrScript {
    pub fn green(branch: &str, sha: &str) -> PrScript {
        PrScript {
            branch: branch.to_string(),
            sha: sha.to_string(),
            base: "spine".to_string(),
            checks: Checks::Green,
            mergeable: Mergeable::Clean,
            merge: ForgeOutcome::Merged,
            landed: true,
            head_after_read: None,
            reported_repo: None,
            blank_effect_branch: false,
        }
    }
}

/// The head of a PR and each failed check with the reason this forge cannot run it again.
pub type FailedScript = (String, Vec<(String, Option<String>)>);

#[derive(Default)]
pub struct FakeForge {
    pub scripts: Mutex<HashMap<u64, PrScript>>,
    pub merges: Mutex<Vec<u64>>,
    /// Every `pr_status` call, by PR number.
    pub status_reads: Mutex<Vec<u64>>,
    /// Every `merge` call: the PR number and the head the caller pinned.
    pub merge_calls: Mutex<Vec<(u64, String)>>,
    /// The failed checks `failed_checks` answers, by PR number: the head and each check with the
    /// reason this forge cannot run it again.
    pub failed: Mutex<HashMap<u64, FailedScript>>,
    /// Make `failed_checks` fail with this text.
    pub failed_error: Mutex<Option<String>>,
    /// Make `rerun_failed_checks` fail with this text.
    pub rerun_error: Mutex<Option<String>>,
    /// The checks a re-run leaves not run, with the reason.
    pub rerun_not_ran: Mutex<Vec<(String, String)>>,
    /// After the next `failed_checks` read, the PR's head becomes this SHA: a push after the read.
    pub head_after_failed_read: Mutex<Option<String>>,
    /// Every `failed_checks` call, by PR number.
    pub failed_reads: Mutex<Vec<u64>>,
    /// Every `rerun_failed_checks` call: the PR number and the head the caller pinned.
    pub rerun_calls: Mutex<Vec<(u64, String)>>,
    /// The tree of a commit, by SHA.
    pub trees: Mutex<HashMap<String, BTreeMap<String, String>>>,
    /// The tip of a branch, by name.
    pub tips: Mutex<HashMap<String, String>>,
    /// Make `tree` fail for these SHAs.
    pub tree_errors: Mutex<HashSet<String>>,
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

    /// Script the failed checks of a PR: its head, and each failed check with the reason this forge
    /// cannot run it again (`None` for a check it can run again).
    pub fn script_failed(&self, number: u64, head: &str, checks: &[(&str, Option<&str>)]) {
        self.failed.lock().unwrap().insert(
            number,
            (
                head.to_string(),
                checks
                    .iter()
                    .map(|(n, b)| (n.to_string(), b.map(str::to_string)))
                    .collect(),
            ),
        );
    }

    pub fn reruns(&self) -> Vec<(u64, String)> {
        self.rerun_calls.lock().unwrap().clone()
    }

    pub fn script_tree(&self, sha: &str, files: &[(&str, &str)]) {
        self.trees.lock().unwrap().insert(
            sha.to_string(),
            files
                .iter()
                .map(|(p, b)| (p.to_string(), b.to_string()))
                .collect(),
        );
    }

    pub fn script_tip(&self, branch: &str, sha: &str) {
        self.tips
            .lock()
            .unwrap()
            .insert(branch.to_string(), sha.to_string());
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
        self.status_reads.lock().unwrap().push(pr.number);
        let s = self.get(pr)?;
        // A push after the read: the stored head changes, and this read still shows the old one.
        {
            let mut scripts = self.scripts.lock().unwrap();
            let stored = scripts.get_mut(&pr.number).unwrap();
            if let Some(sha) = stored.head_after_read.take() {
                stored.sha = sha;
            }
        }
        Ok(PrStatus {
            repo: s.reported_repo.clone().unwrap_or_else(|| pr.repo.clone()),
            branch: s.branch,
            sha: s.sha,
            base: s.base,
            checks: s.checks,
            mergeable: s.mergeable,
        })
    }
    fn merge(&self, pr: &PrRef, expected_sha: &str) -> Result<ForgeEffect, ForgeError> {
        self.merge_calls
            .lock()
            .unwrap()
            .push((pr.number, expected_sha.to_string()));
        let s = self.get(pr)?;
        if s.sha != expected_sha {
            // The head is not the one the caller read: refuse, and merge nothing.
            return Ok(ForgeEffect {
                repo: pr.repo.clone(),
                branch: s.branch,
                sha: s.sha.clone(),
                outcome: ForgeOutcome::Refused(format!(
                    "the head moved: expected {expected_sha}, found {}",
                    s.sha
                )),
            });
        }
        self.merges.lock().unwrap().push(pr.number);
        Ok(ForgeEffect {
            repo: pr.repo.clone(),
            branch: if s.blank_effect_branch {
                String::new()
            } else {
                s.branch
            },
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
    fn failed_checks(&self, pr: &PrRef) -> Result<FailedChecks, ForgeError> {
        self.failed_reads.lock().unwrap().push(pr.number);
        if let Some(why) = self.failed_error.lock().unwrap().clone() {
            return Err(ForgeError(why));
        }
        let (head, checks) = self
            .failed
            .lock()
            .unwrap()
            .get(&pr.number)
            .cloned()
            .ok_or_else(|| ForgeError(format!("no failed checks scripted for PR {}", pr.number)))?;
        // A push after the read: the PR's head changes, and this read still shows the old one.
        if let Some(moved) = self.head_after_failed_read.lock().unwrap().take()
            && let Some(script) = self.scripts.lock().unwrap().get_mut(&pr.number)
        {
            script.sha = moved;
        }
        Ok(FailedChecks {
            head_sha: head,
            failed: checks
                .into_iter()
                .map(|(name, blocked)| FailedCheck { name, blocked })
                .collect(),
        })
    }
    fn rerun_failed_checks(
        &self,
        pr: &PrRef,
        expected_sha: &str,
    ) -> Result<RerunEffect, ForgeError> {
        self.rerun_calls
            .lock()
            .unwrap()
            .push((pr.number, expected_sha.to_string()));
        if let Some(why) = self.rerun_error.lock().unwrap().clone() {
            return Err(ForgeError(why));
        }
        let mut scripts = self.scripts.lock().unwrap();
        let script = scripts
            .get_mut(&pr.number)
            .ok_or_else(|| ForgeError(format!("no PR {}", pr.number)))?;
        if script.sha != expected_sha {
            return Ok(RerunEffect {
                repo: pr.repo.clone(),
                number: pr.number,
                sha: script.sha.clone(),
                ran: Vec::new(),
                not_ran: Vec::new(),
                head_moved: Some(script.sha.clone()),
            });
        }
        let not_ran = self.rerun_not_ran.lock().unwrap().clone();
        let names: Vec<String> = self
            .failed
            .lock()
            .unwrap()
            .get(&pr.number)
            .map(|(_, c)| c.iter().map(|(n, _)| n.clone()).collect())
            .unwrap_or_default();
        let ran: Vec<String> = names
            .into_iter()
            .filter(|n| !not_ran.iter().any(|(m, _)| m == n))
            .collect();
        if !ran.is_empty() {
            script.checks = Checks::Pending;
        }
        Ok(RerunEffect {
            repo: pr.repo.clone(),
            number: pr.number,
            sha: expected_sha.to_string(),
            ran,
            not_ran,
            head_moved: None,
        })
    }
    fn tree(&self, _repo: &str, sha: &str) -> Result<BTreeMap<String, String>, ForgeError> {
        if self.tree_errors.lock().unwrap().contains(sha) {
            return Err(ForgeError(format!("tree {sha} cannot be read")));
        }
        self.trees
            .lock()
            .unwrap()
            .get(sha)
            .cloned()
            .ok_or_else(|| ForgeError(format!("no tree scripted for {sha}")))
    }
    fn branch_tip(&self, _repo: &str, branch: &str) -> Result<String, ForgeError> {
        self.tips
            .lock()
            .unwrap()
            .get(branch)
            .cloned()
            .ok_or_else(|| ForgeError(format!("no tip scripted for {branch}")))
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

/// Like [`workflow`], with a merge definition that declares the repo and the base.
pub fn guarded_workflow(repo: &str, base: &str, head_check: bool) -> Workflow {
    let mut wf = workflow(&["build"], true);
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.repo = repo.to_string();
    def.base = base.to_string();
    def.head_check = head_check;
    wf
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
