//! `SeatPort`: the run facts and the rules (Approach 6).
//!
//! `SeatPort` implements `AgentPort` over the run's adapters. Its `launch` and `hand_off` add the
//! run's facts to the plan, make a fresh session key, and enter the seat in the key table. The
//! `hook` verb looks the key up and calls `pf_rules::decide`. The key table is in memory: it dies
//! with the process, so a seat that outlives its controller is denied everything.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pf_engine::port::agent::*;
use pf_rules::{Context, OwnPaths, ToolCall, audit_line, decide, is_write_tool};
use pf_workflow::commands::run_argv;
use pf_workflow::rules::RoleRules;
use serde_json::{Value, json};

/// A fixed text for the key in an audit line or a reason.
const KEY_MASK: &str = "[redacted]";

/// What a seat is, as the hook needs to judge its calls.
#[derive(Clone)]
struct SeatEntry {
    rules: RoleRules,
    context: Context,
    /// The seat's worktree, for the branch check. Empty for a lane.
    worktree: PathBuf,
    audit_log: PathBuf,
}

/// The keys of the live seats.
#[derive(Default)]
pub struct SeatTable {
    seats: Mutex<HashMap<String, SeatEntry>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn deny(reason: &str) -> Value {
    json!({ "decision": "deny", "reason": reason })
}

impl SeatTable {
    fn insert(&self, key: &str, entry: SeatEntry) {
        locked(&self.seats).insert(key.to_string(), entry);
    }

    fn remove(&self, key: &str) {
        locked(&self.seats).remove(key);
    }

