//! `pf-adapter-omp`: the first agent adapter. It launches OMP as an interactive session in a PTY
//! and reads its turn ends, usage, pushes and stalls from events that Pulse Factory's own OMP
//! extension writes (ADR-0011). The contract for any adapter is `docs/contracts/agent-adapter.md`.

pub mod argv;
pub mod events;
pub mod git;
pub mod isolation;
pub mod plugin;
pub mod probe;
pub mod pty;
pub mod screen;
pub mod token;
pub mod version;

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{
    AgentError, AgentPort, Dialog, DialogAnswer, LaunchPlan, ResolvedPlugin, RuleSet, SessionEvent,
    SessionRef, SessionState, SessionStatus, Usage,
};
use pf_engine::status::{REASON_MAX_CHARS, REDACTED, one_line_reason, redact};
use pf_rules::{Part, push_of};

use crate::events::{Ev, parse_line};
use crate::pty::{PtySession, SpawnSpec};

/// Pulse Factory's own OMP extension. The adapter writes it to its state directory at launch.
pub const EXTENSION_SOURCE: &str = include_str!("../extension/pf-seat.ts");

/// How many characters of the screen go into a failure reason (ADR-0006: a reason holds 500).
pub const SCREEN_TAIL_CHARS: usize = 450;

/// The length of `launch failed: `, which the engine puts before an `AgentError::Failed` reason.
const ENGINE_FAILED_PREFIX_CHARS: usize = "launch failed: ".len();

const DEFAULT_FIRST_TURN_SECONDS: u64 = 120;
const DEFAULT_STALL_MINUTES: u64 = 20;

/// Environment names that only the adapter sets.
const RESERVED_ENV: [&str; 7] = [
    "PI_EDIT_VARIANT",
    "PF_ALLOWED_TOOLS",
    "PF_SEAT_TOKEN",
    "PF_EVENT_FILE",
    "PF_INBOX_FILE",
    "PF_SESSION_DIR",
    "PF_SEAT_ROLE",
];

/// A source of the time, in milliseconds since the Unix epoch.
pub type ClockMs = Arc<dyn Fn() -> u64 + Send + Sync>;

fn system_clock_ms() -> ClockMs {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    })
}

/// What the controller gives the adapter once.
#[derive(Clone)]
pub struct OmpConfig {
    /// The `omp` program.
    pub omp_bin: PathBuf,
    /// Where the adapter keeps one directory for each session, and its extension file.
    pub state_dir: PathBuf,
    /// The OMP versions this adapter accepts.
    pub qualified_versions: Vec<String>,
    /// The repo that a push event names. A plan can override it with the setting `repo`.
    pub repo: String,
    /// The directory that a brief path is relative to (the workflow package).
    pub brief_root: Option<PathBuf>,
    /// Directories that hold plugins, for `resolve_plugin`.
    pub plugin_roots: Vec<PathBuf>,
    pub rows: u16,
    pub cols: u16,
    pub clock_ms: ClockMs,
    /// Refuse a launch when MCP config would reach the seat (see [`isolation`]). On by default.
    /// Only the probes that observe the runtime guard turn it off.
    pub isolation_check: bool,
}

impl OmpConfig {
    pub fn new(state_dir: impl Into<PathBuf>) -> OmpConfig {
        OmpConfig {
            omp_bin: PathBuf::from("omp"),
            state_dir: state_dir.into(),
            qualified_versions: version::QUALIFIED_VERSIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            repo: String::new(),
            brief_root: None,
            plugin_roots: Vec::new(),
            rows: 40,
            cols: 140,
            clock_ms: system_clock_ms(),
            isolation_check: true,
        }
    }
}

/// The default branch of the remote, read at launch.
#[derive(Clone, Debug)]
struct Baseline {
    remote: String,
    branch: String,
    sha: String,
    dir: PathBuf,
}

