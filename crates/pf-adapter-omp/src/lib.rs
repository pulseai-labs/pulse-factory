//! `pf-adapter-omp`: the first agent adapter. It runs OMP as an interactive session on a terminal in
//! a session host (`r2.s2.w4`, ADR-0011's amendment of 2026-11-16: the PTY moves into the host) and
//! reads its turn ends, usage, pushes and stalls from events that Pulse Factory's own OMP extension
//! writes (ADR-0011). The contract for any adapter is `docs/contracts/agent-adapter.md`.
//!
//! The adapter asks the controller's host service (`SessionHosts`) for a host, drives OMP through the
//! host's input and output, and reads the event file. It saves, through `save_position`, how far it
//! has read and what it derived from the lines before that point, so that after a controller restart
//! `rebind` reads on from there and an event is taken exactly once (ADR-0011's kept-and-resent event
//! test).

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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{
    AgentError, AgentPort, Dialog, DialogAnswer, LaunchPlan, RebindOutcome, ResolvedPlugin,
    RuleSet, SessionEvent, SessionRef, SessionState, SessionStatus, Usage,
};
use pf_engine::port::host::{
    HostEvent, HostExit, HostHandle, HostLaunch, HostTransport, Rebind, SessionHosts,
};
use pf_engine::status::{REASON_MAX_CHARS, REDACTED, one_line_reason, redact};
use pf_rules::{Part, push_of};
use serde_json::{Value, json};

use crate::events::{Ev, parse_line};

/// Pulse Factory's own OMP extension. The adapter writes it to its state directory at launch.
pub const EXTENSION_SOURCE: &str = include_str!("../extension/pf-seat.ts");

/// How many characters of the screen go into a failure reason (ADR-0006: a reason holds 500).
pub const SCREEN_TAIL_CHARS: usize = 450;

/// The length of `launch failed: `, which the engine puts before an `AgentError::Failed` reason.
const ENGINE_FAILED_PREFIX_CHARS: usize = "launch failed: ".len();

const DEFAULT_FIRST_TURN_SECONDS: u64 = 120;
const DEFAULT_STALL_MINUTES: u64 = 20;

/// How much output the adapter keeps for `screen_tail`.
const SCREEN_KEEP_BYTES: usize = 256 * 1024;

/// The adapter acknowledges output up to this far below the newest byte, so the host keeps the last
/// 32 KiB (half of the 64 KiB minimum of `session.replay_bytes`) for a rebound adapter's screen.
const ACK_MARGIN_BYTES: u64 = 32 * 1024;

/// How long `stop` waits for the child's group to go before it sends `SIGKILL`, and how long it
/// waits after that.
const GROUP_GRACE: Duration = Duration::from_secs(5);
const GROUP_AFTER_KILL: Duration = Duration::from_secs(2);

/// The names the adapter takes from the controller's own environment. The host clears the child's
/// environment, so this list replaces the inheritance the seat had as a child of the controller:
/// the thirteen names of the paper adapter, and the names a terminal program, git, cargo and a push
/// plausibly read.
const INHERITED_ENV: [&str; 24] = [
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
    "TERM",
    "COLORTERM",
    "SHELL",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "SSH_AUTH_SOCK",
];

/// The launch setting whose value is a secret: the seat's session key.
const SECRET_ENV: &str = "PF_SESSION_KEY";

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