    pub fn len(&self) -> usize {
        locked(&self.seats).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Answer one hook call: `{key, tool, command?, path?}`. It never waits on a tick. Any
    /// trouble is a deny.
    pub fn answer(&self, args: &Value) -> Value {
        let key = args.get("key").and_then(Value::as_str).unwrap_or_default();
        if key.is_empty() {
            return deny("the call carries no session key");
        }
        let Some(entry) = locked(&self.seats).get(key).cloned() else {
            return deny("the session key is unknown to this controller");
        };
        let tool = args.get("tool").and_then(Value::as_str).unwrap_or_default();
        if tool.is_empty() {
            return deny("the call names no tool");
        }
        let call = ToolCall {
            tool: tool.to_string(),
            command: args
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_string),
            path: args.get("path").and_then(Value::as_str).map(str::to_string),
        };
        if call.command.is_none() && call.path.is_none() && is_write_tool(tool) {
            return deny("a write tool with no path is denied, because the rules cannot judge it");
        }
        let mut context = entry.context.clone();
        context.current_branch = current_branch(&entry.worktree);
        let decision = decide(&entry.rules, &context, &call);
        let line = audit_line(&context, &call, &decision).replace(key, KEY_MASK);
        append_line(&entry.audit_log, &line);
        json!({
            "decision": decision.outcome.as_str(),
            "reason": decision.reason.replace(key, KEY_MASK),
            "rule": decision.rule,
        })
    }
}

fn append_line(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// `git -C <worktree> symbolic-ref --short HEAD`, or empty for a seat with no worktree.
fn current_branch(worktree: &Path) -> String {
    if worktree.as_os_str().is_empty() {
        return String::new();
    }
    let argv: Vec<String> = [
        "git",
        "-C",
        &worktree.display().to_string(),
        "symbolic-ref",
        "--short",
        "HEAD",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    match run_argv(&argv, None, 10) {
        Ok(out) if out.exit_code == Some(0) => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    }
}

/// Quote a path for `/bin/sh`.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn fresh_key() -> String {
    let mut bytes = [0u8; 24];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    let mut key = String::from("pfk_");
    for b in bytes {
        key.push_str(&format!("{b:02x}"));
    }
    key
}

/// What a run gives its seats.
#[derive(Clone)]
pub struct SeatFacts {
    pub repo: String,
    pub remote: String,
    pub default_branch: String,
    pub forge_api_hosts: String,
    pub run_dir: PathBuf,
    pub state_dir: PathBuf,
    pub target_dir: String,
    /// The pinned copies of the plugins, in order.
    pub plugins: Vec<PathBuf>,
    /// The rule file of each role, in the run's `package/`, and its parsed rules.
    pub rules: BTreeMap<String, (PathBuf, RoleRules)>,
    /// The binary's canonical path.
    pub binary: PathBuf,
}

pub struct SeatPort {
    inner: Arc<dyn AgentPort>,
    facts: SeatFacts,
    table: Arc<SeatTable>,
}

fn refused(why: impl Into<String>) -> AgentError {
    AgentError::Refused(why.into())
}

impl SeatPort {
    pub fn new(inner: Arc<dyn AgentPort>, facts: SeatFacts, table: Arc<SeatTable>) -> SeatPort {
        SeatPort {
            inner,
            facts,
            table,
        }
    }

    /// Add the run's facts to a plan. Nothing starts if a fact is missing.
    fn prepare(&self, plan: &LaunchPlan) -> Result<(LaunchPlan, String, SeatEntry), AgentError> {
        let f = &self.facts;
        let mut plan = plan.clone();
        let get = |p: &LaunchPlan, k: &str| p.settings.get(k).cloned().unwrap_or_default();
        let (rules_file, rules) =
            f.rules.get(&plan.role).cloned().ok_or_else(|| {
                refused(format!("role {} has no rule file in the run", plan.role))
            })?;
        let unit_dir = f.run_dir.join("out").join(&plan.unit_id);
        let scratch = unit_dir.join("scratch");
        let report_dir = get(&plan, "report_dir");
        let facts: [(&str, String); 8] = [
            ("repo", f.repo.clone()),
            ("remote", f.remote.clone()),
            ("default_branch", f.default_branch.clone()),
            ("forge_api_hosts", f.forge_api_hosts.clone()),
            ("rules_file", rules_file.display().to_string()),
            ("scratch_dir", scratch.display().to_string()),
            ("run_dir", f.run_dir.display().to_string()),
            ("report_dir", report_dir.clone()),
        ];
        for (name, value) in &facts {
            if value.trim().is_empty() {
                return Err(refused(format!(
                    "the launch setting {name} is empty, so no seat starts"
                )));
            }
        }
        let push_own = rules
            .allow
            .contains(&pf_workflow::rules::Allow::PushOwnBranch);
        if push_own && (get(&plan, "push_dir").is_empty() || get(&plan, "push_branch").is_empty()) {
            return Err(refused(format!(
                "role {} may push its own branch, and the stage sets no push_dir or push_branch",
                plan.role
            )));
        }
        std::fs::create_dir_all(&scratch)
            .map_err(|e| AgentError::Failed(format!("cannot make {}: {e}", scratch.display())))?;
        for (name, value) in facts {
            plan.settings.insert(name.to_string(), value);
        }
        plan.settings.insert(
            "plugins".to_string(),
            f.plugins
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(","),
        );
        plan.settings
            .insert("target_dir".to_string(), f.target_dir.clone());
        // These may be empty: a lane has no worktree, and no unit has a tag in this spine.
        for name in ["worktree", "own_branch", "own_tag"] {
            let v = get(&plan, name);
            plan.settings.insert(name.to_string(), v);
        }
        let key = fresh_key();
        let hook = format!("{} hook", shell_quote(&f.binary.display().to_string()));
        plan.settings.insert("env.PF_HOOK_CMD".to_string(), hook);
        plan.settings
            .insert("env.PF_SESSION_KEY".to_string(), key.clone());
        plan.settings.insert(
            "env.PF_STATE_DIR".to_string(),
            f.state_dir.display().to_string(),
        );

        let worktree = PathBuf::from(get(&plan, "worktree"));
        let own_tag = Some(get(&plan, "own_tag")).filter(|t| !t.is_empty());
        let context = Context {
            seat: format!("{}/{}", plan.role, plan.unit_id),
            current_branch: String::new(),
            own_branch: get(&plan, "own_branch"),
            own_tag,
            default_branch: f.default_branch.clone(),
            api_hosts: f
                .forge_api_hosts
                .split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect(),
            cwd: PathBuf::from(get(&plan, "cwd")),
            push_dir: PathBuf::from(get(&plan, "push_dir")),
            paths: OwnPaths {
                scratch,
                build_target: PathBuf::from(&f.target_dir),
                worktree: worktree.clone(),
                report_and_run: vec![PathBuf::from(&report_dir), f.run_dir.clone()],
            },
        };
        let entry = SeatEntry {
            rules,
            context,
            worktree,
            audit_log: unit_dir.join("audit.log"),
        };
        Ok((plan, key, entry))
    }
}

impl AgentPort for SeatPort {
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        let (plan, key, entry) = self.prepare(plan)?;
        // The seat may call the hook as soon as it starts, so the key is in the table first.
        self.table.insert(&key, entry);
        match self.inner.launch(&plan) {
            Ok(s) => Ok(s),
            Err(e) => {
                self.table.remove(&key);
                Err(e)
            }
        }
    }

    fn hand_off(
        &self,
        session: &SessionRef,
        successor: &LaunchPlan,
    ) -> Result<SessionRef, AgentError> {
        let (plan, key, entry) = self.prepare(successor)?;
        self.table.insert(&key, entry);
        match self.inner.hand_off(session, &plan) {
            Ok(s) => Ok(s),
            Err(e) => {
                self.table.remove(&key);
                Err(e)
            }
        }
    }

    fn status(&self, s: &SessionRef) -> Result<SessionStatus, AgentError> {
        self.inner.status(s)
    }
    fn deliver(&self, s: &SessionRef, m: &str) -> Result<(), AgentError> {
        self.inner.deliver(s, m)
    }
    fn interrupt(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.interrupt(s)
    }
    fn usage(&self, s: &SessionRef) -> Result<Usage, AgentError> {
        self.inner.usage(s)
    }
    fn answer_dialog(&self, s: &SessionRef, d: &Dialog) -> Result<DialogAnswer, AgentError> {
        self.inner.answer_dialog(s, d)
    }
    fn apply_rules(&self, p: &mut LaunchPlan, r: &RuleSet) -> Result<(), AgentError> {
        self.inner.apply_rules(p, r)
    }
    fn remove_ask_tool(&self, p: &mut LaunchPlan) -> Result<(), AgentError> {
        self.inner.remove_ask_tool(p)
    }
    fn is_alive(&self, s: &SessionRef) -> Result<bool, AgentError> {
        self.inner.is_alive(s)
    }
    fn stop(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.stop(s)
    }
    fn resolve_plugin(&self, n: &str, v: &str) -> Result<ResolvedPlugin, AgentError> {
        self.inner.resolve_plugin(n, v)
    }
    fn drain_events(&self, s: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        self.inner.drain_events(s)
    }
}