struct Session {
    id: String,
    dir: PathBuf,
    events_path: PathBuf,
    inbox_path: PathBuf,
    log_path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    pty: PtySession,
    token: String,
    state: SessionState,
    turn_ended: bool,
    first_turn_started: bool,
    usage: Usage,
    last_activity_ms: u64,
    stall_reported: bool,
    stall_limit_ms: u64,
    repo: String,
    cwd: PathBuf,
    push_dir: Option<PathBuf>,
    push_branch: Option<String>,
    /// The `remote` setting: the remote that the push measurement reads.
    remote: String,
    /// The remote-tracking refs of `remote` at the last read, by branch name.
    refs: BTreeMap<String, String>,
    baseline: Option<Baseline>,
    queue: Vec<SessionEvent>,
    push_seq: u64,
    inbox_seq: u64,
    /// Tools the seat holds that are not on the seat's tool list.
    unexpected_tools: Vec<String>,
    /// The skill globs the plan declares.
    declared_skills: Vec<String>,
    /// Skills the seat holds that no declared glob matches.
    unexpected_skills: Vec<String>,
}

/// The OMP adapter.
pub struct OmpAdapter {
    cfg: OmpConfig,
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
    seq: AtomicU64,
}

fn setting<'a>(plan: &'a LaunchPlan, key: &str) -> Option<&'a str> {
    plan.settings
        .get(key)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
}

fn failed(why: impl Into<String>) -> AgentError {
    AgentError::Failed(why.into())
}

fn refused(why: impl Into<String>) -> AgentError {
    AgentError::Refused(why.into())
}

impl OmpAdapter {
    pub fn new(cfg: OmpConfig) -> OmpAdapter {
        OmpAdapter {
            cfg,
            sessions: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        }
    }

    /// The file where the extension writes the events of a session.
    pub fn event_file(&self, session: &SessionRef) -> Option<PathBuf> {
        self.find(session).ok().map(|s| {
            s.lock()
                .unwrap_or_else(|e| e.into_inner())
                .events_path
                .clone()
        })
    }

    /// The file where the adapter puts messages for the extension to deliver.
    pub fn inbox_file(&self, session: &SessionRef) -> Option<PathBuf> {
        self.find(session).ok().map(|s| {
            s.lock()
                .unwrap_or_else(|e| e.into_inner())
                .inbox_path
                .clone()
        })
    }

    /// The adapter's own log of a session (lines it skipped).
    pub fn log_file(&self, session: &SessionRef) -> Option<PathBuf> {
        self.find(session)
            .ok()
            .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).log_path.clone())
    }

    /// The process ID of a session's OMP.
    pub fn pid(&self, session: &SessionRef) -> Option<u32> {
        self.find(session)
            .ok()
            .and_then(|s| s.lock().unwrap_or_else(|e| e.into_inner()).pty.pid())
    }

    /// The screen of a session, redacted and flattened, last `max_chars` characters. Reason text
    /// only: never use it to detect a state.
    pub fn screen_tail(&self, session: &SessionRef, max_chars: usize) -> Option<String> {
        let s = self.find(session).ok()?;
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        Some(redacted_tail(&s.pty.screen(), &s.token, max_chars))
    }

    /// Write bytes to the session's terminal.
    pub fn write_terminal(&self, session: &SessionRef, bytes: &[u8]) -> Result<(), AgentError> {
        let s = self.find(session)?;
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.pty.write(bytes).map_err(failed)
    }

    fn find(&self, session: &SessionRef) -> Result<Arc<Mutex<Session>>, AgentError> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session.0)
            .cloned()
            .ok_or_else(|| failed(format!("session {} is unknown", session.0)))
    }

    fn installed_version(&self) -> Result<String, AgentError> {
        let out = retry_busy(|| {
            std::process::Command::new(&self.cfg.omp_bin)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .output()
        })
        .map_err(|e| {
            failed(format!(
                "cannot run {} --version: {e}",
                self.cfg.omp_bin.display()
            ))
        })?;
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        version::parse_version(&text).ok_or_else(|| {
            refused(format!(
                "cannot read the OMP version from {:?}",
                text.trim()
            ))
        })
    }

    fn write_extension(&self) -> Result<PathBuf, AgentError> {
        let dir = self.cfg.state_dir.join("extension");
        std::fs::create_dir_all(&dir).map_err(|e| failed(format!("cannot make {dir:?}: {e}")))?;
        let path = dir.join("pf-seat.ts");
        let same = std::fs::read_to_string(&path).is_ok_and(|t| t == EXTENSION_SOURCE);
        if !same {
            std::fs::write(&path, EXTENSION_SOURCE)
                .map_err(|e| failed(format!("cannot write {path:?}: {e}")))?;
        }
        Ok(path)
    }
}

