//! AC-3: the events the extension writes and what the adapter makes of them. A stub `omp`
//! starts the session, and the test appends scripted event lines to the session's event file.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::*;
use pf_adapter_omp::OmpAdapter;
use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{AgentError, AgentPort, SessionEvent, SessionRef, SessionState};

struct Started {
    _dir: tempfile::TempDir,
    adapter: OmpAdapter,
    session: SessionRef,
    events: PathBuf,
}

fn start(extra: &[(&str, &str)]) -> Started {
    let dir = tempfile::tempdir().unwrap();
    let omp = stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN);
    let adapter = adapter(dir.path(), omp);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let session = adapter.launch(&plan(&work, extra)).unwrap();
    let events = adapter.event_file(&session).unwrap();
    Started {
        _dir: dir,
        adapter,
        session,
        events,
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare remote with `main`, a clone of it with a branch `topic` that holds one more commit.
fn scratch_repo(root: &Path) -> (PathBuf, PathBuf) {
    let remote = root.join("remote.git");
    let work = root.join("clone");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "-b", "main"]);
    git(root, &["clone", remote.to_str().unwrap(), "clone"]);
    git(&work, &["checkout", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "a").unwrap();
    git(&work, &["add", "a.txt"]);
    git(&work, &["commit", "-m", "first"]);
    git(&work, &["push", "-u", "origin", "main"]);
    git(&work, &["checkout", "-b", "topic"]);
    std::fs::write(work.join("b.txt"), "b").unwrap();
    git(&work, &["add", "b.txt"]);
    git(&work, &["commit", "-m", "second"]);
    (remote, work)
}

fn drain(s: &Started) -> Vec<SessionEvent> {
    s.adapter.drain_events(&s.session).unwrap()
}

fn bash_line(command: &str, exit_code: i64) -> String {
    serde_json::json!({"ev":"bash","t":10,"command":command,"exitCode":exit_code}).to_string()
}

