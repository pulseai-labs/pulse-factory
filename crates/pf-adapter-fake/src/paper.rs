//! The paper adapter: a seat as a real process with no model and no PTY.
//!
//! A launch runs `<dir>/seats/<role>.sh` with the plan's `cwd` and `env.*`, plus
//! `PF_PAPER_EVENTS=<a file for the session>`. The script writes one JSON line for each event:
//!
//! - `{"usage":{"input_tokens":..,"output_tokens":..,"cache_read_tokens":..,"cache_write_tokens":..,"cost_micros":..}}`
//!   adds to the session's sum;
//! - `{"push":{"branch":"..","sha":".."}}` is a seat's push;
//! - `{"turn_end":true}` ends the turn.
//!
//! A line it cannot read is ignored and noted in `out/<unit>/paper-notes.log`. The process
//! ending is `Ended`. It spends nothing and reaches no network. The scripted `FakeAdapter` stays
//! for unit tests.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{
    AgentError, AgentPort, Dialog, DialogAnswer, LaunchPlan, ResolvedPlugin, RuleSet, SessionEvent,
    SessionRef, SessionState, SessionStatus, Usage,
};

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
    child: Child,
    pgid: u32,
    events_path: PathBuf,
    notes_path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    usage: Usage,
    queue: Vec<SessionEvent>,
    turn_ended: bool,
    push_seq: u64,
    repo: String,
    id: String,
    exited: bool,
}

pub struct PaperAdapter {
    cfg: PaperConfig,
    sessions: Mutex<HashMap<String, Arc<Mutex<PaperSession>>>>,
    seq: Mutex<u64>,
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
            self.turn_ended = true;
            self.queue.push(SessionEvent::Activity);
        } else {
            self.note(&format!(
                "{}: ignored a line that is no event: {shown}",
                self.id
            ));
        }
    }

    /// Read the whole lines that came since the last read.
    fn refresh(&mut self) {
        if !self.exited && matches!(self.child.try_wait(), Ok(Some(_)) | Err(_)) {
            self.exited = true;
        }
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
    }

    /// End the process group and wait for the leader. Safe to repeat.
    fn kill(&mut self) {
        let _ = Command::new("sh")
            .args(["-c", "kill -s KILL -- \"-$1\" 2>/dev/null; exit 0", "sh"])
            .arg(self.pgid.to_string())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.exited = true;
    }
}

impl PaperAdapter {
    pub fn new(cfg: PaperConfig) -> PaperAdapter {
        PaperAdapter {
            cfg,
            sessions: Mutex::new(HashMap::new()),
            seq: Mutex::new(0),
        }
    }

    fn find(&self, session: &SessionRef) -> Option<Arc<Mutex<PaperSession>>> {
        locked(&self.sessions).get(&session.0).cloned()
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
        let n = {
            let mut seq = locked(&self.seq);
            *seq += 1;
            *seq
        };
        let id = format!("paper-{n:04}");
        let run_dir = Some(setting("run_dir"))
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        let base = run_dir.clone().unwrap_or_else(|| self.cfg.dir.clone());
        let session_dir = base.join("paper");
        std::fs::create_dir_all(&session_dir)
            .map_err(|e| failed(format!("cannot make {}: {e}", session_dir.display())))?;
        let events_path = session_dir.join(format!("{id}.events"));
        std::fs::write(&events_path, b"")
            .map_err(|e| failed(format!("cannot write {}: {e}", events_path.display())))?;
        let notes_dir = run_dir.map_or_else(
            || session_dir.clone(),
            |d| d.join("out").join(&plan.unit_id),
        );

        let mut cmd = Command::new("/bin/sh");
        cmd.arg(&script)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        for (k, v) in &plan.settings {
            if let Some(name) = k.strip_prefix("env.") {
                cmd.env(name, v);
            }
        }
        cmd.env("PF_PAPER_EVENTS", &events_path);
        let child = cmd
            .spawn()
            .map_err(|e| failed(format!("cannot start {}: {e}", script.display())))?;
        let pgid = child.id();
        let session = PaperSession {
            child,
            pgid,
            events_path,
            notes_path: notes_dir.join("paper-notes.log"),
            offset: 0,
            partial: Vec::new(),
            usage: Usage::default(),
            queue: Vec::new(),
            turn_ended: false,
            push_seq: 0,
            repo: Some(setting("repo"))
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "paper".to_string()),
            id: id.clone(),
            exited: false,
        };
        locked(&self.sessions).insert(id.clone(), Arc::new(Mutex::new(session)));
        Ok(SessionRef(id))
    }

    fn status(&self, session: &SessionRef) -> Result<SessionStatus, AgentError> {
        let s = self
            .find(session)
            .ok_or_else(|| failed(format!("session {} is unknown", session.0)))?;
        let mut s = locked(&s);
        s.refresh();
        Ok(SessionStatus {
            state: if s.exited {
                SessionState::Ended
            } else if s.turn_ended {
                SessionState::Idle
            } else {
                SessionState::Working
            },
            turn_ended: s.turn_ended,
            first_turn_started: true,
        })
    }

    fn deliver(&self, _: &SessionRef, _: &str) -> Result<(), AgentError> {
        Err(failed("a paper seat takes no message"))
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

    fn hand_off(
        &self,
        session: &SessionRef,
        successor: &LaunchPlan,
    ) -> Result<SessionRef, AgentError> {
        let next = self.launch(successor)?;
        self.stop(session)?;
        Ok(next)
    }

    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError> {
        let Some(s) = self.find(session) else {
            return Ok(false);
        };
        let mut s = locked(&s);
        s.refresh();
        Ok(!s.exited)
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