/// Does a skill name match a glob? `*` matches any run of characters. Nothing else is special.
fn glob_matches(glob: &str, name: &str) -> bool {
    let parts: Vec<&str> = glob.split('*').collect();
    if parts.len() == 1 {
        return glob == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name.ends_with(last) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// Run a call again, a few times, when the system says the program file is busy (ETXTBSY).
fn retry_busy<T>(mut call: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut result = call();
    for _ in 0..5 {
        match &result {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(Duration::from_millis(30));
                result = call();
            }
            _ => break,
        }
    }
    result
}

/// Collapse, redact (also the session's own token) and cut the end of a screen.
fn redacted_tail(screen: &str, token: &str, max_chars: usize) -> String {
    let mut text = redact(screen);
    if !token.is_empty() {
        text = text.replace(token, REDACTED);
    }
    screen::tail(&text, max_chars)
}

impl Session {
    fn log(&mut self, line: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    fn activity(&mut self, time_ms: Option<u64>, now: u64) {
        let t = time_ms.unwrap_or(now);
        self.last_activity_ms = self.last_activity_ms.max(t);
        self.stall_reported = false;
        if !self.queue.contains(&SessionEvent::Activity) {
            self.queue.push(SessionEvent::Activity);
        }
    }

    /// Read the lines that came since the last read, and update the state from them.
    fn refresh(&mut self, now: u64) {
        if let Ok(mut f) = std::fs::File::open(&self.events_path)
            && f.seek(SeekFrom::Start(self.offset)).is_ok()
        {
            let mut buf = Vec::new();
            if let Ok(n) = f.read_to_end(&mut buf) {
                self.offset += n as u64;
                self.partial.extend_from_slice(&buf);
            }
        }
        while let Some(pos) = self.partial.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.partial.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw[..raw.len() - 1]).to_string();
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(&line) {
                Ok(l) => self.apply(l, now),
                Err(why) => {
                    let shown: String = redact(&line).chars().take(200).collect();
                    self.log(&format!("skipped an event line ({why}): {shown}"));
                }
            }
        }
        if self.pty.exit_code().is_some() {
            self.state = SessionState::Ended;
        }
    }

    fn apply(&mut self, line: events::Line, now: u64) {
        match line.ev {
            Ev::SessionStart | Ev::ToolActivity | Ev::Deny | Ev::Delivered => {
                self.activity(line.time_ms, now);
            }
            Ev::Inventory { tools, skills } => {
                self.unexpected_tools = tools
                    .into_iter()
                    .filter(|t| !argv::SEAT_TOOLS.contains(&t.as_str()))
                    .collect();
                self.unexpected_skills = skills
                    .into_iter()
                    .filter(|s| !self.declared_skills.iter().any(|g| glob_matches(g, s)))
                    .collect();
            }
            Ev::AgentStart => {
                self.first_turn_started = true;
                self.turn_ended = false;
                if self.state != SessionState::Ended {
                    self.state = SessionState::Working;
                }
                self.activity(line.time_ms, now);
            }
            Ev::AgentEnd { will_continue } => {
                if !will_continue {
                    self.turn_ended = true;
                    if self.state != SessionState::Ended {
                        self.state = SessionState::Idle;
                    }
                }
                self.activity(line.time_ms, now);
            }
            Ev::MessageEnd { usage } => {
                events::add(&mut self.usage, &usage);
                self.activity(line.time_ms, now);
            }
            Ev::Bash { command, exit_code } => {
                self.activity(line.time_ms, now);
                self.on_bash(&command, exit_code);
            }
            Ev::Other => {}
        }
    }

    /// One bash call. A push that names its branch and directory in the command gives one `Push`
    /// for that branch. A push with an unread target is measured in git (`measure_unread`), and
    /// the default branch of the remote is read again after it.
    fn on_bash(&mut self, command: &str, exit_code: Option<i64>) {
        let intents = push_of(command);
        if intents.is_empty() {
            return;
        }
        let outcome = match exit_code {
            Some(0) => PushOutcome::Pushed,
            Some(c) => PushOutcome::Failed(one_line_reason(&format!("exit code {c}"))),
            None => PushOutcome::Failed(one_line_reason("the exit code was not reported")),
        };
        let mut unread = false;
        for intent in intents {
            let dir_unread = !matches!(intent.dir, Part::Literal(_) | Part::Absent);
            let branch_unread = matches!(intent.branch, Part::Unresolved(_));
            if dir_unread || branch_unread {
                unread = true;
                continue;
            }
            // A named-branch push from a literal command: the branch is the command's own text,
            // and the SHA is the branch in the push directory.
            let dir: PathBuf = match &intent.dir {
                Part::Literal(d) => PathBuf::from(d),
                _ => self.push_dir.clone().unwrap_or_else(|| self.cwd.clone()),
            };
            let branch: Option<String> = match &intent.branch {
                Part::Literal(b) if b != "HEAD" => Some(b.clone()),
                _ => git::git(&dir, &["symbolic-ref", "--short", "HEAD"]).filter(|b| !b.is_empty()),
            };
            let sha = branch
                .as_deref()
                .and_then(|b| git::rev_parse(&dir, b))
                .unwrap_or_else(|| "unknown".to_string());
            self.push_event(
                branch.unwrap_or_else(|| "unknown".to_string()),
                sha,
                outcome.clone(),
            );
        }
        if unread {
            self.measure_unread(&outcome);
            self.check_default_branch();
        }
    }

    fn push_event(&mut self, branch: String, sha: String, outcome: PushOutcome) {
        self.push_seq += 1;
        self.queue.push(SessionEvent::Push {
            id: format!("{}-push-{}", self.id, self.push_seq),
            repo: self.repo.clone(),
            branch,
            sha,
            outcome,
        });
    }

    /// Measure the pushes of a call whose target the rules could not read. Read the
    /// remote-tracking refs in `push_dir`. Each ref that changed since the last read gives one
    /// `Push`, named by that ref, with its SHA. The last `Push` is always `push_branch`, with its
    /// SHA from `rev-parse`, whether or not its ref changed. A failed read counts as no ref
    /// changed, and a failed `rev-parse` gives the SHA `unknown`.
    fn measure_unread(&mut self, outcome: &PushOutcome) {
        let Some(dir) = self.push_dir.clone() else {
            // A role with no push directory cannot push: the rules deny it.
            return;
        };
        let mut changed: Vec<(String, String)> = Vec::new();
        if let Some(now) = git::remote_refs(&dir, &self.remote) {
            for (name, sha) in &now {
                if self.refs.get(name) != Some(sha) {
                    changed.push((name.clone(), sha.clone()));
                }
            }
            self.refs = now;
        }
        let last = self.push_branch.clone();
        let seq_before = self.push_seq;
        for (name, sha) in changed {
            if last.as_deref() == Some(name.as_str()) {
                continue;
            }
            self.push_event(name, sha, outcome.clone());
        }
        match last {
            Some(branch) => {
                let sha = git::rev_parse(&dir, &branch).unwrap_or_else(|| "unknown".to_string());
                self.push_event(branch, sha, outcome.clone());
            }
            None => {
                // No `push_branch` and no ref moved: the push is still one line, with no name.
                if self.push_seq == seq_before {
                    self.push_event(
                        "unknown".to_string(),
                        "unknown".to_string(),
                        outcome.clone(),
                    );
                }
            }
        }
    }

    /// Read the default branch of the remote again after a push with an unread target.
    ///
    /// - unchanged: nothing;
    /// - moved, and the push directory holds the new commit: `DefaultBranchMoved`;
    /// - moved, and the push directory lacks it: someone else moved it, so the new value becomes
    ///   the baseline and nothing is reported;
    /// - a failed read: one retry, then `DefaultBranchMoved` with an empty `after`.
    fn check_default_branch(&mut self) {
        let Some(base) = self.baseline.clone() else {
            return;
        };
        let read = || git::remote_branch_sha(&base.dir, &base.remote, &base.branch);
        let Some(now_sha) = read().or_else(read) else {
            self.queue.push(SessionEvent::DefaultBranchMoved {
                remote: base.remote.clone(),
                branch: base.branch.clone(),
                before: base.sha.clone(),
                after: String::new(),
            });
            return;
        };
        if now_sha == base.sha {
            return;
        }
        if git::has_commit(&base.dir, &now_sha) {
            self.queue.push(SessionEvent::DefaultBranchMoved {
                remote: base.remote.clone(),
                branch: base.branch.clone(),
                before: base.sha.clone(),
                after: now_sha.clone(),
            });
        }
        if let Some(b) = self.baseline.as_mut() {
            b.sha = now_sha;
        }
    }
}

