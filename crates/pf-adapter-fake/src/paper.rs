//! The paper adapter: a seat as a real process with no model and no PTY, run in a session host.
//!
//! A launch asks the controller's host service (`SessionHosts`) to start `/bin/sh <dir>/seats/<role>.sh`
//! in a host on pipes, with the plan's `cwd` and `env.*`, the controller's own values of a fixed list
//! of names, and `PF_PAPER_EVENTS=<a file for the session>`. The host clears the child's environment,
//! so nothing else reaches the script. The session reference is the host unit. The script writes
//! one JSON line for each event:
//!
//! - `{"usage":{"input_tokens":..,"output_tokens":..,"cache_read_tokens":..,"cache_write_tokens":..,"cost_micros":..}}`
//!   adds to the session's sum;
//! - `{"push":{"branch":"..","sha":".."}}` is a seat's push;
//! - `{"turn_end":true}` ends the turn.
//!
//! A launch also exports `PF_PAPER_INBOX=<a file for the session>`, which exists and is empty. A
//! message the controller delivers is appended to it as one JSON line, `{"message":"..."}`, and
//! starts another turn: the script can wait on the file, run the turn, and write `turn_end` again.
//! The turn is ended when the count of `turn_end` lines is at least 1 plus the count of inbox lines.
//!
//! The events and inbox files are named for the unit, `<run_dir>/paper/<unit>.{events,inbox}`, and a
//! push ID is `<unit>-push-<n>`: a unit never repeats, so two controller lifetimes never share a
//! file or an ID. After a controller restart, `rebind` attaches to the host again and reads the
//! events file from byte 0. Each effect is idempotent: usage is a running sum, and a push has a
//! stable ID.
//!
//! The script writes nothing to its standard output that the controller consumes, so the adapter
//! acknowledges host output as it reads it. When the host sends `exit`, the adapter reads the
//! events file to its end and sends `exit_ack`.
//!
//! A line it cannot read is ignored and noted in `out/<unit>/paper-notes.log`. The process
//! ending is `Ended`. It spends nothing and reaches no network. The scripted `FakeAdapter` stays
//! for unit tests.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{
    AgentError, AgentPort, Dialog, DialogAnswer, LaunchPlan, RebindOutcome, ResolvedPlugin,
    RuleSet, SessionEvent, SessionRef, SessionState, SessionStatus, Usage,
};
use pf_engine::port::host::{
    HostEvent, HostHandle, HostLaunch, HostTransport, Rebind, SessionHosts,
};
use serde_json::{Value, json};

/// The names the adapter takes from the controller's own environment. The host clears the child's
/// environment, so this list replaces the inheritance the seat had as a child of the controller.
const INHERITED_ENV: [&str; 13] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "PF_STATE_DIR",
    "PF_SHARE_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_TERMINAL_PROMPT",
];

/// The launch setting whose value is a secret: the seat's session key.
const SECRET_ENV: &str = "PF_SESSION_KEY";

/// What the controller gives the paper adapter once.
#[derive(Clone, Debug)]
pub struct PaperConfig {
    /// The paper env's directory: `seats/<role>.sh` lives here.
    pub dir: PathBuf,
    /// Directories that hold plugins, for `resolve_plugin`.
    pub plugin_roots: Vec<PathBuf>,
}

impl PaperConfig {
    pub fn new(dir: PathBuf, plugin_roots: Vec<PathBuf>) -> PaperConfig {
        PaperConfig { dir, plugin_roots }
    }
}

struct PaperSession {
    handle: HostHandle,
    events_path: PathBuf,
    inbox_path: PathBuf,
    notes_path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    usage: Usage,
    queue: Vec<SessionEvent>,
    /// The `turn_end` lines read so far.
    turn_ends: u64,
    push_seq: u64,
    repo: String,
    /// The host unit: the session reference, and the name of every file and push ID.
    id: String,
    /// The child ended, the host is gone, or the adapter stopped it.
    exited: bool,
    /// The exit was acknowledged, or there is nothing to acknowledge.
    exit_done: bool,
}

