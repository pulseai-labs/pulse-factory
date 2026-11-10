//! Start proposals (`r1.s4.w2`; ADR-0001, ADR-0003, ADR-0005, ADR-0009, ADR-0012, ADR-0015,
//! ADR-0016; `docs/contracts/cli.md` `propose`, `docs/contracts/api.md`).
//!
//! The Orchestrator chat runs `pulse-factory propose`. The verb starts nothing. The controller finds
//! the chat that called by the ancestry of the caller, copies the workflow package into its own state
//! directory, builds the arm plan in its own process from the inputs, the copy and that chat's project,
//! checks it with the dry run `arm --dry-run` makes, and posts a **start card** into the chat. The card
//! carries the hash of the plan. A paired device confirms it: `confirm_code` with the `arm` subject
//! binds the proposal and the hash, and `arm` spends the code. The spend checks the copy's hash before
//! any of its code runs, builds the plan again from the copy, and arms with the copy of the package.
//!
//! The table is in memory only (ADR-0007): a restart withdraws every open proposal. A session holds at
//! most one open proposal. Every proposal that leaves `Open` removes its copy of the package.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use pf_api::types::{
    self as wire, AnswerOutcome, ArmActed, ArmSummary, ConfirmIssued, ConfirmSubjectShown,
    DeviceArmArgs, DeviceRecord, SCHEMA_VERSION, StartResultKind, StartShown,
};
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{AdmissionOutcome, CONTROLLER_SCOPE, ChatItemKind, Effect, Store};
use pf_ipc::{Peer, Response};
use pf_workflow::package::Package;
use pf_workflow::pin;
use serde::Deserialize;
use serde_json::{Value, json};

use super::commands::{Plan, check_command_id, field};
use super::confirm::{CODE_BYTES, CODE_PREFIX, Subject};
use super::device_acts::{plan_refused, refusal, refused_reason};
use super::seat::{hex, random_bytes};
use super::{ArmStage, Controller, PACKAGE_MOVED, files, lock};
use crate::arm::{ArmInputs, ArmPlan, build_plan, build_summary, check_earlier};

/// The text every proposal ID starts with: `sp-` and 16 lower-case hex digits (8 random bytes).
pub const PROPOSAL_PREFIX: &str = "sp-";

/// The random bytes of a proposal ID.
const PROPOSAL_BYTES: usize = 8;

/// The window of the build bound: one minute.
const BUILD_WINDOW: Duration = Duration::from_secs(60);

/// The prefix a refused `arm` cause of the plan builder carries.
const ARM_PREFIX: &str = "pulse-factory: arm refused: ";

/// Faults a test injects into the arm of a start card, as `Config.random_device` is a seam.
#[derive(Default)]
pub struct StartFaults {
    /// The `run_project` link fails, whatever the store says.
    pub run_project: AtomicBool,
    /// How many of the next `start_result` appends fail.
    pub result_append: AtomicU32,
}

/// Where a proposal stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalState {
    Open,
    /// A spend is arming it now.
    Arming,
    Armed(String),
    Withdrawn(String),
    Failed(String),
}

impl ProposalState {
    fn text(&self) -> String {
        match self {
            ProposalState::Open => "open".to_string(),
            ProposalState::Arming => "arming".to_string(),
            ProposalState::Armed(run) => format!("armed:{run}"),
            ProposalState::Withdrawn(why) => format!("withdrawn:{why}"),
            ProposalState::Failed(why) => format!("failed:{why}"),
        }
    }
}

/// The result item a card still needs.
#[derive(Clone, Debug)]
struct PendingResult {
    result: StartResultKind,
    reason: Option<String>,
    run: Option<String>,
    device: Option<String>,
    /// True once the one stderr line of a failed append was printed.
    noted: bool,
}

/// One proposal.
#[derive(Clone)]
pub struct Proposal {
    pub id: String,
    pub session: String,
    pub card_seq: u64,
    pub project_id: String,
    pub command_id: String,
    pub inputs: ArmInputs,
    /// The plan as built, with no project and no expected package.
    pub plan: ArmPlan,
    pub summary: ArmSummary,
    pub plan_sha256: String,
    /// The `content` hash of the package snapshot's pin.
    pub package_content: String,
    pub state: ProposalState,
    pending: Option<PendingResult>,
}

#[derive(Default)]
struct Table {
    /// In the order the proposals were made.
    list: Vec<Proposal>,
    /// The `propose` command ID to its proposal.
    by_command: BTreeMap<String, String>,
    /// The builds of each session in the last minute.
    builds: BTreeMap<String, Vec<Instant>>,
}

/// The proposals of this controller, in memory.
#[derive(Default)]
pub struct Proposals {
    table: Mutex<Table>,
}

impl Proposals {
    fn builds_in_window(&self, session: &str) -> usize {
        let mut t = lock(&self.table);
        let now = Instant::now();
        let v = t.builds.entry(session.to_string()).or_default();
        v.retain(|at| now.duration_since(*at) < BUILD_WINDOW);
        v.len()
    }

    fn note_build(&self, session: &str) {
        lock(&self.table)
            .builds
            .entry(session.to_string())
            .or_default()
            .push(Instant::now());
    }

    fn get(&self, id: &str) -> Option<Proposal> {
        lock(&self.table).list.iter().find(|p| p.id == id).cloned()
    }

    fn by_command(&self, command_id: &str) -> Option<Proposal> {
        let t = lock(&self.table);
        let id = t.by_command.get(command_id)?;
        t.list.iter().find(|p| &p.id == id).cloned()
    }

    /// Record a new proposal as `Open`.
    fn insert(&self, p: Proposal) {
        let mut t = lock(&self.table);
        t.by_command.insert(p.command_id.clone(), p.id.clone());
        t.list.push(p);
    }