impl OmpAdapter {
    fn launch_inner(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        let cwd = PathBuf::from(
            setting(plan, "cwd").ok_or_else(|| refused("the launch plan has no `cwd`"))?,
        );
        let mut env: Vec<(String, String)> = Vec::new();
        for (key, value) in &plan.settings {
            let Some(name) = key.strip_prefix("env.") else {
                continue;
            };
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if !valid {
                return Err(refused(format!(
                    "the environment name {name:?} is not valid"
                )));
            }
            if RESERVED_ENV.contains(&name) {
                return Err(refused(format!(
                    "the environment name {name} is set by the adapter only"
                )));
            }
            env.push((name.to_string(), value.clone()));
        }
        let parse_num = |key: &str, default: u64| -> Result<u64, AgentError> {
            match setting(plan, key) {
                None => Ok(default),
                Some(t) => t.parse::<u64>().ok().filter(|n| *n >= 1).ok_or_else(|| {
                    refused(format!(
                        "the setting {key} must be a whole number of 1 or more"
                    ))
                }),
            }
        };
        let first_turn = parse_num("first_turn_seconds", DEFAULT_FIRST_TURN_SECONDS)?;
        let stall_minutes = parse_num("stall_minutes", DEFAULT_STALL_MINUTES)?;

        // An unqualified version is refused before anything starts.
        let installed = self.installed_version()?;
        if !self.cfg.qualified_versions.contains(&installed) {
            return Err(refused(format!(
                "OMP {installed} is not qualified; the qualified versions are: {}",
                self.cfg.qualified_versions.join(", ")
            )));
        }

        if self.cfg.isolation_check {
            let plugins = argv::csv(setting(plan, "plugins").unwrap_or_default());
            let plugin_dirs: Vec<PathBuf> = plugins.iter().map(PathBuf::from).collect();
            let home = std::env::var_os("HOME").map(PathBuf::from);
            isolation::check(&cwd, &plugin_dirs, home.as_deref()).map_err(refused)?;
        }

        let hook = self.write_extension()?;
        let token = token::fresh_token();
        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("omp-{n:04}-{}", &token[..8]);
        let dir = self.cfg.state_dir.join("sessions").join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| failed(format!("cannot make {dir:?}: {e}")))?;
        let events_path = dir.join("events.jsonl");
        let inbox_path = dir.join("inbox.jsonl");
        let log_path = dir.join("adapter.log");
        for p in [&events_path, &inbox_path] {
            std::fs::write(p, b"").map_err(|e| failed(format!("cannot write {p:?}: {e}")))?;
        }

