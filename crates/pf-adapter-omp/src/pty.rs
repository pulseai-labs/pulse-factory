//! The PTY driver, on `portable-pty` (ADR-0008). It spawns a program in a PTY, reads its output
//! on a thread, writes to it and resizes it. Its calls block, so an async caller runs them with
//! `tokio::task::spawn_blocking`.
//!
//! This is not the seat path. An OMP seat runs in a session host (`r2.s2.w4`, ADR-0011's amendment
//! of 2026-11-16: the PTY moves into the host), and the adapter drives it through the host port. The
//! module stays for `tests/pty_driver.rs` (ADR-0008's first-use proof of `portable-pty`) and for the
//! real probes, and it holds `wait_group_gone`, which the adapter's `stop` uses too.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::screen::strip_ansi;

/// How much output the driver keeps. Older output is dropped.
const KEEP_BYTES: usize = 256 * 1024;

/// What to start.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added to the environment the process inherits.
    pub env: Vec<(String, String)>,
    pub rows: u16,
    pub cols: u16,
}

/// The live processes of a process group, from `/proc`: a process is live when its `stat` names
/// the group and its state is not `Z` (a zombie) or `X` (dead). Linux only (`r2.s6` gives macOS its
/// own).
pub fn live_in_group(pgid: u32) -> Vec<u32> {
    let mut live = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return live;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // `pid (comm) state ppid pgrp ...`: the name may hold spaces and brackets, so read from the
        // last `)`.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or("");
        let _ppid = fields.next();
        let pgrp: Option<u32> = fields.next().and_then(|f| f.parse().ok());
        if pgrp == Some(pgid) && state != "Z" && state != "X" {
            live.push(pid);
        }
    }
    live.sort_unstable();
    live
}

/// Wait until no live process is left in the group `pgid`. `Ok` when the group is gone, and the
/// live process IDs when `within` ran out.
pub fn wait_group_gone(pgid: u32, within: Duration) -> Result<(), Vec<u32>> {
    let end = Instant::now() + within;
    loop {
        let live = live_in_group(pgid);
        if live.is_empty() {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(live);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Send `SIGKILL` to every process of the group `pgid`.
pub fn kill_group(pgid: u32) {
    let _ = std::process::Command::new("sh")
        .args(["-c", "kill -s KILL -- \"-$1\" 2>/dev/null; exit 0", "sh"])
        .arg(pgid.to_string())
        .status();
}

/// One process in a PTY.
pub struct PtySession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    output: Arc<Mutex<Vec<u8>>>,
    /// True once the reader thread has seen the end of the output.
    output_ended: Arc<AtomicBool>,
    pid: Option<u32>,
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl PtySession {
    pub fn spawn(spec: &SpawnSpec) -> Result<PtySession, String> {
        let pair = native_pty_system()
            .openpty(size(spec.rows, spec.cols))
            .map_err(|e| format!("cannot open a PTY: {e}"))?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("cannot start {}: {e}", spec.program.display()))?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("cannot read the PTY: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("cannot write the PTY: {e}"))?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let output_ended = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&output_ended);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        ended.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(n) => {
                        let mut out = sink.lock().unwrap_or_else(|e| e.into_inner());
                        out.extend_from_slice(&buf[..n]);
                        if out.len() > KEEP_BYTES {
                            let cut = out.len() - KEEP_BYTES;
                            out.drain(..cut);
                        }
                    }
                }
            }
        });
        let pid = child.process_id();
        Ok(PtySession {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            output,
            output_ended,
            pid,
        })
    }

    /// After the process ended, wait until the reader thread has taken all of its output. The
    /// exit code can be known before the last bytes reach the buffer, so a reader that needs the
    /// final screen waits here first. False on timeout.
    pub fn wait_output_end(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while !self.output_ended.load(Ordering::SeqCst) {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The output so far, without escape sequences.
    pub fn screen(&self) -> String {
        let out = self.output.lock().unwrap_or_else(|e| e.into_inner());
        strip_ansi(&out)
    }

    /// Wait until the screen holds `needle`. False on timeout.
    pub fn wait_for(&self, needle: &str, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if self.screen().contains(needle) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(bytes)
            .and_then(|()| w.flush())
            .map_err(|e| format!("cannot write the PTY: {e}"))
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        self.master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resize(size(rows, cols))
            .map_err(|e| format!("cannot resize the PTY: {e}"))
    }

    /// The exit code once the process has ended. A process that a signal ended gives 255.
    pub fn exit_code(&self) -> Option<i32> {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match child.try_wait() {
            Ok(Some(status)) => Some(i32::try_from(status.exit_code()).unwrap_or(255)),
            Ok(None) => None,
            Err(_) => Some(255),
        }
    }

    /// End the process and every process in its group, and wait for the leader. A PTY child is
    /// a session and group leader, so its group holds the children it started, such as `cargo`.
    pub fn kill(&self) {
        if let Some(pid) = self.pid {
            kill_group(pid);
        }
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
        drop(child);
        if let Some(pid) = self.pid {
            let _ = wait_group_gone(pid, Duration::from_secs(2));
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill();
    }
}
