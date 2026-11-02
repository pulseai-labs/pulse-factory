//! Commands as data (Approach 5). Every command the file names is an argv list, run under fixed
//! bounds: `before`, `after_first_turn`, `exit0` signals, `after_merge` and `plan_check`.
//!
//! - There is no shell string in the runner. Each placeholder renders into one argv item, so a
//!   value with spaces or `;` cannot split or add an argument.
//! - A value from a seat's record that starts with `-` is refused, so it cannot act as an option.
//! - A command has a timeout: 60 s by default and at most 600 s. A timed-out process is killed.
//! - Stdout and stderr are each kept to 64 KiB. The runner drains the rest.
//! - A failure is never retried.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_engine::Engine;
use pf_engine::admission::EngineError;
use pf_engine::render::{RenderContext, render};

/// The timeout of a command that sets none.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
/// The longest timeout a file may set.
pub const MAX_TIMEOUT_SECONDS: u64 = 600;
/// How much of stdout and of stderr the runner keeps.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// How long the runner waits for the output pipes after the process has ended. A grandchild that
/// holds a pipe open must not hold the runner.
const PIPE_GRACE: Duration = Duration::from_millis(750);

/// One command the file names: an argv with placeholders, and its timeout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
}

impl CommandSpec {
    /// A command with the default timeout.
    pub fn new(argv: &[&str]) -> CommandSpec {
        CommandSpec {
            argv: argv.iter().map(|s| (*s).to_string()).collect(),
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
        }
    }
}

/// What a command printed, and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutput {
    /// `None` when a signal ended the process.
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// True when the command printed more than the runner keeps.
    pub stdout_cut: bool,
    pub stderr_cut: bool,
}

/// Why a command did not run to a result, or did not end well.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandFailure {
    /// A placeholder cannot render: the record lacks a variable, or a parameter is missing.
    Render(String),
    /// A value from a unit record starts with `-`.
    Dash { position: usize },
    /// The program (the first argv item) comes from a unit record.
    ProgramFromRecord,
    /// The timeout is outside 1 to 600 seconds.
    BadTimeout(u64),
    /// The process could not start.
    Start(String),
    /// The process ran past its timeout and was killed.
    Timeout { seconds: u64 },
    /// The process ended with a non-zero code.
    Exit { code: i32, stderr_line: String },
    /// A signal ended the process.
    Signal,
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandFailure::Render(why) => write!(f, "cannot render: {why}"),
            CommandFailure::Dash { position } => write!(
                f,
                "argv item {position} comes from a unit record and starts with \"-\", so it could act as an option"
            ),
            CommandFailure::ProgramFromRecord => {
                write!(f, "the program may not come from a unit record")
            }
            CommandFailure::BadTimeout(n) => write!(
                f,
                "timeout {n}s is outside 1 to {MAX_TIMEOUT_SECONDS} seconds"
            ),
            CommandFailure::Start(why) => write!(f, "could not start: {why}"),
            CommandFailure::Timeout { seconds } => {
                write!(f, "timed out after {seconds}s and was killed")
            }
            CommandFailure::Exit { code, stderr_line } if stderr_line.is_empty() => {
                write!(f, "exited with code {code}")
            }
            CommandFailure::Exit { code, stderr_line } => {
                write!(f, "exited with code {code}: {stderr_line}")
            }
            CommandFailure::Signal => write!(f, "was ended by a signal"),
        }
    }
}

impl std::error::Error for CommandFailure {}

// ------------------------------------------------------------------ the process runner

struct Capture {
    buf: Arc<Mutex<Vec<u8>>>,
    cut: Arc<AtomicBool>,
    done: Receiver<()>,
}

/// Read a pipe on its own thread. Keep the first `MAX_OUTPUT_BYTES` and drain the rest.
fn capture<R: Read + Send + 'static>(mut pipe: R) -> Capture {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let cut = Arc::new(AtomicBool::new(false));
    let (tx, done) = mpsc::channel();
    let (b, c) = (Arc::clone(&buf), Arc::clone(&cut));
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut kept = b.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let take = n.min(MAX_OUTPUT_BYTES - kept.len());
                    kept.extend_from_slice(&chunk[..take]);
                    if take < n {
                        c.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        let _ = tx.send(());
    });
    Capture { buf, cut, done }
}

