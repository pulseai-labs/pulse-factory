//! Stage signals (Approach 6).
//!
//! [`evaluate`] is one function. It takes a stage, the rendering context, a probe for the file
//! system and for commands, and the session's state. It returns exactly one verdict: keep
//! waiting, done (with an optional PR), or park with a reason.
//!
//! The order of the checks:
//! 1. The stop file parks, with its text as the reason. It wins over every other condition.
//! 2. A `park_on` file parks, with its reason.
//! 3. When every `done` condition holds, the stage is done.
//! 4. A session whose turn ended, with none of the above, parks with "seat ended with no done signal".
//! 5. Otherwise the stage keeps waiting.
//!
//! In this version a gap, a correction packet and a red gate all park. Requests that answer them
//! come later.

use std::io::Read;

use pf_engine::Engine;
use pf_engine::admission::EngineError;
use pf_engine::port::forge::PrRef;
use pf_engine::render::{RenderContext, render};
use pf_engine::status::one_line_reason;
use pf_engine::unit::Signal;

use crate::commands::{CommandFailure, CommandSpec, MAX_OUTPUT_BYTES, render_argv, run_argv};
use crate::file::{DoneCond, StageSpec};

/// The reason of a session that ends with no signal.
pub const REASON_NO_DONE_SIGNAL: &str = "seat ended with no done signal";

/// What the adapter says about the stage's session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionState {
    /// A turn ended and no other turn follows.
    pub turn_ended: bool,
    /// The session's process ended.
    pub ended: bool,
}

/// What a stage's signals say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StageVerdict {
    Wait,
    Done {
        /// The PR that a `pr` condition printed.
        pr: Option<PrRef>,
        /// The signal that the engine records for the stage.
        signal: Signal,
    },
    Park {
        reason: String,
    },
}

/// The file system and the commands, as a stage's signals see them.
pub trait Probe {
    fn file_exists(&self, path: &str) -> bool;
    /// The text of a file, bounded.
    fn read_text(&self, path: &str) -> Result<String, String>;
    /// Run a rendered argv. `Ok(true)` for exit 0, `Ok(false)` for any other exit.
    fn exit0(&self, argv: &[String], timeout_seconds: u64) -> Result<bool, CommandFailure>;
    /// Run a rendered argv and return its stdout, or `None` for a non-zero exit.
    fn stdout(
        &self,
        argv: &[String],
        timeout_seconds: u64,
    ) -> Result<Option<String>, CommandFailure>;
}

/// The real file system and the real command runner.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn file_exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }

    fn read_text(&self, path: &str) -> Result<String, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(MAX_OUTPUT_BYTES as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    fn exit0(&self, argv: &[String], timeout_seconds: u64) -> Result<bool, CommandFailure> {
        Ok(run_argv(argv, None, timeout_seconds)?.exit_code == Some(0))
    }

    fn stdout(
        &self,
        argv: &[String],
        timeout_seconds: u64,
    ) -> Result<Option<String>, CommandFailure> {
        let out = run_argv(argv, None, timeout_seconds)?;
        Ok((out.exit_code == Some(0)).then(|| String::from_utf8_lossy(&out.stdout).to_string()))
    }
}

/// Read a PR reference: `https://<host>/<owner>/<repo>/pull/<n>` or `<owner>/<repo>#<n>`.
pub fn parse_pr(text: &str) -> Option<PrRef> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let (repo, number) = if let Some(rest) = text.strip_prefix("https://") {
        let parts: Vec<&str> = rest.split('/').collect();
        match parts.as_slice() {
            [host, owner, repo, "pull", number] if !host.is_empty() => {
                (format!("{owner}/{repo}"), *number)
            }
            _ => return None,
        }
    } else {
        let (repo, number) = text.split_once('#')?;
        (repo.to_string(), number)
    };
    let mut halves = repo.split('/');
    let (owner, name) = (halves.next()?, halves.next()?);
    if halves.next().is_some() || owner.is_empty() || name.is_empty() {
        return None;
    }
    let number: u64 = number.parse().ok().filter(|n| *n > 0)?;
    Some(PrRef { repo, number })
}

fn park(reason: impl AsRef<str>) -> StageVerdict {
    StageVerdict::Park {
        reason: one_line_reason(reason.as_ref()),
    }
}

