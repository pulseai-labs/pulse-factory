//! AC-11: the paper adapter runs a seat script as a real process, with no PTY and no model. A
//! script writes one JSON line for each event to `$PF_PAPER_EVENTS`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pf_adapter_fake::{PaperAdapter, PaperConfig};
use pf_engine::port::agent::*;

fn setup(script: Option<&str>) -> (tempfile::TempDir, PaperAdapter) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("seats")).unwrap();
    if let Some(body) = script {
        std::fs::write(dir.path().join("seats/implementer.sh"), body).unwrap();
    }
    let adapter = PaperAdapter::new(PaperConfig::new(dir.path().to_path_buf(), vec![]));
    (dir, adapter)
}

fn plan(cwd: &Path, extra: &[(&str, &str)]) -> LaunchPlan {
    let mut settings: BTreeMap<String, String> = BTreeMap::new();
    settings.insert("cwd".into(), cwd.display().to_string());
    for (k, v) in extra {
        settings.insert((*k).to_string(), (*v).to_string());
    }
    LaunchPlan {
        run_id: "r1".into(),
        unit_id: "u1".into(),
        stage: "implement".into(),
        role: "implementer".into(),
        model: "paper".into(),
        runtime: "omp".into(),
        attempt: 1,
        settings,
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

#[test]
fn a_session_runs_its_script_with_the_plans_cwd_and_env() {
    let (dir, a) = setup(Some(
        "pwd > \"$OUT/cwd.txt\"\necho \"$GREETING\" > \"$OUT/greeting.txt\"\nenv | grep -c '^PF_PAPER_EVENTS=' > \"$OUT/events-var.txt\"\n",
    ));
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let s = a
        .launch(&plan(
            &cwd,
            &[
                ("env.OUT", out.to_str().unwrap()),
                ("env.GREETING", "hello paper"),
            ],
        ))
        .unwrap();
    wait_until("the script to end", || {
        a.status(&s).unwrap().state == SessionState::Ended
    });
    assert_eq!(
        std::fs::read_to_string(out.join("cwd.txt")).unwrap().trim(),
        cwd.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(out.join("greeting.txt"))
            .unwrap()
            .trim(),
        "hello paper"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("events-var.txt"))
            .unwrap()
            .trim(),
        "1"
    );
    assert!(a.status(&s).unwrap().first_turn_started);
    assert!(!a.is_alive(&s).unwrap());
}

#[test]
fn usage_lines_sum_and_push_and_turn_end_reach_the_port() {
    let body = r#"echo '{"usage":{"input_tokens":3,"output_tokens":1,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":30}}' >> "$PF_PAPER_EVENTS"
echo '{"usage":{"input_tokens":4,"output_tokens":2,"cache_read_tokens":5,"cache_write_tokens":6,"cost_micros":70}}' >> "$PF_PAPER_EVENTS"
echo '{"push":{"branch":"work/u1","sha":"abc123"}}' >> "$PF_PAPER_EVENTS"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
"#;
    let (dir, a) = setup(Some(body));
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    wait_until("the turn to end", || a.status(&s).unwrap().turn_ended);
    wait_until("the process to end", || {
        a.status(&s).unwrap().state == SessionState::Ended
    });
    let u = a.usage(&s).unwrap();
    assert_eq!(
        u,
        Usage {
            input_tokens: 7,
            output_tokens: 3,
            cache_read_tokens: 5,
            cache_write_tokens: 6,
            cost_micros: 100
        }
    );
    let events = a.drain_events(&s).unwrap();
    let pushes: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Push { .. }))
        .collect();
    assert_eq!(pushes.len(), 1);
    match pushes[0] {
        SessionEvent::Push {
            branch,
            sha,
            outcome,
            ..
        } => {
            assert_eq!((branch.as_str(), sha.as_str()), ("work/u1", "abc123"));
            assert_eq!(*outcome, pf_engine::admission::PushOutcome::Pushed);
        }
        _ => unreachable!(),
    }
    assert!(
        a.drain_events(&s).unwrap().is_empty(),
        "a drain takes the events once"
    );
}

#[test]
fn a_malformed_line_is_ignored_and_noted() {
    let body = "echo 'this is not json' >> \"$PF_PAPER_EVENTS\"\necho '{\"usage\":{\"input_tokens\":2,\"cost_micros\":5}}' >> \"$PF_PAPER_EVENTS\"\necho '{\"mystery\":1}' >> \"$PF_PAPER_EVENTS\"\n";
    let (dir, a) = setup(Some(body));
    let run_dir = dir.path().join("run");
    let s = a
        .launch(&plan(dir.path(), &[("run_dir", run_dir.to_str().unwrap())]))
        .unwrap();
    wait_until("the process to end", || {
        a.status(&s).unwrap().state == SessionState::Ended
    });
    let u = a.usage(&s).unwrap();
    assert_eq!(
        (u.input_tokens, u.cost_micros),
        (2, 5),
        "the good line counts"
    );
    let note = std::fs::read_to_string(run_dir.join("out/u1/paper-notes.log")).unwrap();
    assert!(note.contains("this is not json"), "{note}");
    assert!(note.contains("mystery"), "{note}");
}