impl Capture {
    /// Wait until `deadline` for the pipe to close, then take what was read.
    fn finish(self, deadline: Instant) -> (Vec<u8>, bool) {
        let wait = deadline.saturating_duration_since(Instant::now());
        let _ = self.done.recv_timeout(wait);
        let bytes = self
            .buf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        (bytes, self.cut.load(Ordering::SeqCst))
    }
}

/// Run an argv that holds no placeholder. Nothing here reads a shell.
///
/// `Err` means the process did not end by itself with a status: it could not start, or it timed
/// out and was killed. A non-zero exit is a result.
pub fn run_argv(
    argv: &[String],
    stdin: Option<Vec<u8>>,
    timeout_seconds: u64,
) -> Result<RunOutput, CommandFailure> {
    if timeout_seconds == 0 || timeout_seconds > MAX_TIMEOUT_SECONDS {
        return Err(CommandFailure::BadTimeout(timeout_seconds));
    }
    let Some((program, args)) = argv.split_first() else {
        return Err(CommandFailure::Start("the argv is empty".to_string()));
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| CommandFailure::Start(e.to_string()))?;
    if let Some(bytes) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let (out, err) = match (child.stdout.take(), child.stderr.take()) {
        (Some(o), Some(e)) => (capture(o), capture(e)),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CommandFailure::Start("no output pipe".to_string()));
        }
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(CommandFailure::Start(e.to_string())),
        }
    };

    let grace = Instant::now() + PIPE_GRACE;
    let (stdout, stdout_cut) = out.finish(grace);
    let (stderr, stderr_cut) = err.finish(grace);
    match status {
        None => Err(CommandFailure::Timeout {
            seconds: timeout_seconds,
        }),
        Some(status) => Ok(RunOutput {
            exit_code: status.code(),
            stdout,
            stderr,
            stdout_cut,
            stderr_cut,
        }),
    }
}

// ------------------------------------------------------------------ rendering and running

/// Render each argv item of a command. A placeholder becomes one piece of text inside its item.
pub fn render_argv(spec: &CommandSpec, ctx: &RenderContext) -> Result<Vec<String>, CommandFailure> {
    let mut argv = Vec::with_capacity(spec.argv.len());
    for (i, template) in spec.argv.iter().enumerate() {
        let rendered = render(template, ctx).map_err(|e| CommandFailure::Render(e.to_string()))?;
        if rendered.from_record {
            if i == 0 {
                return Err(CommandFailure::ProgramFromRecord);
            }
            // A literal option in the template, such as `-C`, is the file's own.
            if rendered.text.starts_with('-') && !template.starts_with('-') {
                return Err(CommandFailure::Dash { position: i + 1 });
            }
        }
        argv.push(rendered.text);
    }
    Ok(argv)
}

/// Render and run one command. A non-zero exit is a result, not an error.
pub fn run_command(spec: &CommandSpec, ctx: &RenderContext) -> Result<RunOutput, CommandFailure> {
    let argv = render_argv(spec, ctx)?;
    run_argv(&argv, None, spec.timeout_seconds)
}

/// Run one command as an `exit0` signal: exit 0 means yes and a non-zero exit means "not yet".
/// A failure to start, a timeout and a refused value are still failures.
pub fn exit0(spec: &CommandSpec, ctx: &RenderContext) -> Result<bool, CommandFailure> {
    Ok(run_command(spec, ctx)?.exit_code == Some(0))
}

/// Why a hook stopped, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookFailure {
    pub hook: String,
    /// The command's place in the hook, counting from 1.
    pub place: usize,
    pub program: String,
    pub failure: CommandFailure,
}

impl std::fmt::Display for HookFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} command {} ({}) {}",
            self.hook, self.place, self.program, self.failure
        )
    }
}

impl std::error::Error for HookFailure {}

/// The first line of stderr, cut short, for a failure reason.
fn first_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    line.trim().chars().take(160).collect()
}