    /// Why a proposal cannot be confirmed or armed, or `Ok` when it is open at that hash.
    fn check_open(&self, id: &str, plan_sha256: &str) -> Result<Proposal, String> {
        let t = lock(&self.table);
        let Some(p) = t.list.iter().find(|p| p.id == id) else {
            return Err(format!(
                "proposal {id} is unknown or gone (a restart withdraws every proposal)"
            ));
        };
        match &p.state {
            ProposalState::Open => {}
            ProposalState::Arming => return Err(format!("proposal {id} is being armed")),
            ProposalState::Armed(run) => {
                return Err(format!("proposal {id} is already armed: run {run}"));
            }
            ProposalState::Withdrawn(why) => {
                return Err(format!("proposal {id} was withdrawn: {why}"));
            }
            ProposalState::Failed(why) => return Err(format!("proposal {id} failed: {why}")),
        }
        if p.plan_sha256 != plan_sha256 {
            return Err(format!(
                "the plan of {id} is {}, not {plan_sha256}",
                p.plan_sha256
            ));
        }
        Ok(p.clone())
    }

    /// `Open` to `Arming`, once: a second spend sees `Arming`.
    fn begin_arming(&self, id: &str, plan_sha256: &str) -> Result<Proposal, String> {
        let mut t = lock(&self.table);
        let Some(p) = t.list.iter_mut().find(|p| p.id == id) else {
            return Err(format!(
                "proposal {id} is unknown or gone (a restart withdraws every proposal)"
            ));
        };
        match &p.state {
            ProposalState::Open => {}
            ProposalState::Arming => return Err(format!("proposal {id} is being armed")),
            ProposalState::Armed(run) => {
                return Err(format!("proposal {id} is already armed: run {run}"));
            }
            ProposalState::Withdrawn(why) => {
                return Err(format!("proposal {id} was withdrawn: {why}"));
            }
            ProposalState::Failed(why) => return Err(format!("proposal {id} failed: {why}")),
        }
        if p.plan_sha256 != plan_sha256 {
            return Err(format!(
                "the plan of {id} is {}, not {plan_sha256}",
                p.plan_sha256
            ));
        }
        p.state = ProposalState::Arming;
        Ok(p.clone())
    }

    /// `Arming` back to `Open`: a transient cause.
    fn reopen(&self, id: &str) {
        let mut t = lock(&self.table);
        if let Some(p) = t.list.iter_mut().find(|p| p.id == id)
            && p.state == ProposalState::Arming
        {
            p.state = ProposalState::Open;
        }
    }

    /// Move a proposal to a state that ends it, and queue the result its card needs. A proposal
    /// that ended already is left as it is.
    fn end(&self, id: &str, state: ProposalState, pending: PendingResult) -> bool {
        let mut t = lock(&self.table);
        match t.list.iter_mut().find(|p| p.id == id) {
            Some(p)
                if matches!(p.state, ProposalState::Open | ProposalState::Arming)
                    && p.pending.is_none() =>
            {
                p.state = state;
                p.pending = Some(pending);
                true
            }
            _ => false,
        }
    }

    /// Every proposal that is `Open` and passes the filter becomes `Withdrawn`. Returns the IDs.
    fn withdraw_where(&self, keep: impl Fn(&Proposal) -> bool, reason: &str) -> Vec<String> {
        let mut t = lock(&self.table);
        let mut ids = Vec::new();
        for p in t.list.iter_mut() {
            if p.state == ProposalState::Open && keep(p) {
                p.state = ProposalState::Withdrawn(reason.to_string());
                p.pending = Some(PendingResult {
                    result: StartResultKind::Withdrawn,
                    reason: Some(reason.to_string()),
                    run: None,
                    device: None,
                    noted: false,
                });
                ids.push(p.id.clone());
            }
        }
        ids
    }

    fn pending_of(&self, id: &str) -> Option<(String, u64, PendingResult)> {
        let t = lock(&self.table);
        let p = t.list.iter().find(|p| p.id == id)?;
        Some((p.session.clone(), p.card_seq, p.pending.clone()?))
    }

    fn clear_pending(&self, id: &str) {
        let mut t = lock(&self.table);
        if let Some(p) = t.list.iter_mut().find(|p| p.id == id) {
            p.pending = None;
        }
    }

    fn note_pending(&self, id: &str) -> bool {
        let mut t = lock(&self.table);
        match t
            .list
            .iter_mut()
            .find(|p| p.id == id)
            .and_then(|p| p.pending.as_mut())
        {
            Some(pending) if !pending.noted => {
                pending.noted = true;
                true
            }
            _ => false,
        }
    }

    fn with_pending(&self) -> Vec<String> {
        lock(&self.table)
            .list
            .iter()
            .filter(|p| p.pending.is_some())
            .map(|p| p.id.clone())
            .collect()
    }

    /// The open proposal of a session, if any, other than `except`.
    fn open_of_session(&self, session: &str, except: &str) -> Option<String> {
        lock(&self.table)
            .list
            .iter()
            .find(|p| p.session == session && p.id != except && p.state == ProposalState::Open)
            .map(|p| p.id.clone())
    }

    fn state_text(&self, id: &str) -> Option<String> {
        self.get(id).map(|p| p.state.text())
    }

    fn ids(&self) -> Vec<String> {
        lock(&self.table)
            .list
            .iter()
            .map(|p| p.id.clone())
            .collect()
    }
}

/// The directory of the proposals' snapshots: `<state dir>/proposals`.
pub fn proposals_dir(state: &Path) -> PathBuf {
    state.join("proposals")
}

/// The hash a start card carries: SHA-256 of the compact JSON of the plan (with no project and no
/// expected package), the summary and the content hash of the package snapshot's pin. Two builds
/// hash equal exactly when the three are equal.
pub fn plan_hash(plan: &ArmPlan, summary: &ArmSummary, package: &str) -> String {
    let doc = json!({ "plan": plan, "summary": summary, "package": package });
    sha256_hex(serde_json::to_string(&doc).unwrap_or_default().as_bytes())
}

