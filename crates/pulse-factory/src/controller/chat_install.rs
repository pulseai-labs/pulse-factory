//! The managed Claude Code copy: the fetch and the check (ADR-0014, `r1.s3.w3` Approach 7).
//!
//! The chat runs only the managed copy at the pinned version and SHA-256, unless the operator names
//! an override binary. This module fetches that copy from the registry (Anthropic's official
//! per-platform package; Pulse Factory never ships it), checks it against the pin, and places it
//! atomically at `<share_dir>/claude/<cli version>/claude`. Nothing here ever runs a `claude` found
//! on `PATH`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pf_adapter_claude::pinned::{self, Pin};
use pf_adapter_claude::sha256::hash_file;

/// Where the managed copy of a pin lives.
pub fn copy_path(share_dir: &Path, pin: &Pin) -> PathBuf {
    share_dir
        .join("claude")
        .join(&pin.cli_version)
        .join("claude")
}

/// The pinned SHA-256 and size for this host's platform.
fn pinned_for_host(pin: &Pin) -> Result<(String, String, u64), String> {
    let platform = pinned::host_platform().ok_or_else(|| {
        format!(
            "no pinned Claude Code binary for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let (sha, size) = pin
        .platforms
        .get(platform)
        .ok_or_else(|| format!("the pin names no Claude Code binary for platform {platform}"))?;
    Ok((platform.to_string(), sha.clone(), *size))
}

/// The URL of the tarball of a pin in a registry.
pub fn tarball_url(registry: &str, platform: &str) -> String {
    let registry = registry.trim_end_matches('/');
    format!(
        "{registry}/@anthropic-ai/claude-agent-sdk-{platform}/-/claude-agent-sdk-{platform}-{}.tgz",
        pinned::SDK_VERSION
    )
}

/// A quick check for a view: is the copy there, and does its size match the pin? It reads the file
/// system only. The open does the full check. `None` means no problem was seen.
pub fn copy_problem(share_dir: &Path, pin: &Pin) -> Option<String> {
    let (_, _, size) = match pinned_for_host(pin) {
        Ok(p) => p,
        Err(why) => return Some(why),
    };
    let path = copy_path(share_dir, pin);
    match std::fs::metadata(&path) {
        Err(_) => Some(format!(
            "the managed Claude Code copy {} is missing; run `pulse-factory chat-runtime install`",
            path.display()
        )),
        Ok(m) if m.len() != size => Some(format!(
            "the managed Claude Code copy {} has size {}, but the pin says {size}; run `pulse-factory chat-runtime install`",
            path.display(),
            m.len()
        )),
        Ok(_) => None,
    }
}

/// The full check: size and SHA-256 against the pin. `Ok(true)` for a good copy, `Ok(false)` for a
/// missing or altered one.
pub fn copy_is_good(share_dir: &Path, pin: &Pin) -> Result<bool, String> {
    let (_, sha, size) = pinned_for_host(pin)?;
    let path = copy_path(share_dir, pin);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(false);
    };
    if meta.len() != size {
        return Ok(false);
    }
    let actual = hash_file(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(actual == sha)
}

/// A temporary directory that is removed when it goes out of scope, whatever happened.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Fetch and place the managed copy of `pin` into `share_dir` from `registry`. A copy that already
/// matches the pin is not fetched again. A failed download, a tarball with no `package/claude`, and
/// a size or SHA-256 that differs from the pin each place nothing, leave an existing file as it was,
/// remove the temporary files, and name the cause and the URL.
pub fn fetch_runtime(pin: &Pin, share_dir: &Path, registry: &str) -> Result<PathBuf, String> {
    let (platform, sha, size) = pinned_for_host(pin)?;
    let dest = copy_path(share_dir, pin);
    if copy_is_good(share_dir, pin)? {
        return Ok(dest);
    }
    let url = tarball_url(registry, &platform);
    let fail = |cause: String| format!("{cause} (from {url})");
    let claude_dir = share_dir.join("claude");
    std::fs::create_dir_all(&claude_dir)
        .map_err(|e| fail(format!("cannot make {}: {e}", claude_dir.display())))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let scratch = Scratch(claude_dir.join(format!(".fetch-{}-{nanos}", std::process::id())));
    std::fs::create_dir_all(&scratch.0)
        .map_err(|e| fail(format!("cannot make {}: {e}", scratch.0.display())))?;
    let tarball = scratch.0.join("package.tgz");

    let mut curl = Command::new("curl");
    curl.args(["-fsSL", "--max-time", "600"]);
    if url.starts_with("https://") {
        curl.args(["--proto", "=https"]);
    }
    curl.arg("-o").arg(&tarball).arg(&url);
    let downloaded = curl
        .stdin(Stdio::null())
        .output()
        .map_err(|e| fail(format!("cannot run curl: {e}")))?;
    if !downloaded.status.success() {
        let text = String::from_utf8_lossy(&downloaded.stderr);
        return Err(fail(format!(
            "the download failed: {}",
            text.trim().lines().last().unwrap_or("curl failed")
        )));
    }

    let unpacked = scratch.0.join("unpacked");
    std::fs::create_dir_all(&unpacked)
        .map_err(|e| fail(format!("cannot make {}: {e}", unpacked.display())))?;
    let extracted = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&unpacked)
        .arg("package/claude")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| fail(format!("cannot run tar: {e}")))?;
    let binary = unpacked.join("package").join("claude");
    if !extracted.status.success() || !binary.is_file() {
        return Err(fail("the tarball holds no package/claude".to_string()));
    }
    let got_size = std::fs::metadata(&binary)
        .map_err(|e| fail(format!("cannot read the unpacked binary: {e}")))?
        .len();
    if got_size != size {
        return Err(fail(format!(
            "the binary has size {got_size}, but the pin says {size}"
        )));
    }
    let got_sha =
        hash_file(&binary).map_err(|e| fail(format!("cannot read the unpacked binary: {e}")))?;
    if got_sha != sha {
        return Err(fail(format!(
            "the binary has SHA-256 {got_sha}, but the pin says {sha}"
        )));
    }
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| fail(format!("cannot set the mode of the binary: {e}")))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| fail(format!("cannot make {}: {e}", parent.display())))?;
    }
    std::fs::rename(&binary, &dest)
        .map_err(|e| fail(format!("cannot place {}: {e}", dest.display())))?;
    Ok(dest)
}