/// The version of the position JSON this adapter writes and reads.
const POSITION_VERSION: u64 = 1;

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
    /// The host unit: the session reference, and the name of every file and push ID.
    id: String,
    dir: PathBuf,
    events_path: PathBuf,
    inbox_path: PathBuf,
    log_path: PathBuf,
    handle: HostHandle,
    child_pid: u32,
    /// The host's exit, once seen.
    exit: Option<HostExit>,
    /// The child ended (the exit is seen, the host is gone, or the adapter stopped it).
    exited: bool,
    /// The exit was acknowledged, or there is nothing to acknowledge.
    exit_done: bool,
    stopped: bool,
    /// The child's process group was seen empty: later stops skip the wait (a process ID can be
    /// reused).
    group_gone: bool,
    /// The last `SCREEN_KEEP_BYTES` of output.
    screen: Vec<u8>,
    /// The output cursor after the newest byte the adapter took.
    screen_end: u64,
    /// The output cursor the adapter acknowledged.
    acked: u64,
    got_output: bool,
    /// The seat's token, when the adapter launched the seat. Empty after a rebind.
    token: String,
    /// The byte after the last whole line read.
    offset: u64,
    /// A line was read since the last save.
    unsaved: bool,
    /// The last save held events that were pending.
    pending_saved: bool,
    /// The events file was read to its end.
    at_end: bool,
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
    hosts: Arc<dyn SessionHosts>,
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
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

/// A string at `path` in a record, or empty.
fn record_text<'a>(record: &'a Value, path: &[&str]) -> &'a str {
    let mut v = record;
    for key in path {
        v = &v[*key];
    }
    v.as_str().unwrap_or_default()
}

fn state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Working => "working",
        SessionState::Idle => "idle",
        SessionState::Ended => "ended",
    }
}

fn state_from(name: &str) -> Option<SessionState> {
    match name {
        "working" => Some(SessionState::Working),
        "idle" => Some(SessionState::Idle),
        "ended" => Some(SessionState::Ended),
        _ => None,
    }
}

/// An event in the position's own JSON form, or `None` for one that is not kept. `Activity` and
/// `Stalled` carry no effect, and a stall is found again from `last_activity_ms`.
fn event_to_json(event: &SessionEvent) -> Option<Value> {
    match event {
        SessionEvent::Push {
            id,
            repo,
            branch,
            sha,
            outcome,
        } => {
            let outcome = match outcome {
                PushOutcome::Pushed => json!({ "pushed": true }),
                PushOutcome::Failed(why) => json!({ "failed": why }),
            };
            Some(json!({
                "kind": "push", "id": id, "repo": repo, "branch": branch, "sha": sha,
                "outcome": outcome,
            }))
        }
        SessionEvent::DefaultBranchMoved {
            remote,
            branch,
            before,
            after,
        } => Some(json!({
            "kind": "default_branch_moved", "remote": remote, "branch": branch,
            "before": before, "after": after,
        })),
        SessionEvent::Activity | SessionEvent::Stalled { .. } => None,
    }
}

fn event_from_json(v: &Value) -> Option<SessionEvent> {
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    match v.get("kind")?.as_str()? {
        "push" => {
            let outcome = v.get("outcome")?;
            let outcome = match outcome.get("failed").and_then(Value::as_str) {
                Some(why) => PushOutcome::Failed(why.to_string()),
                None if outcome.get("pushed").and_then(Value::as_bool) == Some(true) => {
                    PushOutcome::Pushed
                }
                None => return None,
            };
            Some(SessionEvent::Push {
                id: text("id")?,
                repo: text("repo")?,
                branch: text("branch")?,
                sha: text("sha")?,
                outcome,
            })
        }
        "default_branch_moved" => Some(SessionEvent::DefaultBranchMoved {
            remote: text("remote")?,
            branch: text("branch")?,
            before: text("before")?,
            after: text("after")?,
        }),
        _ => None,
    }
}

/// The `omp` program as an absolute path. A bare name is looked up on the controller's `PATH`.
fn absolute_program(omp: &Path) -> Result<PathBuf, AgentError> {
    if omp.components().count() > 1 || omp.is_absolute() {
        return std::path::absolute(omp)
            .map_err(|e| failed(format!("cannot resolve {}: {e}", omp.display())));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join(omp))
        .find(|p| p.is_file())
        .and_then(|p| std::path::absolute(p).ok())
        .ok_or_else(|| failed(format!("cannot find {} on PATH", omp.display())))
}