/// Pin the package in `dir` and give the `content` hash.
fn pin_content(dir: &Path) -> Result<String, String> {
    let package = Package::load(dir).map_err(|e| e.to_string())?;
    pin::pin_package(&package)
        .map(|p| p.content)
        .map_err(|e| format!("cannot pin the package: {e}"))
}

/// Copy the pinned files of the package at `source` into `dest`, which this makes with mode 0700.
fn copy_package(source: &Path, dest: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let package = Package::load(source).map_err(|e| format!("the workflow does not load: {e}"))?;
    if let Some(parent) = dest.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|e| format!("cannot make {}: {e}", parent.display()))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dest)
        .map_err(|e| format!("cannot make {}: {e}", dest.display()))?;
    files::copy_files(source, dest, &package.pinned_files())
}

/// The first part of a summary that differs: what the plan change is, for the refusal.
fn plan_diff(was: &ArmSummary, now: &ArmSummary) -> String {
    if was.items != now.items {
        return format!("items {:?} became {:?}", was.items, now.items);
    }
    if was.roles != now.roles {
        let changed = now
            .roles
            .iter()
            .find(|r| !was.roles.contains(r))
            .map_or_else(
                || "a role was removed".to_string(),
                |r| {
                    format!(
                        "role {} is now runtime {}, model {}",
                        r.name, r.runtime, r.model
                    )
                },
            );
        return changed;
    }
    if was.most_launches != now.most_launches {
        return format!(
            "most launches {:?} became {:?}",
            was.most_launches, now.most_launches
        );
    }
    if was.caps != now.caps {
        return "the caps".to_string();
    }
    if was.merges != now.merges {
        return "the merge lines".to_string();
    }
    "the plan, its package or its forge facts".to_string()
}

fn strip_arm_prefix(message: String) -> String {
    message
        .strip_prefix(ARM_PREFIX)
        .unwrap_or(&message)
        .to_string()
}

/// A relative path is refused: `.`, `./`, `../` or not absolute.
fn check_absolute(what: &str, path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "{what} {} is not an absolute path; the controller runs in another directory",
            path.display()
        ))
    }
}