pub struct PaperAdapter {
    cfg: PaperConfig,
    hosts: Arc<dyn SessionHosts>,
    sessions: Mutex<HashMap<String, Arc<Mutex<PaperSession>>>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn failed(why: impl Into<String>) -> AgentError {
    AgentError::Failed(why.into())
}

fn refused(why: impl Into<String>) -> AgentError {
    AgentError::Refused(why.into())
}

impl PaperSession {
    fn note(&self, text: &str) {
        if let Some(dir) = self.notes_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.notes_path)
        {
            let _ = writeln!(f, "{text}");
        }
    }

    fn line(&mut self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let shown: String = trimmed.chars().take(200).collect();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            self.note(&format!(
                "{}: ignored a line that is not JSON: {shown}",
                self.id
            ));
            return;
        };
        let num = |u: &serde_json::Value, k: &str| {
            u.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0)
        };
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage.input_tokens += num(u, "input_tokens");
            self.usage.output_tokens += num(u, "output_tokens");
            self.usage.cache_read_tokens += num(u, "cache_read_tokens");
            self.usage.cache_write_tokens += num(u, "cache_write_tokens");
            self.usage.cost_micros += num(u, "cost_micros");
        } else if let Some(p) = v.get("push").filter(|p| p.is_object()) {
            let branch = p.get("branch").and_then(|b| b.as_str()).unwrap_or_default();
            let sha = p.get("sha").and_then(|b| b.as_str()).unwrap_or_default();
            if branch.is_empty() || sha.is_empty() {
                self.note(&format!(
                    "{}: ignored a push line with no branch or sha: {shown}",
                    self.id
                ));
                return;
            }
            self.push_seq += 1;
            self.queue.push(SessionEvent::Push {
                id: format!("{}-push-{}", self.id, self.push_seq),
                repo: self.repo.clone(),
                branch: branch.to_string(),
                sha: sha.to_string(),
                outcome: PushOutcome::Pushed,
            });
        } else if v.get("turn_end").and_then(serde_json::Value::as_bool) == Some(true) {
            self.turn_ends += 1;
            self.queue.push(SessionEvent::Activity);
        } else {
            self.note(&format!(
                "{}: ignored a line that is no event: {shown}",
                self.id
            ));
        }
    }

    /// Take what the host streams: acknowledge output as it comes, and see the exit.
    fn pump_host(&mut self) {
        if self.exited {
            return;
        }
        match self.handle.events(Duration::ZERO) {
            Ok(events) => {
                for event in events {
                    match event {
                        HostEvent::Out { at, data } => {
                            let _ = self.handle.ack(at + data.len() as u64);
                        }
                        HostEvent::Gap { to, .. } => {
                            let _ = self.handle.ack(to);
                        }
                        HostEvent::Exit(_) => self.exited = true,
                    }
                }
            }
            // The host is gone, or another controller took its place.
            Err(_) => {
                self.exited = true;
                self.exit_done = true;
            }
        }
    }

    /// Read the whole lines that came since the last read.
    fn refresh(&mut self) {
        self.pump_host();
        let Ok(mut f) = std::fs::File::open(&self.events_path) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        }
        self.offset += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);
        while let Some(i) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            self.line(&line[..line.len() - 1]);
        }
        // A last line without its newline counts once the process has ended.
        if self.exited && !self.partial.is_empty() {
            let rest = std::mem::take(&mut self.partial);
            self.line(&rest);
        }
        // The events file is read to its end: the host may end now.
        if self.exited && !self.exit_done {
            let _ = self.handle.exit_ack();
            self.exit_done = true;
        }
    }

    /// The lines of the inbox: the messages the controller delivered.
    fn inbox_lines(&self) -> u64 {
        std::fs::read(&self.inbox_path)
            .map(|b| b.iter().filter(|c| **c == b'\n').count() as u64)
            .unwrap_or(0)
    }

    /// The turn is ended when the `turn_end` lines number at least 1 plus the messages delivered.
    fn turn_ended(&self) -> bool {
        self.turn_ends > self.inbox_lines()
    }

    /// Stop the host and its child. Safe to repeat.
    fn kill(&mut self) {
        let _ = self.handle.stop();
        self.exited = true;
        self.exit_done = true;
    }
}

/// A string at `path` in the record, or empty.
fn record_text<'a>(record: &'a Value, path: &[&str]) -> &'a str {
    let mut v = record;
    for key in path {
        v = &v[*key];
    }
    v.as_str().unwrap_or_default()
}