/// The name of the program for a message: rendered when it renders, else as written.
fn program_name(spec: &CommandSpec, ctx: &RenderContext) -> String {
    let template = spec.argv.first().map_or("", String::as_str);
    render(template, ctx).map_or_else(|_| template.to_string(), |r| r.text)
}

/// Run a hook: its commands in order. The first failure stops it, and nothing retries.
/// A non-zero exit is a failure here.
pub fn run_hook(
    hook: &str,
    specs: &[CommandSpec],
    ctx: &RenderContext,
) -> Result<Vec<RunOutput>, HookFailure> {
    let mut outputs = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let fail = |failure| HookFailure {
            hook: hook.to_string(),
            place: i + 1,
            program: program_name(spec, ctx),
            failure,
        };
        let out = run_command(spec, ctx).map_err(fail)?;
        match out.exit_code {
            Some(0) => outputs.push(out),
            Some(code) => {
                return Err(fail(CommandFailure::Exit {
                    code,
                    stderr_line: first_line(&out.stderr),
                }));
            }
            None => return Err(fail(CommandFailure::Signal)),
        }
    }
    Ok(outputs)
}

/// What a hook run did to the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookOutcome {
    Passed,
    /// A command failed. The unit is parked (or the run halted) with this reason.
    Failed(String),
}

/// Run a hook of a unit. A failure parks the unit with a reason that names the hook, the
/// command's place and the program.
pub fn run_unit_hook(
    engine: &Engine,
    unit_id: &str,
    hook: &str,
    specs: &[CommandSpec],
    ctx: &RenderContext,
) -> Result<HookOutcome, EngineError> {
    match run_hook(hook, specs, ctx) {
        Ok(_) => Ok(HookOutcome::Passed),
        Err(failure) => {
            let reason = failure.to_string();
            engine.park(unit_id, &reason)?;
            Ok(HookOutcome::Failed(reason))
        }
    }
}

/// Run a track's `after_merge` commands for a unit that is `done` already. A failure, a refused
/// value and a missing variable all halt the run through `halt_for`.
pub fn run_after_merge(
    engine: &Engine,
    unit_id: &str,
    specs: &[CommandSpec],
    ctx: &RenderContext,
) -> Result<HookOutcome, EngineError> {
    match run_hook("after_merge", specs, ctx) {
        Ok(_) => Ok(HookOutcome::Passed),
        Err(failure) => {
            let reason = format!("unit {unit_id}: {failure}");
            engine.halt_for(&format!("after_merge:{unit_id}"), &reason)?;
            Ok(HookOutcome::Failed(reason))
        }
    }
}

/// Why `plan_check` gave no count. `arm` refuses on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanCheckError(pub String);

impl std::fmt::Display for PlanCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plan_check failed: {}", self.0)
    }
}

impl std::error::Error for PlanCheckError {}

/// Run `plan_check`: a command that prints the item count as one whole number on stdout. It has
/// no unit, so its failure refuses `arm` instead of parking a unit.
pub fn run_plan_check(spec: &CommandSpec, ctx: &RenderContext) -> Result<u64, PlanCheckError> {
    let program = program_name(spec, ctx);
    let out =
        run_command(spec, ctx).map_err(|f| PlanCheckError(format!("command ({program}) {f}")))?;
    match out.exit_code {
        Some(0) => {}
        Some(code) => {
            return Err(PlanCheckError(format!(
                "command ({program}) {}",
                CommandFailure::Exit {
                    code,
                    stderr_line: first_line(&out.stderr),
                }
            )));
        }
        None => {
            return Err(PlanCheckError(format!(
                "command ({program}) {}",
                CommandFailure::Signal
            )));
        }
    }
    if out.stdout_cut {
        return Err(PlanCheckError(
            "the output is longer than the runner keeps".to_string(),
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let word = text.trim();
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_digit()) {
        let shown: String = word.chars().take(40).collect();
        return Err(PlanCheckError(format!(
            "expected one whole number on stdout, got {shown:?}"
        )));
    }
    word.parse::<u64>()
        .map_err(|_| PlanCheckError(format!("the count {word} is too large")))
}