#[test]
fn a_missing_script_is_refused() {
    let (dir, a) = setup(None);
    let err = a.launch(&plan(dir.path(), &[])).unwrap_err();
    assert!(matches!(err, AgentError::Refused(_)), "{err:?}");
    assert!(err.to_string().contains("implementer.sh"), "{err}");
}

#[test]
fn stop_and_interrupt_end_the_process_and_its_children() {
    for how in ["stop", "interrupt"] {
        let (dir, a) = setup(Some(
            "(trap '' HUP; exec sleep 600) &\necho $! > \"$OUT/child.pid\"\necho $$ > \"$OUT/self.pid\"\nwait\n",
        ));
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let s = a
            .launch(&plan(dir.path(), &[("env.OUT", out.to_str().unwrap())]))
            .unwrap();
        wait_until("the child to start", || {
            out.join("child.pid").exists() && out.join("self.pid").exists()
        });
        let read = |n: &str| -> u32 {
            std::fs::read_to_string(out.join(n))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        let (child, me) = (read("child.pid"), read("self.pid"));
        assert!(alive(child) && alive(me));
        assert!(a.is_alive(&s).unwrap());
        if how == "stop" {
            a.stop(&s).unwrap();
            a.stop(&s).unwrap();
        } else {
            a.interrupt(&s).unwrap();
        }
        wait_until("the processes to end", || !alive(child) && !alive(me));
        assert!(!a.is_alive(&s).unwrap());
        assert_eq!(a.status(&s).unwrap().state, SessionState::Ended);
    }
}

#[test]
fn no_pty_is_opened() {
    let (dir, a) = setup(Some(
        "if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then echo tty > \"$OUT/tty\"; else echo none > \"$OUT/tty\"; fi\n",
    ));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let s = a
        .launch(&plan(dir.path(), &[("env.OUT", out.to_str().unwrap())]))
        .unwrap();
    wait_until("the script to end", || {
        a.status(&s).unwrap().state == SessionState::Ended
    });
    assert_eq!(
        std::fs::read_to_string(out.join("tty")).unwrap().trim(),
        "none"
    );
}

#[test]
fn an_unknown_reference_is_not_alive_and_stops_cleanly() {
    let (_dir, a) = setup(None);
    let r = SessionRef("paper-99".into());
    assert_eq!(a.is_alive(&r), Ok(false));
    assert!(a.stop(&r).is_ok());
}

#[test]
fn resolve_plugin_reads_the_plugin_roots() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("plugins/market/methodkit/1.26.1");
    std::fs::create_dir_all(base.join(".claude-plugin")).unwrap();
    std::fs::write(
        base.join(".claude-plugin/plugin.json"),
        "{\"version\":\"1.26.1\"}",
    )
    .unwrap();
    let a = PaperAdapter::new(PaperConfig::new(
        dir.path().to_path_buf(),
        vec![dir.path().join("plugins")],
    ));
    let found = a.resolve_plugin("methodkit", "1.26.0").unwrap();
    assert_eq!(
        (found.version.as_str(), PathBuf::from(&found.path)),
        ("1.26.1", base)
    );
    assert!(matches!(
        a.resolve_plugin("methodkit", "2.0.0"),
        Err(AgentError::Refused(_))
    ));
    assert!(matches!(
        a.resolve_plugin("nonesuch", "1.0.0"),
        Err(AgentError::Refused(_))
    ));
}

#[test]
fn parse_pr_reads_a_file_url_and_refuses_a_relative_one() {
    let pr = pf_workflow::signals::parse_pr("file:///a/b#3").unwrap();
    assert_eq!((pr.repo.as_str(), pr.number), ("/a/b", 3));
    assert!(pf_workflow::signals::parse_pr("file://a/b#3").is_none());
}

// ------------------------------------------------------------------ deliver (r2.s1.w1)

/// The lines of a file as JSON values. A missing file holds none.
fn json_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("`{l}` is not JSON: {e}")))
        .collect()
}