        let args = argv::build_argv(
            plan,
            &hook,
            &dir.join("omp-sessions"),
            self.cfg.brief_root.as_deref(),
        )?;
        // OMP's `edit` carries no path in its default mode. In `replace` mode it does, so the
        // controller's hook can judge the write.
        env.push(("PI_EDIT_VARIANT".into(), "replace".into()));
        env.push(("PF_SEAT_TOKEN".into(), token.clone()));
        env.push(("PF_EVENT_FILE".into(), events_path.display().to_string()));
        env.push(("PF_INBOX_FILE".into(), inbox_path.display().to_string()));
        env.push(("PF_SESSION_DIR".into(), dir.display().to_string()));
        env.push(("PF_SEAT_ROLE".into(), plan.role.clone()));
        env.push(("PF_ALLOWED_TOOLS".into(), argv::SEAT_TOOLS.join(",")));

        let push_dir = setting(plan, "push_dir").map(PathBuf::from);
        let remote = setting(plan, "remote").unwrap_or("origin").to_string();
        // With a `default_branch` setting, the launch reads that branch on the remote, and a
        // failed read refuses the launch: a seat that pushes must have a baseline to be checked
        // against. Without the setting, the remote's own HEAD gives the branch, and a failed read
        // leaves no baseline.
        let baseline = match (&push_dir, setting(plan, "default_branch")) {
            (Some(d), Some(branch)) => {
                let sha = git::remote_branch_sha(d, &remote, branch).ok_or_else(|| {
                    refused(format!(
                        "cannot read {remote}/{branch} in {}, so the default branch has no baseline",
                        d.display()
                    ))
                })?;
                Some(Baseline {
                    remote: remote.clone(),
                    branch: branch.to_string(),
                    sha,
                    dir: d.clone(),
                })
            }
            (Some(d), None) => git::remote_head(d, &remote).map(|(branch, sha)| Baseline {
                remote: remote.clone(),
                branch,
                sha,
                dir: d.clone(),
            }),
            (None, _) => None,
        };
        let refs = push_dir
            .as_ref()
            .and_then(|d| git::remote_refs(d, &remote))
            .unwrap_or_default();