impl PaperAdapter {
    pub fn new(cfg: PaperConfig, hosts: Arc<dyn SessionHosts>) -> PaperAdapter {
        PaperAdapter {
            cfg,
            hosts,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn find(&self, session: &SessionRef) -> Option<Arc<Mutex<PaperSession>>> {
        locked(&self.sessions).get(&session.0).cloned()
    }

    /// A session over an attached host. The events file is read from byte 0.
    fn adopt(
        &self,
        handle: HostHandle,
        events: PathBuf,
        inbox: PathBuf,
        notes: PathBuf,
        repo: String,
    ) -> Result<SessionRef, AgentError> {
        handle
            .read(0)
            .map_err(|e| failed(format!("cannot read the output of {}: {e}", handle.unit)))?;
        let id = handle.unit.clone();
        let session = PaperSession {
            exited: handle.exit.is_some(),
            exit_done: false,
            handle,
            events_path: events,
            inbox_path: inbox,
            notes_path: notes,
            offset: 0,
            partial: Vec::new(),
            usage: Usage::default(),
            queue: Vec::new(),
            turn_ends: 0,
            push_seq: 0,
            repo,
            id: id.clone(),
        };
        let mut session = session;
        session.refresh();
        locked(&self.sessions).insert(id.clone(), Arc::new(Mutex::new(session)));
        Ok(SessionRef(id))
    }
}

impl AgentPort for PaperAdapter {
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        let script = self.cfg.dir.join("seats").join(format!("{}.sh", plan.role));
        if !script.is_file() {
            return Err(refused(format!(
                "the paper env has no seat script {}",
                script.display()
            )));
        }
        let setting = |k: &str| plan.settings.get(k).cloned().unwrap_or_default();
        let cwd = Some(setting("cwd"))
            .filter(|c| !c.is_empty())
            .map_or_else(|| self.cfg.dir.clone(), PathBuf::from);
        let run_dir = Some(setting("run_dir"))
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        let base = run_dir.clone().unwrap_or_else(|| self.cfg.dir.clone());
        let session_dir = base.join("paper");
        std::fs::create_dir_all(&session_dir)
            .map_err(|e| failed(format!("cannot make {}: {e}", session_dir.display())))?;
        // The unit is not known before the host starts: the service replaces `{unit}`, and makes
        // both files before the child can look for them.
        let events_tpl = session_dir.join("{unit}.events").display().to_string();
        let inbox_tpl = session_dir.join("{unit}.inbox").display().to_string();
        let notes_dir = run_dir.map_or_else(
            || session_dir.clone(),
            |d| d.join("out").join(&plan.unit_id),
        );

        // The host clears the child's environment: the controller's values of the fixed list come
        // first, then the plan's own, then the paper files.
        let mut env: BTreeMap<String, String> = INHERITED_ENV
            .iter()
            .filter_map(|n| std::env::var(n).ok().map(|v| ((*n).to_string(), v)))
            .collect();
        let mut secrets = BTreeMap::new();
        for (k, v) in &plan.settings {
            if let Some(name) = k.strip_prefix("env.") {
                if name == SECRET_ENV {
                    secrets.insert(name.to_string(), v.clone());
                } else {
                    env.insert(name.to_string(), v.clone());
                }
            }
        }
        env.insert("PF_PAPER_EVENTS".to_string(), events_tpl.clone());
        env.insert("PF_PAPER_INBOX".to_string(), inbox_tpl.clone());

        let launch = HostLaunch {
            session_kind: "seat".to_string(),
            session_key: format!("{}/{}", plan.run_id, plan.unit_id),
            argv: vec!["/bin/sh".to_string(), script.display().to_string()],
            cwd: cwd.display().to_string(),
            env,
            files: vec![events_tpl.clone(), inbox_tpl.clone()],
            secrets,
            transport: HostTransport::Pipes,
            runtime: json!({ "id": "paper" }),
            settings: serde_json::to_value(&plan.settings).unwrap_or(Value::Null),
            run_binding: json!({
                "run_id": plan.run_id,
                "unit_id": plan.unit_id,
                "stage": plan.stage,
                "role": plan.role,
                "model": plan.model,
                "attempt": plan.attempt,
            }),
            resume: json!({ "kind": "none" }),
        };
        let handle = self
            .hosts
            .launch(launch)
            .map_err(|e| failed(format!("cannot start the seat in a host: {e}")))?;
        let named = |tpl: &str| PathBuf::from(tpl.replace("{unit}", &handle.unit));
        let (events, inbox) = (named(&events_tpl), named(&inbox_tpl));
        self.adopt(
            handle,
            events,
            inbox,
            notes_dir.join("paper-notes.log"),
            Some(setting("repo"))
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "paper".to_string()),
        )
    }