/// The narrowing of `propose`'s inputs (W1, W2): a workflow by installed name only, no replace, and
/// every path absolute. It is not a security control: the boundary is Release 2.
fn check_inputs(inputs: &ArmInputs) -> Result<(), String> {
    if inputs.replace.is_some() {
        return Err(
            "replacing a run is the operator's; use pulse-factory arm --replace".to_string(),
        );
    }
    let name = &inputs.workflow;
    if name.contains('/') {
        return Err("--workflow takes the name of an installed workflow, not a path".to_string());
    }
    if name.is_empty() || name == "." || name == ".." {
        return Err("--workflow takes the name of an installed workflow".to_string());
    }
    check_absolute("--ledger", &inputs.ledger)?;
    for root in &inputs.plugin_roots {
        check_absolute("--plugin-root", root)?;
    }
    if let Some(paper) = &inputs.paper {
        check_absolute("--paper", paper)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct ProposeArgs {
    inputs: ArmInputs,
    command_id: String,
}

fn given_params(raw: &[String]) -> BTreeMap<String, String> {
    raw.iter()
        .filter_map(|item| item.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Controller {
    // ------------------------------------------------------------------ propose

    /// The `propose` verb over the socket. Every call is audited: an admitted call is its `propose`
    /// admission, and a refused call is a refused admission under a command ID the controller
    /// makes, with the caller's key.
    pub(super) fn verb_propose(&self, args: &Value, peer: Peer) -> Response {
        let caller = self.settings_caller_key(peer);
        let session = match self.identify_chat_caller(peer) {
            Ok(s) => s,
            Err(why) => return self.propose_refusal(&caller, &why),
        };
        let parsed: ProposeArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => {
                return self
                    .propose_refusal(&caller, &format!("the arguments are not readable: {e}"));
            }
        };
        let ProposeArgs { inputs, command_id } = parsed;
        if let Err(why) = check_command_id(&command_id) {
            return self.propose_refusal(&caller, &why);
        }
        let content = json!({ "session": session, "inputs": inputs }).to_string();
        // A command the store has seen is decided by its record: a replay, a conflict, or the
        // first refusal. It builds nothing and counts for nothing.
        if matches!(
            self.store.admission(CONTROLLER_SCOPE, &command_id),
            Ok(Some(_))
        ) {
            return match self.run_command(
                "propose",
                &command_id,
                content,
                Plan::Refuse(String::new()),
            ) {
                Ok((_, true)) => self.replayed_proposal(&command_id, &session),
                Ok((_, false)) => {
                    Response::err("propose refused: the store answered a replay as new".to_string())
                }
                Err(why) => Response::err(format!("propose refused: {why}")),
            };
        }
        if let Err(why) = check_inputs(&inputs) {
            return self.propose_refusal(&caller, &why);
        }
        let row = match self.store.chat_session(&session) {
            Ok(Some(r)) if r.ended_at.is_none() => r,
            Ok(_) => {
                return self.propose_refusal(&caller, &format!("chat session {session} has ended"));
            }
            Err(e) => return self.propose_refusal(&caller, &e.to_string()),
        };
        // The bound, before any build (W19).
        let (limit, from) = self.propose_per_min(&row.project_id, &row.agent_id);
        if limit > 0 {
            let made = self.proposals.builds_in_window(&session);
            if made as u64 >= limit {
                let why = format!(
                    "propose_rate: {session} made {made} proposals in the last minute \
                     (chat.propose_per_min = {limit}, from {from})"
                );
                self.refuse_coalesced(
                    "propose",
                    "propose_rate".to_string(),
                    content,
                    &caller,
                    &why,
                );
                return Response::err(format!("propose refused: {why}"));
            }
        }
        self.proposals.note_build(&session);
        match self.store.project(&row.project_id) {
            Ok(Some(p)) if p.removed_at.is_none() => {}
            Ok(Some(_)) => {
                return self
                    .propose_refusal(&caller, &format!("project {} is removed", row.project_id));
            }
            Ok(None) => {
                return self
                    .propose_refusal(&caller, &format!("project {} is unknown", row.project_id));
            }
            Err(e) => return self.propose_refusal(&caller, &e.to_string()),
        }
        let project_name = self
            .store
            .project(&row.project_id)
            .ok()
            .flatten()
            .map(|p| p.name)
            .unwrap_or_default();

        // The proposal ID is drawn first, so the snapshot has its own directory.
        let id = match random_bytes(&self.cfg.random_device, PROPOSAL_BYTES) {
            Ok(b) => format!("{PROPOSAL_PREFIX}{}", hex(&b)),
            Err(why) => {
                return self.propose_refusal(&caller, &format!("no proposal can be made: {why}"));
            }
        };
        let dir = proposals_dir(&self.cfg.state_dir).join(&id);
        let snapshot = dir.join("package");
        let built = self.build_proposal(&inputs, &snapshot);
        let (prepared, package_content) = match built {
            Ok(b) => b,
            Err(why) => {
                let _ = std::fs::remove_dir_all(&dir);
                return self.propose_refusal(&caller, &why);
            }
        };
        let summary = build_summary(&prepared);
        let plan = prepared.plan;
        let plan_sha256 = plan_hash(&plan, &summary, &package_content);

        let line = format!(
            "propose {} proposal={} project={} spine={} workflow={} items={} most_launches={} \
             plan={} command={}",
            field(&session),
            id,
            field(&row.project_id),
            field(&plan.spine),
            field(&summary.workflow.name),
            summary
                .items
                .map_or_else(|| "unknown".to_string(), |n| n.to_string()),
            summary
                .most_launches
                .map_or_else(|| "unknown".to_string(), |n| n.to_string()),
            plan_sha256,
            command_id
        );
        let admitted = self.run_command(
            "propose",
            &command_id,
            content,
            Plan::Admit {
                effects: Vec::new(),
                subject: id.clone(),
                line,
            },
        );
        match admitted {
            Ok((_, false)) => {}
            Ok((_, true)) => {
                // Another call took this command ID between the check and the commit.
                let _ = std::fs::remove_dir_all(&dir);
                return self.replayed_proposal(&command_id, &session);
            }
            Err(why) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Response::err(format!("propose refused: {why}"));
            }
        }

        // The card.
        let body = wire::ChatItemBody::StartCard {
            proposal: id.clone(),
            project_id: row.project_id.clone(),
            project_name,
            plan_sha256: plan_sha256.clone(),
            summary: summary.clone(),
            params: given_params(&inputs.params),
            ledger: inputs.ledger.display().to_string(),
            plugin_roots: inputs
                .plugin_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        };
        let item = super::chat_view::new_item(&session, &body);
        let card = match self.append_item(item) {
            Ok(row) => row,
            Err(why) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Response::err(format!(
                    "propose refused: the start card is not stored: {why}"
                ));
            }
        };
        self.proposals.insert(Proposal {
            id: id.clone(),
            session: session.clone(),
            card_seq: card.seq,
            project_id: row.project_id.clone(),
            command_id: command_id.clone(),
            inputs: inputs.clone(),
            plan,
            summary: summary.clone(),
            plan_sha256: plan_sha256.clone(),
            package_content,
            state: ProposalState::Open,
            pending: None,
        });
        // A session holds at most one open proposal: the new one supersedes the last.
        if let Some(old) = self.proposals.open_of_session(&session, &id) {
            let reason = format!("superseded by {id}");
            let ids = self.proposals.withdraw_where(|p| p.id == old, &reason);
            for gone in ids {
                self.remove_snapshot(&gone);
                self.flush_start_result(&gone);
            }
        }
        Response::ok(json!({
            "schema_version": SCHEMA_VERSION,
            "replayed": false,
            "proposal": id,
            "session": session,
            "project_id": row.project_id,
            "plan_sha256": plan_sha256,
            "summary": summary,
        }))
    }

    /// Copy the package into the proposal's snapshot, pin the copy, build the plan from the copy and
    /// check it against the earlier runs: the dry run `arm --dry-run` makes. The refusal is the cause.
    fn build_proposal(
        &self,
        inputs: &ArmInputs,
        snapshot: &Path,
    ) -> Result<(crate::arm::Prepared, String), String> {
        let source = self
            .cfg
            .chat_share_dir
            .join("workflows")
            .join(&inputs.workflow);
        copy_package(&source, snapshot)?;
        let content = pin_content(snapshot)?;
        let prepared = build_plan(inputs, snapshot).map_err(strip_arm_prefix)?;
        check_earlier(&self.run_infos(), None).map_err(strip_arm_prefix)?;
        Ok((prepared, content))
    }

    /// One refused call: a refused admission under a command ID the controller makes, so the
    /// caller's own ID is never used up, with the caller's key. The answer is the cause.
    fn propose_refusal(&self, caller: &str, cause: &str) -> Response {
        let reason = pf_engine::status::redact(cause);
        let id = match random_bytes(&self.cfg.random_device, 16) {
            Ok(b) => format!("propose-refused-{}", hex(&b)),
            Err(_) => {
                // No random bytes: the refusal still answers, and the call is not audited.
                return Response::err(format!("propose refused: {reason}"));
            }
        };
        match self.run_command(
            "propose",
            &id,
            json!({ "caller": caller }).to_string(),
            Plan::RefuseDetailed {
                why: reason.clone(),
                detail: format!(" caller={caller}"),
            },
        ) {
            // A refused admission answers `Err(reason)`: that is the success of this call.
            Ok(_) | Err(_) => {}
        }
        Response::err(format!("propose refused: {reason}"))
    }

    /// The answer to a replay of a `propose` command: the first proposal, or its facts when a
    /// restart took the table.
    fn replayed_proposal(&self, command_id: &str, session: &str) -> Response {
        match self.proposals.by_command(command_id) {
            Some(p) => Response::ok(json!({
                "schema_version": SCHEMA_VERSION,
                "replayed": true,
                "proposal": p.id,
                "session": p.session,
                "project_id": p.project_id,
                "plan_sha256": p.plan_sha256,
                "summary": p.summary,
            })),
            None => Response::ok(json!({
                "schema_version": SCHEMA_VERSION,
                "replayed": true,
                "proposal": null,
                "session": session,
                "project_id": null,
                "plan_sha256": null,
                "summary": null,
                "note": "the proposal is gone: a restart withdraws every proposal; propose again",
            })),
        }
    }

    // ------------------------------------------------------------------ the table

    /// For tests: the state of a proposal as text (`open`, `arming`, `armed:<run>`,
    /// `withdrawn:<why>`, `failed:<why>`).
    pub fn proposal_state(&self, id: &str) -> Option<String> {
        self.proposals.state_text(id)
    }

    /// For tests: the faults that a running controller reads, so a test flips one after the start.
    pub fn start_faults(&self) -> &StartFaults {
        &self.cfg.start_faults
    }

    /// For tests: the IDs of every proposal, in the order they were made.
    pub fn proposal_ids(&self) -> Vec<String> {
        self.proposals.ids()
    }

    /// Is the proposal open at this plan hash? `Err` names what moved. It reads the table only.
    pub(super) fn proposal_is_open_for(&self, id: &str, plan_sha256: &str) -> Result<(), String> {
        self.proposals.check_open(id, plan_sha256).map(|_| ())
    }

    fn remove_snapshot(&self, id: &str) {
        let _ = std::fs::remove_dir_all(proposals_dir(&self.cfg.state_dir).join(id));
    }

    /// Withdraw every open proposal that passes the filter (a session, or every one), with a
    /// reason, and append each card's result. Returns the IDs.
    pub(super) fn withdraw_open_proposals(
        &self,
        session: Option<&str>,
        reason: &str,
    ) -> Vec<String> {
        let ids = self
            .proposals
            .withdraw_where(|p| session.is_none_or(|s| p.session == s), reason);
        for id in &ids {
            self.remove_snapshot(id);
            self.flush_start_result(id);
        }
        ids
    }

    /// End a proposal as `Failed` with a permanent cause: its snapshot goes and its card gets
    /// `failed`.
    fn fail_proposal(&self, id: &str, cause: &str) {
        let ended = self.proposals.end(
            id,
            ProposalState::Failed(cause.to_string()),
            PendingResult {
                result: StartResultKind::Failed,
                reason: Some(cause.to_string()),
                run: None,
                device: None,
                noted: false,
            },
        );
        if ended {
            self.remove_snapshot(id);
            self.flush_start_result(id);
        }
    }

    /// The `start_result` item of a card.
    fn append_start_result(
        &self,
        session: &str,
        card_seq: u64,
        result: &PendingResult,
    ) -> Result<(), String> {
        let faults = &self.cfg.start_faults.result_append;
        if faults
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("the append failed (a fault a test injected)".to_string());
        }
        let body = wire::ChatItemBody::StartResult {
            outcome: match result.result {
                StartResultKind::Armed => AnswerOutcome::Answered,
                StartResultKind::Withdrawn | StartResultKind::Failed => AnswerOutcome::Withdrawn,
            },
            result: result.result,
            reason: result.reason.clone(),
            run: result.run.clone(),
            device: result.device.clone(),
        };
        let mut item = super::chat_view::new_item(session, &body);
        item.answers_seq = Some(card_seq);
        self.append_item(item).map(|_| ())
    }

    /// Append the result a proposal's card waits for. A failure is kept, with one stderr line, and
    /// the next tick tries again.
    fn flush_start_result(&self, id: &str) {
        let Some((session, seq, pending)) = self.proposals.pending_of(id) else {
            return;
        };
        match self.append_start_result(&session, seq, &pending) {
            Ok(()) => self.proposals.clear_pending(id),
            Err(why) => {
                if self.proposals.note_pending(id) {
                    eprintln!(
                        "pulse-factory: the result of start card {id} is not stored: {why}; \
                         it is tried again at each tick"
                    );
                }
            }
        }
    }

    /// The tick: every result that did not land is appended again.
    pub(super) fn retry_start_results(&self) {
        for id in self.proposals.with_pending() {
            self.flush_start_result(&id);
        }
    }

    /// At controller start: the snapshots go, and every start card with no result in a session that
    /// has not ended gets one. A card whose proposal has a `run_project` admission got its run, so it
    /// gets `armed`. The others get `withdrawn`.
    pub(super) fn recover_start_cards(&self) {
        let _ = std::fs::remove_dir_all(proposals_dir(&self.cfg.state_dir));
        let Ok(projects) = self.store.projects() else {
            return;
        };
        let mut waiting: Vec<(String, u64, String)> = Vec::new();
        for project in projects {
            let Ok(sessions) = self.store.chat_sessions(&project.id) else {
                continue;
            };
            for session in sessions.into_iter().filter(|s| s.ended_at.is_none()) {
                let mut cards: Vec<(u64, String)> = Vec::new();
                let mut answered: std::collections::BTreeSet<u64> = Default::default();
                let mut after = 0;
                while let Ok(page) = self.store.chat_items_after(&session.id, after, 500) {
                    let Some(last) = page.last().map(|i| i.seq) else {
                        break;
                    };
                    for item in &page {
                        if item.kind == ChatItemKind::StartCard.as_str() {
                            let proposal = serde_json::from_str::<Value>(&item.payload)
                                .ok()
                                .and_then(|v| {
                                    v.get("proposal")
                                        .and_then(Value::as_str)
                                        .map(str::to_string)
                                })
                                .unwrap_or_default();
                            cards.push((item.seq, proposal));
                        } else if item.kind == ChatItemKind::StartResult.as_str()
                            && let Some(seq) = item.answers_seq
                        {
                            answered.insert(seq);
                        }
                    }
                    after = last;
                }
                for (seq, proposal) in cards {
                    if !answered.contains(&seq) {
                        waiting.push((session.id.clone(), seq, proposal));
                    }
                }
            }
        }
        if waiting.is_empty() {
            return;
        }
        let admissions = self.store.admissions(CONTROLLER_SCOPE).unwrap_or_default();
        let of_proposal = |kind: &str, proposal: &str| -> Option<Value> {
            admissions
                .iter()
                .filter(|a| a.kind == kind && a.outcome == AdmissionOutcome::Admitted)
                .filter_map(|a| serde_json::from_str::<Value>(&a.content).ok())
                .find(|c| c.get("proposal").and_then(Value::as_str) == Some(proposal))
        };
        for (session, seq, proposal) in waiting {
            let linked = of_proposal("run_project", &proposal);
            let pending = match linked {
                Some(c) => PendingResult {
                    result: StartResultKind::Armed,
                    reason: None,
                    run: c.get("run").and_then(Value::as_str).map(str::to_string),
                    device: of_proposal("device_arm", &proposal)
                        .and_then(|a| a.get("device").and_then(Value::as_str).map(str::to_string)),
                    noted: false,
                },
                None => PendingResult {
                    result: StartResultKind::Withdrawn,
                    reason: Some(
                        "the controller restarted; ask the Orchestrator to propose again"
                            .to_string(),
                    ),
                    run: None,
                    device: None,
                    noted: false,
                },
            };
            if let Err(why) = self.append_start_result(&session, seq, &pending) {
                eprintln!(
                    "pulse-factory: the start card {seq} of chat session {session} is not closed: {why}"
                );
            }
        }
    }

    // ------------------------------------------------------------------ the run's project

    /// The `run_project` admission: link a run to a live project, under `@controller`, before the
    /// run's `run.json` is written.
    pub(super) fn link_run_project(
        &self,
        run_id: &str,
        project: &str,
        proposal: Option<&str>,
    ) -> Result<(), String> {
        if self.cfg.start_faults.run_project.load(Ordering::SeqCst) {
            return Err(
                "the run cannot be linked to its project (a fault a test injected)".to_string(),
            );
        }
        let command_id = format!("run-project-{run_id}");
        let content =
            json!({ "run": run_id, "project": project, "proposal": proposal }).to_string();
        let line = format!(
            "run_project {} project={} proposal={} admitted command={command_id}",
            field(run_id),
            field(project),
            field(proposal.unwrap_or("none")),
        );
        self.run_command(
            "run_project",
            &command_id,
            content,
            Plan::Admit {
                effects: vec![Effect::SetRunProject {
                    run_id: run_id.to_string(),
                    project_id: project.to_string(),
                }],
                subject: run_id.to_string(),
                line,
            },
        )
        .map(|_| ())
        .map_err(|why| format!("the run cannot be linked to its project: {why}"))
    }

    // ------------------------------------------------------------------ confirm_code, arm subject

    /// `confirm_code` with the `arm` subject. The proposal must be open at the hash; the issue does
    /// not build the plan again. The code binds no claim generation (W6).
    pub(super) fn device_confirm_arm(
        &self,
        device: &DeviceRecord,
        command_id: &str,
        proposal: &str,
        plan_sha256: &str,
    ) -> Response {
        if let Err(why) = check_command_id(command_id) {
            return Response::err(why);
        }
        // Fail closed: with no random bytes there is no code, and no admission.
        let bytes = match random_bytes(&self.cfg.random_device, CODE_BYTES) {
            Ok(b) => b,
            Err(why) => {
                return Response::err(format!("confirm_code refused: no code can be made: {why}"));
            }
        };
        let _one_at_a_time = lock(&self.claim_lock);
        let dev = match self.live_device(&device.id) {
            Ok(d) => d,
            Err(r) => return r,
        };
        let content = json!({
            "device": dev.id,
            "subject": { "kind": "arm", "proposal": proposal, "plan_sha256": plan_sha256 },
        })
        .to_string();
        if matches!(
            self.store.admission(CONTROLLER_SCOPE, command_id),
            Ok(Some(_))
        ) {
            return match self.run_command(
                "confirm_code",
                command_id,
                content,
                Plan::Refuse(String::new()),
            ) {
                Ok((_, true)) => {
                    self.replayed_arm_issue(&dev.id, command_id, proposal, plan_sha256)
                }
                Ok((_, false)) => {
                    Response::err("confirm_code refused: the store answered a replay as new")
                }
                Err(why) => Response::err(why),
            };
        }
        let found = match self.proposals.check_open(proposal, plan_sha256) {
            Ok(p) => p,
            Err(why) => return refusal("subject_moved", why),
        };
        let line = format!(
            "confirm_code {} proposal={} verb=arm plan={} spine={} command={}",
            field(&dev.id),
            proposal,
            plan_sha256,
            field(&found.plan.spine),
            command_id
        );
        let plan = Plan::Admit {
            effects: Vec::new(),
            subject: proposal.to_string(),
            line,
        };
        match self.run_command("confirm_code", command_id, content, plan) {
            Ok((_, true)) => self.replayed_arm_issue(&dev.id, command_id, proposal, plan_sha256),
            Ok((_, false)) => {
                let code = format!("{CODE_PREFIX}{}", hex(&bytes));
                let at = std::time::SystemTime::now()
                    .checked_add(self.cfg.confirm_ttl)
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .unwrap_or_default();
                let expires_at = pf_engine::engine::format_utc(at.as_secs(), at.subsec_millis());
                let answer =
                    arm_issued(&dev.id, &found, false, Some(code.clone()), Some(expires_at));
                // The first record a replay of this command ID gives back: no code, `replayed` true.
                let first = ConfirmIssued {
                    replayed: true,
                    code: None,
                    ..answer.clone()
                };
                self.confirm.issue(
                    &dev.id,
                    sha256_hex(code.as_bytes()),
                    Subject::Arm {
                        proposal: proposal.to_string(),
                        plan_sha256: plan_sha256.to_string(),
                    },
                    None,
                    self.cfg.confirm_ttl,
                    self.cfg.confirm_tombstone,
                    (command_id.to_string(), first),
                );
                Response::ok(serde_json::to_value(answer).unwrap_or(Value::Null))
            }
            Err(why) => Response::err(why),
        }
    }

    /// The answer to a replay of an arm `confirm_code`: the first record, with no code. Once the table
    /// dropped it, the facts the proposal table still has, with `expires_at` null.
    fn replayed_arm_issue(
        &self,
        device: &str,
        command_id: &str,
        proposal: &str,
        plan_sha256: &str,
    ) -> Response {
        if let Some(first) = self.confirm.first_answer(device, command_id) {
            return Response::ok(serde_json::to_value(first).unwrap_or(Value::Null));
        }
        let facts = self.proposals.get(proposal);
        let shown = match facts {
            Some(p) => arm_issued(device, &p, true, None, None),
            None => ConfirmIssued {
                schema_version: SCHEMA_VERSION,
                replayed: true,
                code: None,
                expires_at: None,
                device: device.to_string(),
                generation: None,
                holder: None,
                subject: ConfirmSubjectShown {
                    request: proposal.to_string(),
                    revision: 0,
                    kind: "start".to_string(),
                    run: String::new(),
                    unit: None,
                    reason: "start".to_string(),
                    pr: None,
                    base: None,
                    head_branch: None,
                    head_sha: None,
                },
                start: Some(StartShown {
                    proposal: proposal.to_string(),
                    session: String::new(),
                    project_id: String::new(),
                    spine: String::new(),
                    plan_sha256: plan_sha256.to_string(),
                }),
            },
        };
        Response::ok(serde_json::to_value(shown).unwrap_or(Value::Null))
    }

    // ------------------------------------------------------------------ arm from a device

    /// `arm` from a paired device: the proposal of a start card, the hash of its plan and a confirm
    /// code. The spend takes no claim. It checks the package snapshot before any of its code runs,
    /// builds the plan again from the snapshot, and arms through `Controller::arm_with`, which commits
    /// the `device_arm` admission inside the run registry's critical section.
    pub(super) fn device_arm(&self, device: &DeviceRecord, args: &Value) -> Response {
        let a: DeviceArmArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => {
                return Response::err(format!("arm refused: the arguments are not readable: {e}"));
            }
        };
        if let Err(why) = check_command_id(&a.command_id) {
            return Response::err(why);
        }
        let dev = match self.live_device(&device.id) {
            Ok(d) => d,
            Err(r) => return r,
        };
        let code_sha = sha256_hex(a.code.as_bytes());
        let content = json!({
            "verb": "arm",
            "proposal": a.proposal,
            "plan_sha256": a.plan_sha256,
            "code_sha256": code_sha,
            "device": dev.id,
        })
        .to_string();
        // A command the store has seen is decided by its record: a replay answers the first
        // record, and nothing arms again.
        if matches!(
            self.store.admission(CONTROLLER_SCOPE, &a.command_id),
            Ok(Some(_))
        ) {
            return match self.run_command(
                "device_arm",
                &a.command_id,
                content,
                Plan::Refuse(String::new()),
            ) {
                Ok((_, true)) => self.replayed_arm(&dev.id, &a.proposal),
                Ok((_, false)) => Response::err("arm refused: the store answered a replay as new"),
                Err(why) => refused_reason(&why),
            };
        }
        // The code: its words.
        let call = Subject::Arm {
            proposal: a.proposal.clone(),
            plan_sha256: a.plan_sha256.clone(),
        };
        if let Err(r) = self
            .confirm
            .take(&dev.id, &code_sha, &call, self.cfg.confirm_tombstone)
        {
            return self.arm_refused(&a, content, Some(r.word()), r.text().to_string());
        }
        // The proposal is open: `Open` becomes `Arming`, so a second spend sees it taken.
        let p = match self.proposals.begin_arming(&a.proposal, &a.plan_sha256) {
            Ok(p) => p,
            Err(why) => return self.arm_refused(&a, content, Some("subject_moved"), why),
        };
        // The snapshot, before any code of it runs.
        let snapshot = proposals_dir(&self.cfg.state_dir)
            .join(&p.id)
            .join("package");
        match pin_content(&snapshot) {
            Ok(now) if now == p.package_content => {}
            Ok(_) | Err(_) => {
                let cause = "the package snapshot changed";
                self.fail_proposal(&p.id, cause);
                return self.arm_refused(&a, content, Some("subject_moved"), cause.to_string());
            }
        }
        // The plan again, from the snapshot: its `plan_check` runs from the snapshot only.
        let prepared = match build_plan(&p.inputs, &snapshot).map_err(strip_arm_prefix) {
            Ok(prepared) => prepared,
            Err(why) => {
                self.fail_proposal(&p.id, &why);
                return self.arm_refused(&a, content, None, why);
            }
        };
        let summary = build_summary(&prepared);
        let again = plan_hash(&prepared.plan, &summary, &p.package_content);
        if again != p.plan_sha256 {
            let cause = format!(
                "the plan changed since the card was posted: {}",
                plan_diff(&p.summary, &summary)
            );
            self.fail_proposal(&p.id, &cause);
            return self.arm_refused(&a, content, Some("subject_moved"), cause);
        }
        let mut plan = prepared.plan;
        plan.project_id = Some(p.project_id.clone());
        plan.expect_package = Some(p.package_content.clone());
        let admit = || -> Result<(), String> {
            let line = format!(
                "device_arm {} proposal={} plan={} spine={} admitted command={}",
                field(&dev.id),
                p.id,
                p.plan_sha256,
                field(&p.plan.spine),
                a.command_id
            );
            self.run_command(
                "device_arm",
                &a.command_id,
                content.clone(),
                Plan::Admit {
                    effects: Vec::new(),
                    subject: p.id.clone(),
                    line,
                },
            )
            .map(|_| ())
        };
        match self.arm_with(plan, Some(&p.id), &admit) {
            Ok(run_id) => {
                // The proposal is `Armed` first. Then the card gets its result, and a failed
                // append is tried again at each tick.
                let ended = self.proposals.end(
                    &p.id,
                    ProposalState::Armed(run_id.clone()),
                    PendingResult {
                        result: StartResultKind::Armed,
                        reason: None,
                        run: Some(run_id.clone()),
                        device: Some(dev.id.clone()),
                        noted: false,
                    },
                );
                if ended {
                    self.remove_snapshot(&p.id);
                    self.flush_start_result(&p.id);
                }
                Response::ok(
                    serde_json::to_value(ArmActed {
                        schema_version: SCHEMA_VERSION,
                        replayed: false,
                        device: dev.id.clone(),
                        proposal: p.id.clone(),
                        run_id: Some(run_id.clone()),
                        project_id: self.run_project_of(&run_id),
                    })
                    .unwrap_or(Value::Null),
                )
            }
            Err(failure) => match failure.stage {
                // Nothing was admitted: a transient cause. The proposal is open again.
                ArmStage::Before => {
                    self.proposals.reopen(&p.id);
                    self.arm_refused(&a, content.clone(), None, failure.why)
                }
                ArmStage::AdmitFailed => {
                    self.proposals.reopen(&p.id);
                    Response::err(format!("arm refused: {}", failure.why))
                }
                // The admission is committed: the cause is permanent.
                ArmStage::After => {
                    let cause = failure.why;
                    self.fail_proposal(&p.id, &cause);
                    if cause == PACKAGE_MOVED {
                        refusal("subject_moved", format!("arm refused: {cause}"))
                    } else {
                        Response::err(format!("arm refused: {cause}"))
                    }
                }
            },
        }
    }

    /// A refusal of a device `arm` before it admitted: a refused admission with the cause, and one
    /// ledger line. The word, when it has one, rides in the answer.
    fn arm_refused(
        &self,
        a: &DeviceArmArgs,
        content: String,
        word: Option<&'static str>,
        text: String,
    ) -> Response {
        let text = if word.is_some() {
            text
        } else {
            format!("arm refused: {text}")
        };
        match self.run_command(
            "device_arm",
            &a.command_id,
            content,
            plan_refused((word, text)),
        ) {
            Err(reason) => refused_reason(&reason),
            Ok(_) => Response::err("arm refused: the store admitted a refusal"),
        }
    }

    /// The answer to a replay of a device `arm`: the run the proposal's state or its `run_project`
    /// admission names, or null.
    fn replayed_arm(&self, device: &str, proposal: &str) -> Response {
        let from_table = self.proposals.get(proposal).and_then(|p| match p.state {
            ProposalState::Armed(run) => Some(run),
            _ => None,
        });
        let run = from_table.or_else(|| {
            self.store
                .admissions(CONTROLLER_SCOPE)
                .ok()?
                .iter()
                .filter(|a| a.kind == "run_project" && a.outcome == AdmissionOutcome::Admitted)
                .filter_map(|a| serde_json::from_str::<Value>(&a.content).ok())
                .find(|c| c.get("proposal").and_then(Value::as_str) == Some(proposal))
                .and_then(|c| c.get("run").and_then(Value::as_str).map(str::to_string))
        });
        let project_id = run.as_deref().and_then(|r| self.run_project_of(r));
        Response::ok(
            serde_json::to_value(ArmActed {
                schema_version: SCHEMA_VERSION,
                replayed: true,
                device: device.to_string(),
                proposal: proposal.to_string(),
                run_id: run,
                project_id,
            })
            .unwrap_or(Value::Null),
        )
    }
}

/// The answer of an arm `confirm_code`: `subject.kind` is `start`, `request` is the proposal, and the
/// typed facts are in `start` (W9). No required field of a request code's answer changes shape.
fn arm_issued(
    device: &str,
    p: &Proposal,
    replayed: bool,
    code: Option<String>,
    expires_at: Option<String>,
) -> ConfirmIssued {
    ConfirmIssued {
        schema_version: SCHEMA_VERSION,
        replayed,
        code,
        expires_at,
        device: device.to_string(),
        generation: None,
        holder: None,
        subject: ConfirmSubjectShown {
            request: p.id.clone(),
            revision: 0,
            kind: "start".to_string(),
            run: String::new(),
            unit: None,
            reason: format!("start spine {}", p.plan.spine),
            pr: None,
            base: None,
            head_branch: None,
            head_sha: None,
        },
        start: Some(StartShown {
            proposal: p.id.clone(),
            session: p.session.clone(),
            project_id: p.project_id.clone(),
            spine: p.plan.spine.clone(),
            plan_sha256: p.plan_sha256.clone(),
        }),
    }
}
