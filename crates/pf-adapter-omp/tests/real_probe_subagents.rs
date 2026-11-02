//! Real probe: turn end while OMP subagents run.
//!
//! A seat that has the `task` tool can start subagents. This probe launches OMP itself, with the
//! seat's flags and the `task` tool added, and watches whether `agent_end` (without
//! `willContinue`) fires while a subagent's command is still running. The adapter's seat list
//! has no `task`, so the probe cannot use `OmpAdapter::launch`.
//!
//! `#[ignore]`d. It runs only with `PF_REAL_OMP=1`. It reserves 3 sessions of the budget first:
//! the session and up to 2 subagents.
//! `PF_REAL_OMP=1 cargo test -p pf-adapter-omp --test real_probe_subagents -- --ignored --nocapture`

mod common;

use std::time::{Duration, Instant};

use common::*;
use pf_adapter_omp::probe;
use pf_adapter_omp::pty::{PtySession, SpawnSpec};

/// Count processes whose command line is `sleep 27`: the subagents' command.
fn running_children() -> usize {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter(|e| {
            std::fs::read(e.path().join("cmdline"))
                .map(|c| {
                    c.split(|b| *b == 0)
                        .collect::<Vec<_>>()
                        .starts_with(&[b"sleep".as_slice(), b"27".as_slice()])
                })
                .unwrap_or(false)
        })
        .count()
}

#[test]
#[ignore = "spends up to 3 real sessions; runs only with PF_REAL_OMP=1"]
fn turn_end_while_subagents_run() {
    if !probe::enabled() {
        return;
    }
    probe::reserve(3).expect("the budget has room for a session and 2 subagents");
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let hook = allow_hook(dir.path());
    let events = dir.path().join("events.jsonl");
    std::fs::write(&events, b"").unwrap();
    let ext = dir.path().join("pf-seat.ts");
    std::fs::write(&ext, pf_adapter_omp::EXTENSION_SOURCE).unwrap();
    let message = "Use the task tool to start 2 subagents at the same time, in the background if the tool allows it. Each subagent runs the bash command `sleep 27` and then replies OK. After you have started them, reply with the single word STARTED right away. Do not wait for them.";
    let tools = "read,bash,edit,write,grep,glob,task";
    let args: Vec<String> = [
        "--model",
        probe::PROBE_MODEL,
        "--thinking",
        "low",
        "--no-extensions",
        "--no-rules",
        "--no-skills",
        "--approval-mode",
        "yolo",
        "--tools",
        tools,
        "--hook",
        ext.to_str().unwrap(),
        "--session-dir",
        dir.path().join("omp").to_str().unwrap(),
        message,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let session = PtySession::spawn(&SpawnSpec {
        program: "omp".into(),
        args,
        cwd: scratch.path().to_path_buf(),
        env: vec![
            ("PF_EVENT_FILE".into(), events.display().to_string()),
            (
                "PF_INBOX_FILE".into(),
                dir.path().join("inbox").display().to_string(),
            ),
            ("PF_HOOK_CMD".into(), hook),
            ("PF_ALLOWED_TOOLS".into(), tools.into()),
        ],
        rows: 40,
        cols: 140,
    })
    .unwrap();
    let text = || std::fs::read_to_string(&events).unwrap_or_default();
    let t0 = Instant::now();
    // The first `agent_end` can have `willContinue: true`: that is not a turn end. Wait for one
    // without it, up to 2 minutes.
    let first_end = wait_until(Duration::from_secs(120), || {
        text().contains("\"ev\":\"agent_end\",\"willContinue\":false")
    });
    let alive_at_end = running_children();
    report(
        "subagents.first_agent_end",
        &format!("{first_end} after {:?}", t0.elapsed()),
    );
    report(
        "subagents.sleep_children_alive_at_first_agent_end",
        &alive_at_end.to_string(),
    );
    // Watch for a later turn that starts by itself when the subagents finish.
    std::thread::sleep(Duration::from_secs(35));
    report(
        "subagents.agent_starts",
        &text().matches("\"ev\":\"agent_start\"").count().to_string(),
    );
    report(
        "subagents.agent_ends",
        &text().matches("\"ev\":\"agent_end\"").count().to_string(),
    );
    report(
        "subagents.timeline",
        &format!(
            "\n{}",
            text()
                .lines()
                .filter(|l| [
                    "agent_start",
                    "agent_end",
                    "\"tool_call\"",
                    "\"bash\"",
                    "inventory"
                ]
                .iter()
                .any(|k| l.contains(k)))
                .map(|l| l.chars().take(200).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    session.kill();
    assert!(first_end);
}
