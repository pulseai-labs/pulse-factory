//! The managed copy of the CLI: where it lives, whether it is the pinned binary, and what it
//! prints for `--version` (ADR-0014).
//!
//! With no override the binary is `<share_dir>/claude/<version>/claude`, never the user's own
//! `claude` and never one found on `PATH`. A missing copy, a copy whose size or SHA-256 differs
//! from the pin, or a platform with no pin is `RuntimeNotReady`, before any process starts. An
//! override runs with no SHA-256 check and is marked unqualified. Nothing here fetches anything.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use pf_engine::port::chat::ChatError;

use crate::pinned::{self, Pin};
use crate::sha256::hash_file;

/// The binary a chat process runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    pub bin: PathBuf,
    /// What `--version` printed, as a bare version.
    pub version: String,
    /// True for the managed copy at the pinned version and SHA-256.
    pub qualified: bool,
}

/// Start a process, trying again while the binary is busy for writing (`ETXTBSY`). Another thread
/// that forks at the moment a file is written holds its descriptor until its own `exec`, so a
/// binary written just before it runs can be busy for a few milliseconds.
pub(crate) fn spawn_retry(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 40 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

/// Run `bin --version` and read the version. The call has a time limit and a bare environment.
pub fn read_version(bin: &Path) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--version")
        .env_clear()
        .env("DISABLE_AUTOUPDATER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    let mut child =
        spawn_retry(&mut cmd).map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut text);
        text
    });
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} --version did not end in 10 s", bin.display()));
            }
            Err(e) => return Err(format!("cannot wait for {}: {e}", bin.display())),
        }
    }
    let text = reader.join().unwrap_or_default();
    text.split_whitespace()
        .find(|w| w.contains('.') && w.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "{} --version printed no version: {:?}",
                bin.display(),
                text.trim()
            )
        })
}

/// Resolves and verifies the binary. It remembers a verified copy by `(path, size, modification
/// time)`, so a copy is hashed once for the adapter's life and again after a change.
#[derive(Default)]
pub struct Resolver {
    verified: Mutex<Option<(PathBuf, u64, SystemTime)>>,
    hashes: AtomicUsize,
}

impl Resolver {
    /// How many times a binary was hashed. A test reads it.
    pub fn hash_count(&self) -> usize {
        self.hashes.load(Ordering::SeqCst)
    }

    pub fn resolve(
        &self,
        share_dir: &Path,
        pin: &Pin,
        claude_bin_override: Option<&Path>,
    ) -> Result<Runtime, ChatError> {
        if let Some(bin) = claude_bin_override {
            let version = read_version(bin).map_err(|e| {
                ChatError::Failed(format!(
                    "cannot read the version of the override binary: {e}"
                ))
            })?;
            return Ok(Runtime {
                bin: bin.to_path_buf(),
                version,
                qualified: false,
            });
        }
        let install = "Run Pulse Factory's install step for the chat runtime";
        let Some(key) = pinned::host_platform() else {
            return Err(ChatError::RuntimeNotReady(format!(
                "no pinned Claude Code binary for {}-{}. Pin one in pinned.rs, or name an override binary",
                std::env::consts::OS,
                std::env::consts::ARCH
            )));
        };
        let Some((sha, size)) = pin.platforms.get(key) else {
            return Err(ChatError::RuntimeNotReady(format!(
                "the pin names no Claude Code binary for platform {key}. Pin one in pinned.rs, or name an override binary"
            )));
        };
        let bin = share_dir
            .join("claude")
            .join(&pin.cli_version)
            .join("claude");
        let meta = std::fs::metadata(&bin).map_err(|e| {
            ChatError::RuntimeNotReady(format!(
                "the managed Claude Code copy {} is missing ({e}). {install}: it fetches version {} and checks its SHA-256",
                bin.display(),
                pin.cli_version
            ))
        })?;
        if meta.len() != *size {
            return Err(ChatError::RuntimeNotReady(format!(
                "the managed Claude Code copy {} has size {}, but the pin says {size}. {install} to fetch it again",
                bin.display(),
                meta.len()
            )));
        }
        let mtime = meta.modified().map_err(|e| {
            ChatError::RuntimeNotReady(format!(
                "cannot read the modification time of {}: {e}",
                bin.display()
            ))
        })?;
        let key_now = (bin.clone(), meta.len(), mtime);
        let known = self
            .verified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|k| *k == key_now);
        if !known {
            self.hashes.fetch_add(1, Ordering::SeqCst);
            let actual = hash_file(&bin).map_err(|e| {
                ChatError::RuntimeNotReady(format!(
                    "cannot read {} to check it: {e}. {install}",
                    bin.display()
                ))
            })?;
            if actual != *sha {
                return Err(ChatError::RuntimeNotReady(format!(
                    "the managed Claude Code copy {} has SHA-256 {actual}, but the pin says {sha}. {install} to fetch it again",
                    bin.display()
                )));
            }
            *self.verified.lock().unwrap_or_else(|e| e.into_inner()) = Some(key_now);
        }
        let version = read_version(&bin).map_err(|e| {
            ChatError::RuntimeNotReady(format!(
                "the managed copy {} cannot report its version: {e}. {install}",
                bin.display()
            ))
        })?;
        if version != pin.cli_version {
            return Err(ChatError::Refused(format!(
                "the managed copy {} prints version {version}, but the pin is {}",
                bin.display(),
                pin.cli_version
            )));
        }
        Ok(Runtime {
            bin,
            version,
            qualified: true,
        })
    }
}