        let spec = SpawnSpec {
            program: self.cfg.omp_bin.clone(),
            args,
            cwd: cwd.clone(),
            env,
            rows: self.cfg.rows,
            cols: self.cfg.cols,
        };
        let mut spawned = PtySession::spawn(&spec);
        for _ in 0..5 {
            // A program that was written a moment ago can be busy for a few milliseconds.
            if !spawned
                .as_ref()
                .is_err_and(|e| e.contains("Text file busy"))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
            spawned = PtySession::spawn(&spec);
        }
        let pty = spawned.map_err(failed)?;

        let now = (self.cfg.clock_ms)();
        let mut session = Session {
            id: id.clone(),
            dir,
            events_path,
            inbox_path,
            log_path,
            offset: 0,
            partial: Vec::new(),
            pty,
            token,
            state: SessionState::Working,
            turn_ended: false,
            first_turn_started: false,
            usage: Usage::default(),
            last_activity_ms: now,
            stall_reported: false,
            stall_limit_ms: stall_minutes * 60_000,
            repo: setting(plan, "repo").map_or_else(|| self.cfg.repo.clone(), str::to_string),
            cwd,
            push_dir,
            push_branch: setting(plan, "push_branch").map(str::to_string),
            remote,
            refs,
            baseline,
            queue: Vec::new(),
            push_seq: 0,
            inbox_seq: 0,
            unexpected_tools: Vec::new(),
            declared_skills: argv::csv(setting(plan, "skills").unwrap_or_default()),
            unexpected_skills: Vec::new(),
        };

        // First turn: no `agent_start` in time fails the launch, with the screen as data.
        let end = Instant::now() + Duration::from_secs(first_turn);
        loop {
            session.refresh((self.cfg.clock_ms)());
            if !session.unexpected_tools.is_empty() {
                // A tool outside the seat's list reached it (for example an MCP server's tool).
                let list = session.unexpected_tools.join(", ");
                session.pty.kill();
                return Err(refused(format!(
                    "tools outside the seat tool list reached the seat and cannot be limited: {list}"
                )));
            }
            if !session.unexpected_skills.is_empty() {
                // A skill that the plan did not declare reached the seat. Fail closed.
                let list = session.unexpected_skills.join(", ");
                session.pty.kill();
                return Err(refused(format!(
                    "skills that the plan does not declare reached the seat: {list}"
                )));
            }
            if session.first_turn_started {
                break;
            }
            let exited = session.pty.exit_code();
            let timed_out = Instant::now() >= end;
            if exited.is_some() || timed_out {
                if exited.is_some() {
                    // The exit code can be known before the last output reaches the buffer.
                    session.pty.wait_output_end(Duration::from_secs(2));
                }
                let head = match exited {
                    Some(code) => format!("OMP exited with {code} before a first turn; screen: "),
                    None => format!("no first turn in {first_turn} s; screen: "),
                };
                // The engine adds its own prefix and cuts a reason at REASON_MAX_CHARS. A cut
                // takes the end of the screen, where a prompt sits, so the tail shrinks to fit.
                let room = REASON_MAX_CHARS
                    .saturating_sub(ENGINE_FAILED_PREFIX_CHARS + head.chars().count());
                let tail = redacted_tail(
                    &session.pty.screen(),
                    &session.token,
                    SCREEN_TAIL_CHARS.min(room),
                );
                session.pty.kill();
                return Err(failed(format!("{head}{tail}")));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), Arc::new(Mutex::new(session)));
        Ok(SessionRef(id))
    }
}