#[test]
fn will_continue_true_is_not_a_turn_end() {
    let s = start(&[]);
    // The stub reported `agent_start`: the first turn runs.
    let st = s.adapter.status(&s.session).unwrap();
    assert!(st.first_turn_started);
    assert_eq!(st.state, SessionState::Working);
    assert!(!st.turn_ended);

    append(
        &s.events,
        &[r#"{"ev":"agent_end","t":5,"willContinue":true}"#],
    );
    let st = s.adapter.status(&s.session).unwrap();
    assert!(!st.turn_ended, "willContinue true goes on");
    assert_eq!(st.state, SessionState::Working);

    append(
        &s.events,
        &[r#"{"ev":"agent_end","t":6,"willContinue":false}"#],
    );
    let st = s.adapter.status(&s.session).unwrap();
    assert!(st.turn_ended);
    assert_eq!(st.state, SessionState::Idle);

    // A turn that does not say `willContinue` ends too, as the spike's logs show.
    append(&s.events, &[r#"{"ev":"agent_start","t":7}"#]);
    assert!(!s.adapter.status(&s.session).unwrap().turn_ended);
    append(&s.events, &[r#"{"ev":"agent_end","t":8}"#]);
    assert!(s.adapter.status(&s.session).unwrap().turn_ended);
}

#[test]
fn usage_sums_each_assistant_message_end() {
    let s = start(&[]);
    append(
        &s.events,
        &[
            r#"{"ev":"message_end","t":3,"role":"assistant","usage":{"input":100,"output":10,"cacheRead":50,"cacheWrite":5,"cost":{"total":0.0013}}}"#,
            r#"{"ev":"message_end","t":4,"role":"user","usage":{"input":999,"output":999}}"#,
            r#"{"ev":"message_end","t":5,"role":"assistant","usage":{"input":20,"output":2,"cacheRead":70,"cacheWrite":0,"cost":{"total":0.0002}}}"#,
        ],
    );
    let u = s.adapter.usage(&s.session).unwrap();
    assert_eq!(u.input_tokens, 120);
    assert_eq!(u.output_tokens, 12);
    assert_eq!(u.cache_read_tokens, 120);
    assert_eq!(u.cache_write_tokens, 5);
    assert_eq!(u.cost_micros, 1500);
}

#[test]
fn a_delivery_reaches_an_idle_session_only() {
    let s = start(&[]);
    let inbox = s.adapter.inbox_file(&s.session).unwrap();
    let err = s.adapter.deliver(&s.session, "too early").unwrap_err();
    assert!(matches!(err, AgentError::Failed(_)), "{err:?}");
    assert!(lines(&inbox).is_empty(), "nothing reached the inbox");

    append(
        &s.events,
        &[r#"{"ev":"agent_end","t":5,"willContinue":false}"#],
    );
    s.adapter.deliver(&s.session, "next task").unwrap();
    let got = lines(&inbox);
    assert_eq!(got.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
    assert_eq!(v["text"], "next task");

    // The session is busy until its new turn ends: a second message waits for the caller.
    assert!(s.adapter.deliver(&s.session, "again").is_err());
    assert_eq!(lines(&inbox).len(), 1);
}

#[test]
fn a_push_event_gives_one_push_with_id_repo_branch_sha_and_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(&[("push_dir", work.to_str().unwrap())]);
    let topic = git(&work, &["rev-parse", "topic"]);

    append(
        &s.events,
        &[&bash_line(
            &format!("git -C {} push origin topic", work.display()),
            0,
        )],
    );
    let pushes: Vec<_> = drain(&s)
        .into_iter()
        .filter(|e| matches!(e, SessionEvent::Push { .. }))
        .collect();
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    let SessionEvent::Push {
        id,
        repo,
        branch,
        sha,
        outcome,
    } = &pushes[0]
    else {
        unreachable!()
    };
    assert!(!id.is_empty());
    assert_eq!(
        repo, "acme/widgets",
        "the declared repo, not the seat's text"
    );
    assert_eq!(branch, "topic");
    assert_eq!(sha, &topic);
    assert_eq!(outcome, &PushOutcome::Pushed);

    // A failed push gives one event with its outcome, and its ID is fresh.
    append(
        &s.events,
        &[&bash_line(
            &format!("git -C {} push origin topic", work.display()),
            1,
        )],
    );
    let failed: Vec<_> = drain(&s)
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Push { id, outcome, .. } => Some((id, outcome)),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1);
    assert_ne!(&failed[0].0, id);
    assert!(
        matches!(&failed[0].1, PushOutcome::Failed(why) if why.contains("exit code 1")),
        "{failed:?}"
    );
}

#[test]
fn a_target_in_variables_is_measured_at_push_dir_and_push_branch() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(&[
        ("push_dir", work.to_str().unwrap()),
        ("push_branch", "topic"),
    ]);
    let topic = git(&work, &["rev-parse", "topic"]);
    append(
        &s.events,
        &[&bash_line(r#"git -C "$wt" push -u origin "$b""#, 0)],
    );
    let pushes: Vec<_> = drain(&s)
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Push {
                branch,
                sha,
                outcome,
                ..
            } => Some((branch, sha, outcome)),
            _ => None,
        })
        .collect();
    assert_eq!(
        pushes,
        vec![("topic".to_string(), topic, PushOutcome::Pushed)]
    );
}

#[test]
fn a_malformed_line_is_skipped_logged_through_redact_and_never_run() {
    let s = start(&[]);
    let marker = s.events.parent().unwrap().join("pwned");
    let cmd = format!("touch {}", marker.display());
    append(
        &s.events,
        &[
            &cmd,
            &format!(
                r#"{{"ev":"bash","command":"{cmd} ghp_abcdefghijklmnopqrstuvwxyz0123456789","#
            ),
            r#"{"ev":"message_end","t":3,"role":"assistant","usage":{"input":7,"output":1,"cost":{"total":0}}}"#,
        ],
    );
    // The good line after the bad ones still counts.
    assert_eq!(s.adapter.usage(&s.session).unwrap().input_tokens, 7);
    assert!(!marker.exists(), "the text of a line never runs");
    let log = std::fs::read_to_string(s.adapter.log_file(&s.session).unwrap()).unwrap();
    assert!(log.contains("skipped"), "{log}");
    assert!(
        !log.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "the log holds no secret: {log}"
    );
}

#[test]
fn a_session_with_no_activity_reports_one_stall_with_a_redacted_screen_tail() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let c = Arc::clone(&clock);
    let body = r#"echo 'Press enter to continue GH_TOKEN=secretvalue123'
echo '{"ev":"agent_start","t":1000000}' >> "$PF_EVENT_FILE"
exec sleep 600"#;
    let omp = stub_omp(dir.path(), "18.11.3", body);
    let adapter = adapter_with_clock(dir.path(), omp, Arc::new(move || c.load(Ordering::SeqCst)));
    let session = adapter
        .launch(&plan(dir.path(), &[("stall_minutes", "1")]))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while !adapter
        .screen_tail(&session, 450)
        .unwrap()
        .contains("Press enter")
    {
        assert!(Instant::now() < end, "the stub printed nothing");
        std::thread::sleep(Duration::from_millis(20));
    }

    clock.store(1_000_000 + 59_000, Ordering::SeqCst);
    assert!(
        !adapter
            .drain_events(&session)
            .unwrap()
            .iter()
            .any(|e| matches!(e, SessionEvent::Stalled { .. })),
        "inside the limit"
    );
    clock.store(1_000_000 + 61_000, Ordering::SeqCst);
    let events = adapter.drain_events(&session).unwrap();
    let tails: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Stalled { screen_tail } => Some(screen_tail.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tails.len(), 1, "{events:?}");
    assert!(tails[0].contains("Press enter to continue"), "{}", tails[0]);
    assert!(!tails[0].contains("secretvalue123"), "{}", tails[0]);
    assert!(tails[0].chars().count() <= 450);
    // One report for one quiet period.
    clock.store(1_000_000 + 120_000, Ordering::SeqCst);
    assert!(
        !adapter
            .drain_events(&session)
            .unwrap()
            .iter()
            .any(|e| matches!(e, SessionEvent::Stalled { .. }))
    );
    // New activity ends the quiet period, and the next one reports again.
    let events_file = adapter.event_file(&session).unwrap();
    append(
        &events_file,
        &[r#"{"ev":"tool_call","t":1120000,"tool":"read"}"#],
    );
    clock.store(1_120_000 + 61_000, Ordering::SeqCst);
    assert!(
        adapter
            .drain_events(&session)
            .unwrap()
            .iter()
            .any(|e| matches!(e, SessionEvent::Stalled { .. }))
    );
}

#[test]
fn the_extension_file_is_valid_javascript_when_node_is_installed() {
    let Ok(out) = Command::new("node").arg("--version").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("pf-seat.mjs");
    std::fs::write(&copy, pf_adapter_omp::EXTENSION_SOURCE).unwrap();
    let check = Command::new("node")
        .arg("--check")
        .arg(&copy)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}
