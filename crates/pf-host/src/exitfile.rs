//! The exit file `<state>/hosts/exits/<unit>.json`: what is left of a host after it ends, for the
//! controller's start-up check. The host writes it when it ends. `pf-host stop-post`, which the
//! service manager runs after the host's process ends, writes one when the host could not, so a
//! host killed with `SIGKILL` still leaves one (Spike A, F1).

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::names;

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write `body` as the exit file of `unit`: a temporary file at mode 0600, then a rename. The
/// directory is made at mode 0700. With `only_if_absent`, an existing file is left alone and the
/// answer is `false`.
pub fn write(
    state: &Path,
    unit: &str,
    body: &Value,
    only_if_absent: bool,
) -> std::io::Result<bool> {
    let path = names::exit_path(state, unit);
    let dir = path.parent().expect("an exit path has a parent");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    if only_if_absent && path.exists() {
        return Ok(false);
    }
    let tmp = dir.join(format!("{unit}.json.tmp-{}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(body)?.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    if only_if_absent && path.exists() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(false);
    }
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

/// The step `ExecStopPost=` runs. `env` reads the variables systemd sets for it:
/// `SERVICE_RESULT`, `EXIT_CODE` and `EXIT_STATUS`. Returns whether it wrote a file.
pub fn stop_post(
    state: &Path,
    unit: &str,
    session: &str,
    incarnation: u64,
    env: impl Fn(&str) -> Option<String>,
) -> std::io::Result<bool> {
    let body = json!({
        "written_by": "stop-post",
        "unit": unit,
        "session": session,
        "incarnation": incarnation,
        "reason": "stop_post",
        "service_result": env("SERVICE_RESULT"),
        "exit_code": env("EXIT_CODE"),
        "exit_status": env("EXIT_STATUS"),
        "written_at": now_secs(),
    });
    write(state, unit, &body, true)
}