impl AgentPort for OmpAdapter {
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.launch_inner(plan)
    }

    fn status(&self, session: &SessionRef) -> Result<SessionStatus, AgentError> {
        let s = self.find(session)?;
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.refresh((self.cfg.clock_ms)());
        Ok(SessionStatus {
            state: s.state,
            turn_ended: s.turn_ended,
            first_turn_started: s.first_turn_started,
        })
    }

    fn deliver(&self, session: &SessionRef, message: &str) -> Result<(), AgentError> {
        let s = self.find(session)?;
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.refresh((self.cfg.clock_ms)());
        if s.state != SessionState::Idle {
            return Err(failed(
                "the session is not idle; a message reaches an idle session only",
            ));
        }
        s.inbox_seq += 1;
        let line = serde_json::json!({ "id": s.inbox_seq, "text": message }).to_string();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&s.inbox_path)
            .map_err(|e| failed(format!("cannot write the inbox: {e}")))?;
        writeln!(f, "{line}").map_err(|e| failed(format!("cannot write the inbox: {e}")))?;
        // The extension starts a turn at once. Until its `agent_start` arrives, the session is busy.
        s.state = SessionState::Working;
        s.turn_ended = false;
        Ok(())
    }

    fn interrupt(&self, session: &SessionRef) -> Result<(), AgentError> {
        let s = self.find(session)?;
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        // Escape interrupts the running turn of OMP's terminal UI.
        s.pty.write(b"\x1b").map_err(failed)
    }

    fn usage(&self, session: &SessionRef) -> Result<Usage, AgentError> {
        let s = self.find(session)?;
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.refresh((self.cfg.clock_ms)());
        Ok(s.usage)
    }

    fn answer_dialog(
        &self,
        _session: &SessionRef,
        _dialog: &Dialog,
    ) -> Result<DialogAnswer, AgentError> {
        // No launch dialog is known yet. An unknown dialog parks the unit; it never blocks it.
        Ok(DialogAnswer::Unknown)
    }

    fn apply_rules(&self, plan: &mut LaunchPlan, rules: &RuleSet) -> Result<(), AgentError> {
        if !rules.prompts_off {
            return Err(refused(
                "a seat runs with permission prompts off, and the rule set says they are on",
            ));
        }
        plan.settings
            .insert("rule_set".to_string(), rules.name.clone());
        plan.settings
            .insert("rule_deny".to_string(), rules.deny.join(","));
        Ok(())
    }

    fn remove_ask_tool(&self, plan: &mut LaunchPlan) -> Result<(), AgentError> {
        // The tool list in the command line never holds `ask`. The setting records the duty.
        plan.settings
            .insert("ask_tool".to_string(), "off".to_string());
        Ok(())
    }

    fn hand_off(
        &self,
        session: &SessionRef,
        successor: &LaunchPlan,
    ) -> Result<SessionRef, AgentError> {
        // The successor starts first. A failed start leaves the old session as it was.
        let next = self.launch(successor)?;
        self.stop(session)?;
        Ok(next)
    }

    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError> {
        let Ok(s) = self.find(session) else {
            return Ok(false);
        };
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        Ok(s.pty.exit_code().is_none())
    }

    /// End the process group of a session. A session this adapter does not know is gone: `Ok`.
    fn stop(&self, session: &SessionRef) -> Result<(), AgentError> {
        let Ok(s) = self.find(session) else {
            return Ok(());
        };
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.pty.kill();
        s.state = SessionState::Ended;
        Ok(())
    }

    fn resolve_plugin(&self, name: &str, min_version: &str) -> Result<ResolvedPlugin, AgentError> {
        plugin::resolve(&self.cfg.plugin_roots, name, min_version)
    }

    fn drain_events(&self, session: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        let s = self.find(session)?;
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.cfg.clock_ms)();
        s.refresh(now);
        if s.state == SessionState::Working
            && !s.stall_reported
            && now.saturating_sub(s.last_activity_ms) > s.stall_limit_ms
        {
            s.stall_reported = true;
            let tail = redacted_tail(&s.pty.screen(), &s.token, SCREEN_TAIL_CHARS);
            s.queue.push(SessionEvent::Stalled { screen_tail: tail });
        }
        Ok(std::mem::take(&mut s.queue))
    }
}

/// The directory of a session, for tests and diagnosis.
pub fn session_dir_of(adapter: &OmpAdapter, session: &SessionRef) -> Option<PathBuf> {
    let s = adapter.find(session).ok()?;
    let dir: PathBuf = s.lock().unwrap_or_else(|e| e.into_inner()).dir.clone();
    Some(dir)
}
