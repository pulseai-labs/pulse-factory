//! Run `gh` as an argv list with no shell, and a time limit on every call.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The most stdout bytes the forge keeps. `gh` output is untrusted and may be large.
const MAX_STDOUT: u64 = 8 * 1024 * 1024;
/// The most stderr bytes the forge keeps.
const MAX_STDERR: u64 = 64 * 1024;

/// What a finished call gave back.
pub(crate) struct Output {
    /// The exit code. `None` when a signal ended the process.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why a call gave no output.
pub(crate) enum RunError {
    /// The program did not start.
    Spawn(String),
    /// The call did not finish within the limit. The process is killed.
    Timeout,
}

fn read_all<R: Read + Send + 'static>(pipe: R, cap: u64) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.take(cap).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// Run `program args…` and wait at most `timeout`. The child gets no stdin. When `path_prefix` is
/// set, it comes first on the child's `PATH`; the process-wide `PATH` stays as it is.
pub(crate) fn run(
    program: &OsStr,
    args: &[String],
    path_prefix: Option<&Path>,
    timeout: Duration,
) -> Result<Output, RunError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    if let Some(prefix) = path_prefix {
        let mut dirs = vec![prefix.to_path_buf()];
        if let Some(inherited) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&inherited));
        }
        let joined = std::env::join_paths(dirs).map_err(|e| RunError::Spawn(e.to_string()))?;
        command.env("PATH", joined);
    }
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|e| RunError::Spawn(format!("cannot start {}: {e}", program.to_string_lossy())))?;
    let stdout = child.stdout.take().map(|p| read_all(p, MAX_STDOUT));
    let stderr = child.stderr.take().map(|p| read_all(p, MAX_STDERR));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Spawn(format!("cannot wait for the process: {e}")));
            }
        }
    };
    // A grandchild may hold a pipe open. The limit covers the reads too.
    let left = timeout.saturating_sub(started.elapsed());
    let take = |rx: Option<mpsc::Receiver<Vec<u8>>>| match rx {
        Some(rx) => rx.recv_timeout(left).map_err(|_| RunError::Timeout),
        None => Ok(Vec::new()),
    };
    Ok(Output {
        code: status.code(),
        stdout: take(stdout)?,
        stderr: take(stderr)?,
    })
}
