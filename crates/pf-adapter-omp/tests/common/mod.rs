//! Helpers for the adapter tests. Every test uses a stub `omp` script and spends nothing.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pf_adapter_omp::{OmpAdapter, OmpConfig};
use pf_engine::port::agent::LaunchPlan;

/// Write a stub `omp` into `dir`. `--version` prints `omp/<version>`. Any other call runs `body`,
/// a POSIX shell snippet. `$DIR` is the stub's directory.
pub fn stub_omp(dir: &Path, version: &str, body: &str) -> PathBuf {
    let path = dir.join("omp");
    let script = format!(
        "#!/bin/sh\nDIR='{}'\nif [ \"$1\" = \"--version\" ]; then echo \"omp/{}\"; exit 0; fi\n{}\n",
        dir.display(),
        version,
        body
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The usual stub: record argv, environment and token, report a first turn, then wait.
pub const STUB_STARTS_A_TURN: &str = r#"printf '%s\n' "$@" > "$DIR/argv"
env > "$DIR/env"
printf '%s\n' "$PF_SEAT_TOKEN" >> "$DIR/tokens"
echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
exec sleep 600"#;

pub fn adapter(dir: &Path, omp: PathBuf) -> OmpAdapter {
    let mut cfg = OmpConfig::new(dir.join("state"));
    cfg.omp_bin = omp;
    cfg.repo = "acme/widgets".to_string();
    OmpAdapter::new(cfg)
}

pub fn adapter_with_clock(
    dir: &Path,
    omp: PathBuf,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
) -> OmpAdapter {
    let mut cfg = OmpConfig::new(dir.join("state"));
    cfg.omp_bin = omp;
    cfg.repo = "acme/widgets".to_string();
    cfg.clock_ms = clock;
    OmpAdapter::new(cfg)
}

pub fn plan(cwd: &Path, extra: &[(&str, &str)]) -> LaunchPlan {
    let mut settings: BTreeMap<String, String> = BTreeMap::new();
    settings.insert("cwd".into(), cwd.display().to_string());
    settings.insert("message".into(), "Do the work item.".into());
    settings.insert("first_turn_seconds".into(), "20".into());
    settings.insert("stall_minutes".into(), "10".into());
    for (k, v) in extra {
        settings.insert((*k).to_string(), (*v).to_string());
    }
    LaunchPlan {
        run_id: "run-1".into(),
        unit_id: "u1".into(),
        stage: "implement".into(),
        role: "implementer".into(),
        model: "zai/glm-5.3-flash".into(),
        runtime: "omp".into(),
        attempt: 1,
        settings,
    }
}

pub fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Append event lines to a session's event file.
pub fn append(path: &Path, lines: &[&str]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

pub fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
        && std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.contains(") Z "))
            .unwrap_or(false)
}

// ---- helpers for the real probes (`tests/real_probe_*.rs`)

use std::time::{Duration, Instant};

/// Wait until `cond` is true. False on timeout.
pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A rule hook that allows every call. The probes do not test denial: the spike did (ADR-0011).
pub fn allow_hook(dir: &Path) -> String {
    let path = dir.join("allow-hook.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ncat >/dev/null\necho '{\"decision\":\"allow\"}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

/// An adapter for a real `omp`, with its state under `dir`.
pub fn real_adapter(dir: &Path) -> OmpAdapter {
    let mut cfg = OmpConfig::new(dir.join("state"));
    cfg.repo = "probe/repo".to_string();
    // The probes observe what OMP does with MCP config, so the static refusal is off here.
    cfg.isolation_check = false;
    OmpAdapter::new(cfg)
}

/// A probe plan: the probe model, low thinking, a scratch cwd and the allow hook.
pub fn probe_plan(dir: &Path, cwd: &Path, message: &str, extra: &[(&str, &str)]) -> LaunchPlan {
    let hook = allow_hook(dir);
    let mut p = plan(
        cwd,
        &[
            ("message", message),
            ("thinking", "low"),
            ("first_turn_seconds", "90"),
            ("env.PF_HOOK_CMD", &hook),
            ("env.PF_PROBE_SHAPE", "1"),
        ],
    );
    p.model = pf_adapter_omp::probe::PROBE_MODEL.to_string();
    for (k, v) in extra {
        p.settings.insert((*k).to_string(), (*v).to_string());
    }
    p
}

/// Print a probe result in a fixed form, for the report.
pub fn report(name: &str, text: &str) {
    println!("PROBE-RESULT {name}: {text}");
}