/// `pulse-factory chat-runtime install`: run [`fetch_runtime`] for the release pin, `$PF_SHARE_DIR`
/// and `$PF_CHAT_REGISTRY`, in the CLI process, with no controller.
pub fn install_from_env() -> Result<PathBuf, String> {
    let vars = std::env::vars().collect();
    let (env, _) = super::chats::parse_chat_env(&vars, None);
    fetch_runtime(&Pin::release(), &env.share_dir, &env.registry)
}

impl super::Controller {
    /// At controller start, with a registry set (production): check the managed copy on a
    /// background thread and, when it is missing or altered, fetch it. The outcome is one stderr
    /// line. Until a good copy is in place, an open is `RuntimeNotReady` with the adapter's text.
    /// Nothing here falls back to a `claude` on `PATH`.
    pub(super) fn check_managed_copy(&self) {
        let (Some(registry), true) = (self.cfg.chat_registry.clone(), self.cfg.chat_managed) else {
            return;
        };
        let share = self.cfg.chat_share_dir.clone();
        let pin = self.cfg.chat_pin.clone();
        let spawned = std::thread::Builder::new()
            .name("chat-runtime-check".to_string())
            .spawn(move || match copy_is_good(&share, &pin) {
                Ok(true) => {}
                Ok(false) | Err(_) => match fetch_runtime(&pin, &share, &registry) {
                    Ok(path) => eprintln!(
                        "pulse-factory: the chat runtime Claude Code {} is installed at {}",
                        pin.cli_version,
                        path.display()
                    ),
                    Err(why) => eprintln!(
                        "pulse-factory: the chat runtime is not installed: {why}; run `pulse-factory chat-runtime install`"
                    ),
                },
            });
        if let Err(e) = spawned {
            eprintln!("pulse-factory: the chat runtime check cannot start: {e}");
        }
    }
}