    fn status(&self, session: &SessionRef) -> Result<SessionStatus, AgentError> {
        let s = self
            .find(session)
            .ok_or_else(|| failed(format!("session {} is unknown", session.0)))?;
        let mut s = locked(&s);
        s.refresh();
        let turn_ended = s.turn_ended();
        Ok(SessionStatus {
            state: if s.exited {
                SessionState::Ended
            } else if turn_ended {
                SessionState::Idle
            } else {
                SessionState::Working
            },
            turn_ended,
            first_turn_started: true,
        })
    }

    fn deliver(&self, session: &SessionRef, message: &str) -> Result<(), AgentError> {
        let s = self
            .find(session)
            .ok_or_else(|| failed(format!("session {} is unknown", session.0)))?;
        let mut s = locked(&s);
        // Read what the script wrote first: a process that ended on its own is seen as ended.
        s.refresh();
        if s.exited {
            return Err(failed(format!(
                "session {} has ended; a message reaches a live session only",
                session.0
            )));
        }
        let line = serde_json::json!({ "message": message }).to_string();
        let mut inbox = std::fs::OpenOptions::new()
            .append(true)
            .open(&s.inbox_path)
            .map_err(|e| failed(format!("cannot open {}: {e}", s.inbox_path.display())))?;
        // The message starts a turn: the turn counts as ended only when the script writes
        // another `turn_end`.
        writeln!(inbox, "{line}")
            .map_err(|e| failed(format!("cannot write {}: {e}", s.inbox_path.display())))?;
        Ok(())
    }

    fn interrupt(&self, session: &SessionRef) -> Result<(), AgentError> {
        self.stop(session)
    }

    fn usage(&self, session: &SessionRef) -> Result<Usage, AgentError> {
        let s = self
            .find(session)
            .ok_or_else(|| failed(format!("session {} is unknown", session.0)))?;
        let mut s = locked(&s);
        s.refresh();
        Ok(s.usage)
    }

    fn answer_dialog(&self, _: &SessionRef, _: &Dialog) -> Result<DialogAnswer, AgentError> {
        Ok(DialogAnswer::Unknown)
    }

    fn apply_rules(&self, _: &mut LaunchPlan, _: &RuleSet) -> Result<(), AgentError> {
        Ok(())
    }

    fn remove_ask_tool(&self, _: &mut LaunchPlan) -> Result<(), AgentError> {
        Ok(())
    }

    /// The old host stops first, then the successor starts (Q9): no two copies run.
    fn hand_off(
        &self,
        session: &SessionRef,
        successor: &LaunchPlan,
    ) -> Result<SessionRef, AgentError> {
        self.stop(session)?;
        self.launch(successor)
    }

    /// The host's child PID, from its `hello`: the caller check of ADR-0005 reads it.
    fn process_id(&self, session: &SessionRef) -> Option<u32> {
        let s = self.find(session)?;
        let s = locked(&s);
        (!s.exited).then_some(s.handle.child_pid)
    }

    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError> {
        let Some(s) = self.find(session) else {
            return Ok(false);
        };
        let mut s = locked(&s);
        s.refresh();
        Ok(!s.exited)
    }

