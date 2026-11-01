//! The PTY driver, on `portable-pty` (ADR-0008). It spawns a program in a PTY, reads its output
//! on a thread, writes to it and resizes it. Its calls block, so an async caller runs them with
//! `tokio::task::spawn_blocking`.

use std::io::{Read, Write};
use std::path::PathBuf;
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

/// One process in a PTY.
pub struct PtySession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    output: Arc<Mutex<Vec<u8>>>,
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
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
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
            pid,
        })
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

    /// End the process and wait for it.
    pub fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill();
    }
}