/// Decide what a stage's signals say now.
pub fn evaluate(
    stage: &StageSpec,
    ctx: &RenderContext,
    probe: &dyn Probe,
    session: &SessionState,
) -> StageVerdict {
    let text = |template: &str| render(template, ctx).map(|r| r.text);
    let broken = |what: &str, why: &dyn std::fmt::Display| {
        park(format!("stage {} {what}: {why}", stage.name))
    };

    // 1. The stop file.
    let stop = match text(&stage.stop_file) {
        Ok(p) => p,
        Err(e) => return broken("stop_file cannot render", &e),
    };
    if probe.file_exists(&stop) {
        return match probe.read_text(&stop) {
            Ok(content) => park(content),
            Err(e) => park(format!(
                "the stop file {stop} exists and cannot be read: {e}"
            )),
        };
    }

    // 2. A park file.
    for entry in &stage.park_on {
        let path = match text(&entry.file) {
            Ok(p) => p,
            Err(e) => return broken("park_on file cannot render", &e),
        };
        if probe.file_exists(&path) {
            return match text(&entry.reason) {
                Ok(reason) => park(reason),
                Err(e) => broken("park_on reason cannot render", &e),
            };
        }
    }

    // 3. The done conditions. All must hold.
    let mut pr: Option<PrRef> = None;
    let mut signal: Option<Signal> = None;
    for (i, cond) in stage.done.iter().enumerate() {
        let place = i + 1;
        let command_failed = |spec: &CommandSpec, f: CommandFailure| {
            let program = spec.argv.first().map_or("", String::as_str);
            park(format!(
                "stage {} done condition {place} ({program}) {f}",
                stage.name
            ))
        };
        match cond {
            DoneCond::File(template) => {
                let path = match text(template) {
                    Ok(p) => p,
                    Err(e) => return broken(&format!("done condition {place} cannot render"), &e),
                };
                if !probe.file_exists(&path) {
                    return not_done(session);
                }
                signal.get_or_insert(Signal::DoneFile { path });
            }
            DoneCond::Exit0(spec) => {
                let argv = match render_argv(spec, ctx) {
                    Ok(a) => a,
                    Err(f) => return command_failed(spec, f),
                };
                match probe.exit0(&argv, spec.timeout_seconds) {
                    Ok(true) => {
                        signal.get_or_insert(Signal::ExitCode { code: 0 });
                    }
                    // A non-zero exit means "not yet".
                    Ok(false) => return not_done(session),
                    Err(f) => return command_failed(spec, f),
                }
            }
            DoneCond::Pr { command, .. } => {
                let argv = match render_argv(command, ctx) {
                    Ok(a) => a,
                    Err(f) => return command_failed(command, f),
                };
                match probe.stdout(&argv, command.timeout_seconds) {
                    Ok(Some(printed)) => match parse_pr(&printed) {
                        Some(found) => {
                            pr.get_or_insert(found);
                            signal.get_or_insert(Signal::PrState {
                                state: "recorded".to_string(),
                            });
                        }
                        None => return not_done(session),
                    },
                    Ok(None) => return not_done(session),
                    Err(f) => return command_failed(command, f),
                }
            }
        }
    }
    match signal {
        Some(signal) => StageVerdict::Done { pr, signal },
        // A stage with no done condition cannot be done by its signals. The file parser refuses it.
        None => not_done(session),
    }
}

/// No done condition holds yet: wait, or park when the session is over.
fn not_done(session: &SessionState) -> StageVerdict {
    if session.turn_ended || session.ended {
        park(REASON_NO_DONE_SIGNAL)
    } else {
        StageVerdict::Wait
    }
}

/// What `apply` did to the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyResult {
    Waiting,
    /// The stage's signal was recorded.
    Signalled,
    /// The unit was parked with this reason.
    Parked(String),
}

/// Carry out a verdict on a unit's running stage.
pub fn apply(
    engine: &Engine,
    unit_id: &str,
    stage: &str,
    verdict: &StageVerdict,
) -> Result<ApplyResult, EngineError> {
    match verdict {
        StageVerdict::Wait => Ok(ApplyResult::Waiting),
        StageVerdict::Done { pr, signal } => {
            engine.signal(unit_id, stage, signal.clone(), pr.clone())?;
            Ok(ApplyResult::Signalled)
        }
        StageVerdict::Park { reason } => {
            engine.park(unit_id, reason)?;
            let stored = one_line_reason(reason);
            Ok(ApplyResult::Parked(stored))
        }
    }
}
