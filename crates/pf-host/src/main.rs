//! The `pf-host` binary: `pf-host run ...` is the host process, `pf-host stop-post ...` is the step
//! a systemd unit runs after the host's process ends. Both take `--name value` pairs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use pf_host::host::{LauncherKind, RunArgs};

fn flags(args: &[String]) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    let mut it = args.iter();
    while let Some(name) = it.next() {
        let Some(name) = name.strip_prefix("--") else {
            return Err(format!("unexpected argument {name}"));
        };
        let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
        map.insert(name.to_string(), value.clone());
    }
    Ok(map)
}

fn need<'a>(m: &'a HashMap<String, String>, name: &str) -> Result<&'a String, String> {
    m.get(name).ok_or_else(|| format!("--{name} is required"))
}

fn number<T: std::str::FromStr>(m: &HashMap<String, String>, name: &str) -> Result<T, String> {
    need(m, name)?
        .parse()
        .map_err(|_| format!("--{name} is not a number"))
}

fn run(args: &[String]) -> Result<(), String> {
    let m = flags(args)?;
    let launcher = match need(&m, "launcher")?.as_str() {
        "systemd" => LauncherKind::Systemd,
        "detached" => LauncherKind::Detached,
        other => return Err(format!("--launcher is systemd or detached, not {other}")),
    };
    let run = RunArgs {
        record: PathBuf::from(need(&m, "record")?),
        session: need(&m, "session")?.clone(),
        incarnation: number(&m, "incarnation")?,
        unit: need(&m, "unit")?.clone(),
        state_dir: PathBuf::from(need(&m, "state-dir")?),
        dir: PathBuf::from(need(&m, "dir")?),
        controller_cgroup: m.get("controller-cgroup").cloned(),
        secrets: PathBuf::from(need(&m, "secrets")?),
        replay_bytes: number(&m, "replay-bytes")?,
        attach_wait: Duration::from_millis(number(&m, "attach-wait-ms")?),
        exit_hold: Duration::from_millis(number(&m, "exit-hold-ms")?),
        launcher,
    };
    pf_host::host::run(&run)
}

fn stop_post(args: &[String]) -> Result<(), String> {
    let m = flags(args)?;
    let state = PathBuf::from(need(&m, "state-dir")?);
    pf_host::exitfile::stop_post(
        &state,
        need(&m, "unit")?,
        need(&m, "session")?,
        number(&m, "incarnation")?,
        |name| std::env::var(name).ok(),
    )
    .map(|_| ())
    .map_err(|e| format!("cannot write the exit file: {e}"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("stop-post") => stop_post(&args[1..]),
        _ => Err("usage: pf-host run|stop-post --name value ...".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("pf-host: {why}");
            ExitCode::FAILURE
        }
    }
}