impl OmpAdapter {
    pub fn new(cfg: OmpConfig, hosts: Arc<dyn SessionHosts>) -> OmpAdapter {
        OmpAdapter {
            cfg,
            hosts,
            sessions: Mutex::new(HashMap::new()),
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

    /// The process ID of a session's OMP, while it runs.
    pub fn pid(&self, session: &SessionRef) -> Option<u32> {
        let s = self.find(session).ok()?;
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        (!s.exited && !s.stopped).then_some(s.child_pid)
    }

    /// The screen of a session, redacted and flattened, last `max_chars` characters. Reason text
    /// only: never use it to detect a state.
    pub fn screen_tail(&self, session: &SessionRef, max_chars: usize) -> Option<String> {
        let s = self.find(session).ok()?;
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        Some(redacted_tail(&s.screen_text(), &s.token, max_chars))
    }

    /// Write bytes to the session's terminal.
    pub fn write_terminal(&self, session: &SessionRef, bytes: &[u8]) -> Result<(), AgentError> {
        let s = self.find(session)?;
        let s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.handle
            .input(bytes)
            .map_err(|e| failed(format!("cannot write the terminal of {}: {e}", s.id)))
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

/// Collapse, redact (also the session's own token) and cut the end of a screen. A run of exactly
/// 32 hex digits is masked too (`screen::tail`): after a rebind the token's value is not known.
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

    /// The output kept so far, without escape sequences.
    fn screen_text(&self) -> String {
        screen::strip_ansi(&self.screen)
    }

    fn activity(&mut self, time_ms: Option<u64>, now: u64) {
        let t = time_ms.unwrap_or(now);
        self.last_activity_ms = self.last_activity_ms.max(t);
        self.stall_reported = false;
        if !self.queue.contains(&SessionEvent::Activity) {
            self.queue.push(SessionEvent::Activity);
        }
    }

    /// Take what the host streams: keep the screen, acknowledge output, and see the exit.
    fn pump_host(&mut self) {
        if self.exit_done {
            return;
        }
        match self.handle.events(Duration::ZERO) {
            Ok(events) => {
                for event in events {
                    match event {
                        HostEvent::Out { at, data } => {
                            self.got_output = true;
                            self.screen.extend_from_slice(&data);
                            if self.screen.len() > SCREEN_KEEP_BYTES {
                                let cut = self.screen.len() - SCREEN_KEEP_BYTES;
                                self.screen.drain(..cut);
                            }
                            self.screen_end = self.screen_end.max(at + data.len() as u64);
                        }
                        HostEvent::Gap { from, to } => {
                            // After a rebind the host replays from what it still holds: a gap
                            // before the first byte is expected.
                            if self.got_output {
                                self.log(&format!("the host lost output from {from} to {to}"));
                            }
                            self.screen_end = self.screen_end.max(to);
                        }
                        HostEvent::Exit(exit) => {
                            self.exited = true;
                            self.screen_end = self.screen_end.max(exit.at);
                            self.ack_to(exit.at);
                            self.exit = Some(exit);
                        }
                    }
                }
                if !self.exited {
                    self.ack_to(self.screen_end.saturating_sub(ACK_MARGIN_BYTES));
                }
            }
            // The host is gone, or another controller took its place.
            Err(_) => {
                self.exited = true;
                self.exit_done = true;
            }
        }
    }

    /// Acknowledge output below `to`. The screen is reason text only, so no commit consumes it.
    fn ack_to(&mut self, to: u64) {
        if to > self.acked && self.handle.ack(to).is_ok() {
            self.acked = to;
        }
    }

    /// Read the lines that came since the last read, and update the state from them.
    fn refresh(&mut self, now: u64) {
        self.pump_host();
        self.read_events(now);
        if self.exited {
            self.state = SessionState::Ended;
        }
    }

    /// Read the events file from `offset` and take each whole line. A last line with no newline
    /// counts once the child has ended.
    fn read_events(&mut self, now: u64) {
        let Ok(mut f) = std::fs::File::open(&self.events_path) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        let mut used = 0;
        while let Some(i) = buf[used..].iter().position(|b| *b == b'\n') {
            self.take_line(&buf[used..used + i], now);
            used += i + 1;
        }
        if self.exited && used < buf.len() {
            self.take_line(&buf[used..], now);
            used = buf.len();
        }
        if used > 0 {
            self.offset += used as u64;
            self.unsaved = true;
        }
        self.at_end = used == buf.len();
    }

    fn take_line(&mut self, raw: &[u8], now: u64) {
        let line = String::from_utf8_lossy(raw).to_string();
        if line.trim().is_empty() {
            return;
        }
        match parse_line(&line) {
            Ok(l) => self.apply(l, now),
            Err(why) => {
                let shown: String = redact(&line).chars().take(200).collect();
                self.log(&format!("skipped an event line ({why}): {shown}"));
            }
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

    /// The position (ADR-0011's kept-and-resent event test): how far the adapter read, what it
    /// derived from the lines before that point, and the events it took from them that nobody has
    /// acknowledged (`pending`).
    fn position(&self, pending: &[Value]) -> Value {
        json!({
            "v": POSITION_VERSION,
            "offset": self.offset,
            "usage": serde_json::to_value(self.usage).unwrap_or(Value::Null),
            "push_seq": self.push_seq,
            "state": state_name(self.state),
            "turn_ended": self.turn_ended,
            "first_turn_started": self.first_turn_started,
            "last_activity_ms": self.last_activity_ms,
            "refs": self.refs,
            "baseline": self.baseline.as_ref().map(|b| json!({
                "remote": b.remote,
                "branch": b.branch,
                "sha": b.sha,
                "dir": b.dir.display().to_string(),
            })),
            "pending": pending,
        })
    }

    /// Take a stored position back. The text says what is wrong with a position that is not one.
    fn restore(&mut self, p: &Value) -> Result<(), String> {
        if p.get("v").and_then(Value::as_u64) != Some(POSITION_VERSION) {
            return Err("its version is not known".to_string());
        }
        self.offset = p
            .get("offset")
            .and_then(Value::as_u64)
            .ok_or("it has no offset")?;
        self.usage = p
            .get("usage")
            .and_then(|u| serde_json::from_value(u.clone()).ok())
            .ok_or("its usage is unreadable")?;
        self.push_seq = p.get("push_seq").and_then(Value::as_u64).unwrap_or(0);
        self.state = p
            .get("state")
            .and_then(Value::as_str)
            .and_then(state_from)
            .ok_or("it has no state")?;
        self.turn_ended = p
            .get("turn_ended")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.first_turn_started = p
            .get("first_turn_started")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.last_activity_ms = p
            .get("last_activity_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.refs = p
            .get("refs")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        self.baseline = p
            .get("baseline")
            .filter(|b| b.is_object())
            .map(|b| Baseline {
                remote: record_text(b, &["remote"]).to_string(),
                branch: record_text(b, &["branch"]).to_string(),
                sha: record_text(b, &["sha"]).to_string(),
                dir: PathBuf::from(record_text(b, &["dir"])),
            });
        // The events handed over before the restart and not acknowledged come again, first.
        let pending: Vec<SessionEvent> = p
            .get("pending")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(event_from_json).collect())
            .unwrap_or_default();
        self.pending_saved = !pending.is_empty();
        let mut queue = pending;
        queue.append(&mut self.queue);
        self.queue = queue;
        Ok(())
    }

    /// Save the position, with the persistable events of the queue as pending.
    fn save(&mut self, hosts: &dyn SessionHosts, what: &str) -> Result<(), AgentError> {
        let persist: Vec<Value> = self.queue.iter().filter_map(event_to_json).collect();
        let position = self.position(&persist);
        hosts.save_position(&self.id, position).map_err(|e| {
            failed(format!(
                "cannot save the {what}position of {}: {e}",
                self.id
            ))
        })?;
        self.unsaved = false;
        self.pending_saved = !persist.is_empty();
        Ok(())
    }

    /// The exit in words, for a failure reason.
    fn exit_text(&self) -> String {
        match &self.exit {
            Some(HostExit {
                code: Some(code), ..
            }) => code.to_string(),
            Some(HostExit {
                signal: Some(signal),
                ..
            }) => format!("signal {signal}"),
            _ => "an unknown status".to_string(),
        }
    }

    /// End the host and wait until its child's process group is gone. After the first successful
    /// wait the group is not looked at again: a process ID can be reused.
    fn shutdown(&mut self) -> Result<(), AgentError> {
        if !self.group_gone {
            // The host may be gone already: its error is no failure.
            let _ = self.handle.stop();
            let mut gone = pty::wait_group_gone(self.child_pid, GROUP_GRACE);
            if gone.is_err() {
                pty::kill_group(self.child_pid);
                gone = pty::wait_group_gone(self.child_pid, GROUP_AFTER_KILL);
            }
            if let Err(pids) = gone {
                let list: Vec<String> = pids.iter().map(u32::to_string).collect();
                return Err(failed(format!(
                    "the group of {} still has a live process: {}",
                    self.id,
                    list.join(", ")
                )));
            }
            self.group_gone = true;
        }
        self.exited = true;
        self.exit_done = true;
        self.stopped = true;
        self.state = SessionState::Ended;
        Ok(())
    }
}

impl OmpAdapter {
    fn launch_inner(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        let cwd = PathBuf::from(
            setting(plan, "cwd").ok_or_else(|| refused("the launch plan has no `cwd`"))?,
        );
        let mut env: BTreeMap<String, String> = INHERITED_ENV
            .iter()
            .filter_map(|n| std::env::var(n).ok().map(|v| ((*n).to_string(), v)))
            .collect();
        let mut secrets: BTreeMap<String, String> = BTreeMap::new();
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
            if name == SECRET_ENV {
                secrets.insert(name.to_string(), value.clone());
            } else {
                env.insert(name.to_string(), value.clone());
            }
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
        // The host's unit is not known before the host starts: the service replaces `{unit}`, and
        // makes both files and their directory before the child can look for them.
        let session_tpl = self.cfg.state_dir.join("sessions").join("{unit}");
        let events_tpl = session_tpl.join("events.jsonl").display().to_string();
        let inbox_tpl = session_tpl.join("inbox.jsonl").display().to_string();
        let session_tpl_text = session_tpl.display().to_string();

        let omp = absolute_program(&self.cfg.omp_bin)?;
        let mut command = vec![omp.display().to_string()];
        command.extend(argv::build_argv(
            plan,
            &hook,
            &session_tpl.join("omp-sessions"),
            self.cfg.brief_root.as_deref(),
        )?);
        // OMP's `edit` carries no path in its default mode. In `replace` mode it does, so the
        // controller's hook can judge the write.
        env.insert("PI_EDIT_VARIANT".into(), "replace".into());
        env.insert("PF_EVENT_FILE".into(), events_tpl.clone());
        env.insert("PF_INBOX_FILE".into(), inbox_tpl.clone());
        env.insert("PF_SESSION_DIR".into(), session_tpl_text.clone());
        env.insert("PF_SEAT_ROLE".into(), plan.role.clone());
        env.insert("PF_ALLOWED_TOOLS".into(), argv::SEAT_TOOLS.join(","));
        secrets.insert("PF_SEAT_TOKEN".into(), token.clone());

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

        let binary = std::fs::read(&omp)
            .map_err(|e| failed(format!("cannot read {}: {e}", omp.display())))?;
        let launch = HostLaunch {
            session_kind: "seat".to_string(),
            session_key: format!("{}/{}", plan.run_id, plan.unit_id),
            argv: command,
            cwd: cwd.display().to_string(),
            env,
            files: vec![events_tpl.clone(), inbox_tpl.clone()],
            secrets,
            transport: HostTransport::Terminal {
                rows: self.cfg.rows,
                cols: self.cfg.cols,
            },
            runtime: json!({
                "id": "omp",
                "version": installed,
                "binary_path": omp.display().to_string(),
                "sha256": pf_engine::claim::sha256_hex(&binary),
            }),
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

        let unit = handle.unit.clone();
        let named = |tpl: &str| PathBuf::from(tpl.replace("{unit}", &unit));
        let dir = named(&session_tpl_text);
        let now = (self.cfg.clock_ms)();
        let mut session = Session {
            id: unit.clone(),
            log_path: dir.join("adapter.log"),
            dir,
            events_path: named(&events_tpl),
            inbox_path: named(&inbox_tpl),
            child_pid: handle.child_pid,
            exit: handle.exit.clone(),
            exited: handle.exit.is_some(),
            exit_done: false,
            stopped: false,
            group_gone: false,
            screen: Vec::new(),
            screen_end: 0,
            acked: 0,
            got_output: false,
            handle,
            token,
            offset: 0,
            unsaved: false,
            pending_saved: false,
            at_end: false,
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
        if let Err(e) = session.handle.read(0) {
            let _ = session.shutdown();
            return Err(failed(format!("cannot read the output of {unit}: {e}")));
        }

        // First turn: no `agent_start` in time fails the launch, with the screen as data. Every
        // failure ends the host first, so no host and no child is left.
        let end = Instant::now() + Duration::from_secs(first_turn);
        loop {
            session.refresh((self.cfg.clock_ms)());
            if !session.unexpected_tools.is_empty() {
                // A tool outside the seat's list reached it (for example an MCP server's tool).
                let list = session.unexpected_tools.join(", ");
                let _ = session.shutdown();
                return Err(refused(format!(
                    "tools outside the seat tool list reached the seat and cannot be limited: {list}"
                )));
            }
            if !session.unexpected_skills.is_empty() {
                // A skill that the plan did not declare reached the seat. Fail closed.
                let list = session.unexpected_skills.join(", ");
                let _ = session.shutdown();
                return Err(refused(format!(
                    "skills that the plan does not declare reached the seat: {list}"
                )));
            }
            if session.first_turn_started {
                break;
            }
            let timed_out = Instant::now() >= end;
            if session.exited || timed_out {
                let head = if session.exited {
                    format!(
                        "OMP exited with {} before a first turn; screen: ",
                        session.exit_text()
                    )
                } else {
                    format!("no first turn in {first_turn} s; screen: ")
                };
                // The engine adds its own prefix and cuts a reason at REASON_MAX_CHARS. A cut
                // takes the end of the screen, where a prompt sits, so the tail shrinks to fit.
                let room = REASON_MAX_CHARS
                    .saturating_sub(ENGINE_FAILED_PREFIX_CHARS + head.chars().count());
                let tail = redacted_tail(
                    &session.screen_text(),
                    &session.token,
                    SCREEN_TAIL_CHARS.min(room),
                );
                let _ = session.shutdown();
                return Err(failed(format!("{head}{tail}")));
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // The first position is saved before the launch returns, so that the baseline and the refs
        // read at launch survive a restart. A failed save ends the host and fails the launch.
        if let Err(e) = session.save(self.hosts.as_ref(), "first ") {
            let _ = session.shutdown();
            return Err(e);
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(unit.clone(), Arc::new(Mutex::new(session)));
        Ok(SessionRef(unit))
    }

    /// Take a host that survived a controller restart: rebuild the session from the launched record
    /// and the stored position.
    fn rebind_session(&self, handle: HostHandle) -> Result<RebindOutcome, AgentError> {
        let unit = handle.unit.clone();
        let unreachable = |why: String| Ok(RebindOutcome::Unreachable { why });
        let position = match self.hosts.position(&unit) {
            Ok(Some(p)) => p,
            Ok(None) => return unreachable(format!("no stored position for {unit}")),
            Err(e) => return unreachable(format!("cannot read the position of {unit}: {e}")),
        };
        let record = handle.record.clone();
        let events = record_text(&record, &["env", "PF_EVENT_FILE"]);
        let inbox = record_text(&record, &["env", "PF_INBOX_FILE"]);
        let dir = record_text(&record, &["env", "PF_SESSION_DIR"]);
        if events.is_empty() || inbox.is_empty() || dir.is_empty() {
            return unreachable(format!(
                "the launch record of {unit} names no event file, inbox file or session directory"
            ));
        }
        let text = |key: &str| record_text(&record, &["settings", key]);
        let stall_minutes = text("stall_minutes")
            .parse::<u64>()
            .ok()
            .filter(|n| *n >= 1)
            .unwrap_or(DEFAULT_STALL_MINUTES);
        let inbox_path = PathBuf::from(inbox);
        let inbox_lines = std::fs::read(&inbox_path)
            .map(|b| b.iter().filter(|c| **c == b'\n').count() as u64)
            .unwrap_or(0);
        let dir = PathBuf::from(dir);
        let mut session = Session {
            id: unit.clone(),
            log_path: dir.join("adapter.log"),
            dir,
            events_path: PathBuf::from(events),
            inbox_path,
            child_pid: handle.child_pid,
            exit: handle.exit.clone(),
            exited: handle.exit.is_some(),
            exit_done: false,
            stopped: false,
            group_gone: false,
            screen: Vec::new(),
            screen_end: 0,
            acked: 0,
            got_output: false,
            handle,
            token: String::new(),
            offset: 0,
            unsaved: false,
            pending_saved: false,
            at_end: false,
            state: SessionState::Working,
            turn_ended: false,
            first_turn_started: false,
            usage: Usage::default(),
            last_activity_ms: (self.cfg.clock_ms)(),
            stall_reported: false,
            stall_limit_ms: stall_minutes * 60_000,
            repo: Some(text("repo"))
                .filter(|r| !r.is_empty())
                .map_or_else(|| self.cfg.repo.clone(), str::to_string),
            cwd: PathBuf::from(record_text(&record, &["cwd"])),
            push_dir: Some(text("push_dir"))
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
            push_branch: Some(text("push_branch"))
                .filter(|b| !b.is_empty())
                .map(str::to_string),
            remote: Some(text("remote"))
                .filter(|r| !r.is_empty())
                .unwrap_or("origin")
                .to_string(),
            refs: BTreeMap::new(),
            baseline: None,
            queue: Vec::new(),
            push_seq: 0,
            inbox_seq: inbox_lines,
            unexpected_tools: Vec::new(),
            declared_skills: argv::csv(text("skills")),
            unexpected_skills: Vec::new(),
        };
        if let Err(why) = session.restore(&position) {
            return unreachable(format!(
                "the stored position of {unit} is unreadable: {why}"
            ));
        }
        if let Some(exit) = session.exit.clone() {
            // The child ended before the attach: every byte is consumed here, so the host can end.
            session.ack_to(exit.at);
        }
        if let Err(e) = session.handle.read(0) {
            return unreachable(format!("cannot read the output of {unit}: {e}"));
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(unit, Arc::new(Mutex::new(session)));
        Ok(RebindOutcome::Survived)
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
        // The next drain saves this state: a restart before the `agent_start` line reads on busy.
        s.unsaved = true;
        Ok(())
    }

    fn interrupt(&self, session: &SessionRef) -> Result<(), AgentError> {
        // Escape interrupts the running turn of OMP's terminal UI.
        self.write_terminal(session, b"\x1b")
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

    /// The old host stops first, then the successor starts: the service stops an older incarnation
    /// of the same session key anyway, so two copies never run (Q9).
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
        self.pid(session)
    }

    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError> {
        let Ok(s) = self.find(session) else {
            return Ok(false);
        };
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.pump_host();
        Ok(!s.exited && !s.stopped)
    }

    /// Attach to the unit's host again and rebuild the state from the launch record and the stored
    /// position.
    fn rebind(&self, session: &SessionRef) -> Result<RebindOutcome, AgentError> {
        match self.hosts.rebind(&session.0) {
            Rebind::Lost { exit } => {
                self.sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&session.0);
                let cause = exit
                    .as_str()
                    .map_or_else(|| exit.to_string(), str::to_string);
                Ok(RebindOutcome::Lost { cause })
            }
            Rebind::Unreachable { why } => Ok(RebindOutcome::Unreachable { why }),
            Rebind::Survived(handle) => self.rebind_session(handle),
        }
    }

    /// End the host and wait until no process of the child's group is live. A session this
    /// adapter does not know is gone: `Ok`.
    fn stop(&self, session: &SessionRef) -> Result<(), AgentError> {
        let Ok(s) = self.find(session) else {
            return Ok(());
        };
        let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
        s.shutdown()
    }

    fn resolve_plugin(&self, name: &str, min_version: &str) -> Result<ResolvedPlugin, AgentError> {
        plugin::resolve(&self.cfg.plugin_roots, name, min_version)
    }

    /// Take the events since the last drain. The position is saved first, with the events as
    /// pending, and only then are they handed over: a failed save keeps the queue. The next drain
    /// is the acknowledge, because the driver commits what a drain returned before it drains again.
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
            let tail = redacted_tail(&s.screen_text(), &s.token, SCREEN_TAIL_CHARS);
            s.queue.push(SessionEvent::Stalled { screen_tail: tail });
        }
        let persisted = s.queue.iter().any(|e| event_to_json(e).is_some());
        if s.unsaved || persisted || s.pending_saved {
            s.save(self.hosts.as_ref(), "")?;
        }
        // The file is read to its end and nothing is pending: the host may end.
        if s.exited && !s.exit_done && s.at_end && !persisted {
            let _ = s.handle.exit_ack();
            s.exit_done = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_token_that_is_not_hex_is_still_masked_in_a_tail() {
        let token = "pf-seat-secret-value";
        let tail = redacted_tail(&format!("login {token} done"), token, 200);
        assert!(!tail.contains(token), "{tail}");
        assert!(tail.contains(REDACTED), "{tail}");
    }

    #[test]
    fn a_run_of_32_hex_digits_is_masked_when_the_token_is_not_known() {
        let tail = redacted_tail("key 0123456789abcdef0123456789abcdef end", "", 200);
        assert!(!tail.contains("0123456789abcdef"), "{tail}");
    }

    #[test]
    fn a_persisted_event_comes_back_as_it_was() {
        let events = [
            SessionEvent::Push {
                id: "u-push-1".into(),
                repo: "acme/widgets".into(),
                branch: "topic".into(),
                sha: "abc".into(),
                outcome: PushOutcome::Pushed,
            },
            SessionEvent::Push {
                id: "u-push-2".into(),
                repo: "acme/widgets".into(),
                branch: "other".into(),
                sha: "def".into(),
                outcome: PushOutcome::Failed("exit code 1".into()),
            },
            SessionEvent::DefaultBranchMoved {
                remote: "origin".into(),
                branch: "main".into(),
                before: "a".into(),
                after: String::new(),
            },
        ];
        for e in events {
            let json = event_to_json(&e).expect("kept");
            assert_eq!(event_from_json(&json), Some(e));
        }
        assert_eq!(event_to_json(&SessionEvent::Activity), None);
        assert_eq!(
            event_to_json(&SessionEvent::Stalled {
                screen_tail: String::new()
            }),
            None
        );
    }
}