#[test]
fn a_launch_exports_the_inbox_path_and_the_file_exists_for_the_script_to_wait_on() {
    let (dir, a) = setup(Some(
        "echo \"$PF_PAPER_INBOX\" > \"$OUT/inbox-path.txt\"\nif [ -f \"$PF_PAPER_INBOX\" ]; then echo yes > \"$OUT/inbox-exists.txt\"; fi\n",
    ));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let s = a
        .launch(&plan(dir.path(), &[("env.OUT", out.to_str().unwrap())]))
        .unwrap();
    wait_until("the script to end", || {
        a.status(&s).unwrap().state == SessionState::Ended
    });
    let path = std::fs::read_to_string(out.join("inbox-path.txt")).unwrap();
    assert!(
        path.trim().ends_with(".inbox") && path.contains(&s.0),
        "the inbox file is named for the session: {path}"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("inbox-exists.txt"))
            .unwrap()
            .trim(),
        "yes",
        "the file exists at launch"
    );
}

#[test]
fn deliver_appends_one_json_line_to_the_inbox_and_starts_another_turn() {
    // The seat ends a turn and waits for a message. When one came it copies the inbox, marks the
    // copy done, and ends a second turn only once the test releases it: the test reads the status
    // and the copy at points it chooses, not when the scheduler lets it.
    let body = r#"echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
while [ ! -s "$PF_PAPER_INBOX" ]; do sleep 0.02; done
cp "$PF_PAPER_INBOX" "$OUT/seen-inbox.txt" && : > "$OUT/seen-inbox.done"
while [ ! -e "$OUT/release" ]; do sleep 0.02; done
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
sleep 600
"#;
    let (dir, a) = setup(Some(body));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let run_dir = dir.path().join("run");
    let s = a
        .launch(&plan(
            dir.path(),
            &[
                ("env.OUT", out.to_str().unwrap()),
                ("run_dir", run_dir.to_str().unwrap()),
            ],
        ))
        .unwrap();
    wait_until("the first turn to end", || a.status(&s).unwrap().turn_ended);

    let message = "Finish u1: open the PR.\nSay \"done\" when it is open.";
    a.deliver(&s, message).unwrap();
    // The message starts a turn: the session is working until its next turn end.
    let st = a.status(&s).unwrap();
    assert!(
        !st.turn_ended && st.state == SessionState::Working,
        "{st:?}"
    );

    // `cp` creates the file before it has written it: wait for the seat's mark that the copy is
    // whole.
    wait_until("the seat to copy the message", || {
        out.join("seen-inbox.done").exists()
    });
    let seen = json_lines(&out.join("seen-inbox.txt"));
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["message"], message);
    assert_eq!(
        seen[0].as_object().unwrap().len(),
        1,
        "the line is {{\"message\": ...}} and nothing else"
    );
    // The seat still works: its turn ends only on the release.
    let st = a.status(&s).unwrap();
    assert!(
        !st.turn_ended && st.state == SessionState::Working,
        "{st:?}"
    );
    std::fs::write(out.join("release"), "").unwrap();
    wait_until("the second turn to end", || {
        let st = a.status(&s).unwrap();
        st.turn_ended && st.state == SessionState::Idle
    });

    // A second message is a second line.
    a.deliver(&s, "again").unwrap();
    let inbox: Vec<PathBuf> = std::fs::read_dir(run_dir.join("paper"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "inbox"))
        .collect();
    assert_eq!(inbox.len(), 1, "one inbox file for the session");
    let lines = json_lines(&inbox[0]);
    assert_eq!(
        lines
            .iter()
            .map(|l| l["message"].clone())
            .collect::<Vec<_>>(),
        vec![serde_json::json!(message), serde_json::json!("again")]
    );
    a.stop(&s).unwrap();
}

#[test]
fn a_delivery_to_an_ended_session_is_refused_and_writes_nothing() {
    let (dir, a) = setup(Some("exit 0\n"));
    let run_dir = dir.path().join("run");
    let s = a
        .launch(&plan(dir.path(), &[("run_dir", run_dir.to_str().unwrap())]))
        .unwrap();
    wait_until("the script to end", || {
        a.status(&s).unwrap().state == SessionState::Ended
    });
    let err = a.deliver(&s, "too late").unwrap_err();
    assert!(matches!(err, AgentError::Failed(_)), "{err:?}");
    assert!(err.to_string().contains("ended"), "{err}");
    let inbox = run_dir.join("paper").join(format!("{}.inbox", s.0));
    assert!(json_lines(&inbox).is_empty(), "nothing was appended");

    // A stopped session is an ended one too.
    let (dir, a) = setup(Some("sleep 600\n"));
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    a.stop(&s).unwrap();
    assert!(a.deliver(&s, "too late").is_err());

    // So is a session the adapter does not know.
    assert!(a.deliver(&SessionRef("paper-99".into()), "x").is_err());
}