    /// Attach to the unit's host again and rebuild the state from the launch record and the events
    /// file, which is read from byte 0.
    fn rebind(&self, session: &SessionRef) -> Result<RebindOutcome, AgentError> {
        match self.hosts.rebind(&session.0) {
            Rebind::Lost { exit } => {
                locked(&self.sessions).remove(&session.0);
                let cause = exit
                    .as_str()
                    .map_or_else(|| exit.to_string(), str::to_string);
                Ok(RebindOutcome::Lost { cause })
            }
            Rebind::Unreachable { why } => Ok(RebindOutcome::Unreachable { why }),
            Rebind::Survived(handle) => {
                let record = &handle.record;
                let events = record_text(record, &["env", "PF_PAPER_EVENTS"]);
                let inbox = record_text(record, &["env", "PF_PAPER_INBOX"]);
                if events.is_empty() || inbox.is_empty() {
                    return Ok(RebindOutcome::Unreachable {
                        why: format!("the launch record of {} names no paper files", handle.unit),
                    });
                }
                let run_dir = record_text(record, &["settings", "run_dir"]);
                let unit_id = record_text(record, &["run_binding", "unit_id"]);
                let session_dir = Path::new(events)
                    .parent()
                    .map_or_else(PathBuf::new, Path::to_path_buf);
                let notes_dir = if run_dir.is_empty() {
                    session_dir
                } else {
                    Path::new(run_dir).join("out").join(unit_id)
                };
                let repo = Some(record_text(record, &["settings", "repo"]))
                    .filter(|r| !r.is_empty())
                    .unwrap_or("paper")
                    .to_string();
                let (events, inbox) = (PathBuf::from(events), PathBuf::from(inbox));
                self.adopt(
                    handle,
                    events,
                    inbox,
                    notes_dir.join("paper-notes.log"),
                    repo,
                )?;
                Ok(RebindOutcome::Survived)
            }
        }
    }

    fn stop(&self, session: &SessionRef) -> Result<(), AgentError> {
        if let Some(s) = self.find(session) {
            let mut s = locked(&s);
            s.kill();
            s.refresh();
        }
        Ok(())
    }

    fn resolve_plugin(&self, name: &str, min_version: &str) -> Result<ResolvedPlugin, AgentError> {
        resolve(&self.cfg.plugin_roots, name, min_version)
    }

    fn drain_events(&self, session: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        let s = self
            .find(session)
            .ok_or_else(|| failed(format!("session {} is unknown", session.0)))?;
        let mut s = locked(&s);
        s.refresh();
        Ok(std::mem::take(&mut s.queue))
    }
}

// ------------------------------------------------------------------ plugins

fn version_at_least(version: &str, minimum: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    let (a, b) = (parse(version), parse(minimum));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    true
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

fn manifest_version(dir: &Path) -> Option<String> {
    for rel in [".claude-plugin/plugin.json", "package.json", "plugin.json"] {
        let Ok(text) = std::fs::read_to_string(dir.join(rel)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
            return Some(ver.to_string());
        }
    }
    None
}

/// A plugin is `<root>/<name>`, `<root>/<name>/<version>` or `<root>/<market>/<name>/<version>`.
fn candidates(root: &Path, name: &str) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut add = |dir: PathBuf| {
        if let Some(v) = manifest_version(&dir) {
            out.push((v, dir.clone()));
        }
        for sub in subdirs(&dir) {
            if sub
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            if let Some(v) = manifest_version(&sub) {
                out.push((v, sub));
            }
        }
    };
    let direct = root.join(name);
    if direct.is_dir() {
        add(direct);
    }
    for market in subdirs(root) {
        let nested = market.join(name);
        if nested.is_dir() {
            add(nested);
        }
    }
    out
}

fn resolve(roots: &[PathBuf], name: &str, min_version: &str) -> Result<ResolvedPlugin, AgentError> {
    let mut best: Option<(String, PathBuf)> = None;
    let mut lower: Option<String> = None;
    for root in roots {
        for (version, path) in candidates(root, name) {
            if !version_at_least(&version, min_version) {
                lower = Some(version);
            } else if best
                .as_ref()
                .is_none_or(|(b, _)| version_at_least(&version, b))
            {
                best = Some((version, path));
            }
        }
    }
    match (best, lower) {
        (Some((version, path)), _) => Ok(ResolvedPlugin {
            name: name.to_string(),
            version,
            path: path.display().to_string(),
        }),
        (None, Some(v)) => Err(refused(format!(
            "plugin {name} is at version {v}, below the minimum {min_version}"
        ))),
        (None, None) => Err(refused(format!(
            "plugin {name} was not found in any plugin directory"
        ))),
    }
}
