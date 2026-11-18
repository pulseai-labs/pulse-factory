//! Shared helpers of the host's integration tests. Each test file uses the detached launcher in its
//! own temporary runtime directory, so no test needs a systemd user manager.
#![allow(dead_code)]

use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use pf_host::launcher::{Detached, Launcher, StartSpec, Started};
use serde_json::{Value, json};

/// 32 bytes as hex: the attach key the tests hand to every host.
pub const KEY: &str = "5f1d0c9a7b3e4d2c8a6f1e0d9c7b5a3948372615f4e3d2c1b0a99887766554a3";

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A temporary directory, short enough for a Unix socket path, removed on drop.
pub struct Tmp {
    pub path: PathBuf,
}

impl Tmp {
    pub fn new() -> Tmp {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("pfh-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Tmp { path }
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The `pf-host` binary the tests start: the one `PF_HOST_BIN` names (the barrier check passes the
/// binary the orchestrator built), else the one Cargo built for this test.
pub fn host_bin() -> PathBuf {
    std::env::var_os("PF_HOST_BIN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_pf-host")))
}

pub fn pipes() -> Value {
    json!({"kind": "pipes"})
}

pub fn pty(rows: u16, cols: u16) -> Value {
    json!({"kind": "pty", "rows": rows, "cols": cols})
}

/// A launch record: the JSON file the host reads.
pub fn record(argv: &[&str], transport: Value) -> String {
    json!({
        "argv": argv,
        "cwd": "/",
        "env": {"PATH": "/usr/bin:/bin", "PF_TEST_VISIBLE": "yes"},
        "transport": transport,
    })
    .to_string()
}

/// A rig: a runtime directory, a state directory and a detached launcher. It stops every host it
/// started when it drops.
pub struct Rig {
    pub tmp: Tmp,
    pub rt: PathBuf,
    pub state: PathBuf,
    pub launcher: Detached,
    units: RefCell<Vec<String>>,
}

impl Rig {
    pub fn new() -> Rig {
        let tmp = Tmp::new();
        let rt = tmp.path.join("rt");
        let state = tmp.path.join("st");
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let launcher = Detached::new(rt.clone());
        Rig {
            tmp,
            rt,
            state,
            launcher,
            units: RefCell::new(Vec::new()),
        }
    }

    pub fn write_record(&self, name: &str, json: &str) -> PathBuf {
        let dir = self.tmp.path.join("rec");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.json"));
        std::fs::write(&path, json).unwrap();
        path
    }

    pub fn spec(&self, session: &str, record_json: &str) -> StartSpec {
        let record = self.write_record(session, record_json);
        StartSpec::new(
            "t",
            session,
            1,
            record,
            self.state.clone(),
            host_bin(),
            KEY.to_string(),
        )
    }

    pub fn start(&self, session: &str, record_json: &str) -> Started {
        self.start_with(session, record_json, |_| {})
    }

    pub fn start_with(
        &self,
        session: &str,
        record_json: &str,
        tweak: impl FnOnce(&mut StartSpec),
    ) -> Started {
        let mut spec = self.spec(session, record_json);
        tweak(&mut spec);
        let started = self
            .launcher
            .start(&spec)
            .unwrap_or_else(|e| panic!("the host did not start: {e}"));
        self.units.borrow_mut().push(started.unit.clone());
        started
    }

    /// Have the rig stop this unit when it drops, for a host the test did not start through the rig.
    pub fn track(&self, unit: &str) {
        self.units.borrow_mut().push(unit.to_string());
    }

    pub fn exit_file(&self, unit: &str) -> PathBuf {
        self.state
            .join("hosts")
            .join("exits")
            .join(format!("{unit}.json"))
    }

    /// Wait for the exit file and parse it.
    pub fn wait_exit_file(&self, unit: &str, timeout: Duration) -> Value {
        let path = self.exit_file(unit);
        assert!(
            wait_until(timeout, || path.exists()),
            "no exit file at {} after {timeout:?}",
            path.display()
        );
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for unit in self.units.borrow().iter() {
            let _ = self.launcher.stop(unit);
        }
    }
}

/// Poll `f` every 20 ms until it is true or the time is up.
pub fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn pid_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // A zombie is gone for our purpose: its process ended.
        Ok(stat) => !stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .trim_start()
            .starts_with('Z'),
        Err(_) => false,
    }
}

/// A bare socket to a host, for the tests of what the host sends before a client is known.
pub struct Raw {
    pub stream: UnixStream,
    pub buf: Vec<u8>,
}

impl Raw {
    pub fn connect(socket: &Path) -> Raw {
        let stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        Raw {
            stream,
            buf: Vec::new(),
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    /// The next line, without its newline. `None` on a timeout or the end of the stream.
    pub fn line(&mut self) -> Option<String> {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                return Some(String::from_utf8(line[..line.len() - 1].to_vec()).unwrap());
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return None;
                }
                Err(_) => return None,
            }
        }
    }

    /// The next line, waiting up to `within`.
    pub fn line_within(&mut self, within: Duration) -> Option<String> {
        let end = Instant::now() + within;
        loop {
            if let Some(line) = self.line() {
                return Some(line);
            }
            if Instant::now() >= end {
                return None;
            }
        }
    }

    /// Exactly `n` raw bytes, waiting up to `within`.
    pub fn exact(&mut self, n: usize, within: Duration) -> Option<Vec<u8>> {
        let end = Instant::now() + within;
        while self.buf.len() < n {
            if Instant::now() >= end {
                return None;
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(k) => self.buf.extend_from_slice(&chunk[..k]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return None,
            }
        }
        Some(self.buf.drain(..n).collect())
    }

    pub fn json(&mut self) -> Value {
        let line = self.line().expect("the host sent no line");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    /// True when the host closed the connection (an end of stream, not a timeout).
    pub fn closed(&mut self) -> bool {
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return true,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return true,
            }
        }
        false
    }
}

pub fn attach_line(key: &str) -> Vec<u8> {
    format!("{{\"t\":\"attach\",\"proto\":1,\"key\":\"{key}\"}}\n").into_bytes()
}
