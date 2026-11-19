//! AC-11: the paper adapter runs a seat script as a real process, with no PTY and no model. A
//! script writes one JSON line for each event to `$PF_PAPER_EVENTS`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pf_adapter_fake::{PaperAdapter, PaperConfig};
use pf_engine::port::agent::*;
use pf_engine::port::host::SessionHosts;

mod hosts_support;
use hosts_support::{Dirs, Guard, TestHosts};

/// A paper env directory, a host service on a detached launcher, and the guard that stops every
/// host when the test ends, pass or fail.
struct Fx {
    _guard: Guard,
    _dirs: Dirs,
    tmp: tempfile::TempDir,
    hosts: Arc<TestHosts>,
}

impl Fx {
    fn path(&self) -> &Path {
        self.tmp.path()
    }

    /// A new adapter over the same paper env and the same host files: a controller that started again.
    fn adapter(&self) -> PaperAdapter {
        PaperAdapter::new(
            PaperConfig::new(self.tmp.path().to_path_buf(), vec![]),
            self.hosts.clone() as Arc<dyn SessionHosts>,
        )
    }
}

fn setup(script: Option<&str>) -> (Fx, PaperAdapter) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("seats")).unwrap();
    if let Some(body) = script {
        std::fs::write(tmp.path().join("seats/implementer.sh"), body).unwrap();
    }
    let dirs = Dirs::new();
    let hosts = TestHosts::new(&dirs);
    let fx = Fx {
        _guard: Guard(hosts.clone()),
        _dirs: dirs,
        tmp,
        hosts,
    };
    let adapter = fx.adapter();
    (fx, adapter)
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
    let dirs = Dirs::new();
    let a = PaperAdapter::new(
        PaperConfig::new(dir.path().to_path_buf(), vec![dir.path().join("plugins")]),
        TestHosts::new(&dirs) as Arc<dyn SessionHosts>,
    );
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

// ------------------------------------------------------------------ hosts (r2.s2.w3)

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// The names the paper adapter takes from the controller's own environment.
const INHERITED: [&str; 13] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "PF_STATE_DIR",
    "PF_SHARE_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_TERMINAL_PROMPT",
];

#[test]
fn a_launch_starts_the_script_in_a_host_on_pipes_with_the_listed_environment_and_nothing_else() {
    // The script stays alive, so the host and its launch record are still there to read.
    let (dir, a) = setup(Some(
        "env > \"$OUT/env.tmp\" && mv \"$OUT/env.tmp\" \"$OUT/env.txt\"\nsleep 600\n",
    ));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let s = a
        .launch(&plan(
            dir.path(),
            &[("env.OUT", out.to_str().unwrap()), ("env.GREETING", "hi")],
        ))
        .unwrap();
    wait_until("the script to list its environment", || {
        out.join("env.txt").exists()
    });
    let names: Vec<String> = read(&out.join("env.txt"))
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_string()))
        .collect();
    // The shell adds a few variables of its own.
    let shell = ["PWD", "OLDPWD", "SHLVL", "_"];
    let allowed = |n: &str| {
        INHERITED.contains(&n)
            || shell.contains(&n)
            || ["OUT", "GREETING", "PF_PAPER_EVENTS", "PF_PAPER_INBOX"].contains(&n)
    };
    let stray: Vec<&String> = names.iter().filter(|n| !allowed(n)).collect();
    assert!(
        stray.is_empty(),
        "the host leaked {stray:?} (all: {names:?})"
    );
    assert!(names.contains(&"GREETING".to_string()), "{names:?}");
    assert!(names.contains(&"PF_PAPER_EVENTS".to_string()), "{names:?}");
    // The test process itself runs under cargo, so these are in its own environment.
    for inherited_by_cargo in ["CARGO_MANIFEST_DIR", "CARGO_PKG_NAME"] {
        if std::env::var_os(inherited_by_cargo).is_some() {
            assert!(
                !names.contains(&inherited_by_cargo.to_string()),
                "{inherited_by_cargo} must not reach the child"
            );
        }
    }
    // The script ran in a host, on pipes.
    let unit = &s.0;
    let record = dir.hosts.launched_record(unit).unwrap().unwrap();
    assert_eq!(record["transport"], serde_json::json!({"kind": "pipes"}));
    assert_eq!(record["argv"][0], "/bin/sh");
    assert_eq!(record["runtime"], serde_json::json!({"id": "paper"}));
    assert!(record["runtime"].get("binary_path").is_none(), "{record}");
    assert!(
        a.process_id(&s).is_some(),
        "a live session has a process ID"
    );
    a.stop(&s).unwrap();
    assert!(a.process_id(&s).is_none(), "a stopped session has none");
}

#[test]
fn the_session_key_reaches_the_child_and_the_record_holds_only_its_hash() {
    let secret = "pfk_0123456789abcdef0123456789abcdef";
    let (dir, a) = setup(Some(
        "echo \"$PF_SESSION_KEY\" > \"$OUT/key.tmp\" && mv \"$OUT/key.tmp\" \"$OUT/key.txt\"\nsleep 600\n",
    ));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let s = a
        .launch(&plan(
            dir.path(),
            &[
                ("env.OUT", out.to_str().unwrap()),
                ("env.PF_SESSION_KEY", secret),
            ],
        ))
        .unwrap();
    wait_until("the script to write the key", || {
        out.join("key.txt").exists()
    });
    assert_eq!(read(&out.join("key.txt")).trim(), secret);
    let record_text = dir
        .hosts
        .launched_record(&s.0)
        .unwrap()
        .unwrap()
        .to_string();
    assert!(!record_text.contains(secret), "the record holds the key");
    let want = format!("sha256:{}", pf_host::sha256::sha256_hex(secret.as_bytes()));
    let record: serde_json::Value = serde_json::from_str(&record_text).unwrap();
    assert_eq!(record["env"]["PF_SESSION_KEY"], want);
    assert_eq!(record["settings"]["env.PF_SESSION_KEY"], want);
    assert_eq!(
        record["secret_names"],
        serde_json::json!(["PF_SESSION_KEY"])
    );
}

const TWO_USAGES_A_PUSH_AND_A_TURN_END: &str = r#"echo '{"usage":{"input_tokens":3,"output_tokens":1,"cache_read_tokens":0,"cache_write_tokens":0,"cost_micros":30}}' >> "$PF_PAPER_EVENTS"
echo '{"usage":{"input_tokens":4,"output_tokens":2,"cache_read_tokens":5,"cache_write_tokens":6,"cost_micros":70}}' >> "$PF_PAPER_EVENTS"
echo '{"push":{"branch":"work/u1","sha":"abc123"}}' >> "$PF_PAPER_EVENTS"
echo '{"turn_end":true}' >> "$PF_PAPER_EVENTS"
sleep 600
"#;

fn pushes(events: &[SessionEvent]) -> Vec<(String, String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Push {
                id, branch, sha, ..
            } => Some((id.clone(), branch.clone(), sha.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn after_a_rebind_usage_is_the_same_sum_each_push_comes_once_and_the_turn_rule_holds() {
    let (dir, a) = setup(Some(TWO_USAGES_A_PUSH_AND_A_TURN_END));
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    wait_until("the turn to end", || a.status(&s).unwrap().turn_ended);
    let before = a.usage(&s).unwrap();
    let first_pushes = pushes(&a.drain_events(&s).unwrap());
    assert_eq!(first_pushes.len(), 1, "{first_pushes:?}");

    // The controller stops: a new adapter over the same host files rebinds the same unit.
    drop(a);
    let b = dir.adapter();
    assert_eq!(b.rebind(&s).unwrap(), RebindOutcome::Survived);
    assert!(b.is_alive(&s).unwrap());
    assert_eq!(b.usage(&s).unwrap(), before, "the usage is the same sum");
    assert_eq!(
        before,
        Usage {
            input_tokens: 7,
            output_tokens: 3,
            cache_read_tokens: 5,
            cache_write_tokens: 6,
            cost_micros: 100
        }
    );
    let again = pushes(&b.drain_events(&s).unwrap());
    assert_eq!(again, first_pushes, "the push comes once, with the same ID");
    assert!(
        b.drain_events(&s).unwrap().is_empty(),
        "a drain takes the events once"
    );
    assert!(b.status(&s).unwrap().turn_ended);
    assert!(b.process_id(&s).is_some());

    // A message delivered after the last turn_end reads as working, also to the next controller.
    b.deliver(&s, "go on").unwrap();
    let st = b.status(&s).unwrap();
    assert!(
        !st.turn_ended && st.state == SessionState::Working,
        "{st:?}"
    );
    drop(b);
    let c = dir.adapter();
    assert_eq!(c.rebind(&s).unwrap(), RebindOutcome::Survived);
    let st = c.status(&s).unwrap();
    assert!(
        !st.turn_ended && st.state == SessionState::Working,
        "one turn_end and one message: still working: {st:?}"
    );
    c.stop(&s).unwrap();
}

#[test]
fn a_gone_host_is_lost_after_a_rebind() {
    let (dir, a) = setup(Some("sleep 600\n"));
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    a.stop(&s).unwrap();
    wait_until("the host to end", || !dir.hosts.is_live(&s.0));
    let b = dir.adapter();
    assert!(
        matches!(b.rebind(&s).unwrap(), RebindOutcome::Lost { .. }),
        "a gone host is lost"
    );
    assert!(!b.is_alive(&s).unwrap());
}

#[test]
fn a_seat_that_ended_while_the_controller_was_down_is_survived_and_reads_as_ended() {
    // The script waits for a release, so the first adapter sees no exit: the seat ends while no
    // controller is attached.
    let body = "echo $$ > \"$OUT/self.pid\"\nwhile [ ! -e \"$OUT/release\" ]; do sleep 0.02; done\necho '{\"usage\":{\"input_tokens\":9}}' >> \"$PF_PAPER_EVENTS\"\nexit 0\n";
    let (dir, a) = setup(Some(body));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let s = a
        .launch(&plan(dir.path(), &[("env.OUT", out.to_str().unwrap())]))
        .unwrap();
    wait_until("the script to start", || out.join("self.pid").exists());
    let pid: u32 = read(&out.join("self.pid")).trim().parse().unwrap();
    // The first controller is gone: it reads nothing more.
    std::mem::forget(a);
    std::fs::write(out.join("release"), "").unwrap();
    wait_until("the child to end", || !alive(pid));
    let b = dir.adapter();
    assert_eq!(b.rebind(&s).unwrap(), RebindOutcome::Survived);
    wait_until("the session to read as ended", || {
        b.status(&s).unwrap().state == SessionState::Ended
    });
    assert!(!b.is_alive(&s).unwrap());
    assert_eq!(b.usage(&s).unwrap().input_tokens, 9);
    wait_until("the host to end after the exit ack", || {
        !dir.hosts.is_live(&s.0)
    });
}

#[test]
fn the_childs_exit_ends_the_session_and_the_host_is_gone_after_the_exit_ack() {
    let (dir, a) = setup(Some(
        "echo '{\"usage\":{\"input_tokens\":5}}' >> \"$PF_PAPER_EVENTS\"\nexit 3\n",
    ));
    let s = a.launch(&plan(dir.path(), &[])).unwrap();
    wait_until("the session to end", || {
        a.status(&s).unwrap().state == SessionState::Ended
    });
    assert!(!a.is_alive(&s).unwrap());
    assert_eq!(a.usage(&s).unwrap().input_tokens, 5, "the last line counts");
    wait_until("the host to end after the exit ack", || {
        !dir.hosts.is_live(&s.0)
    });
}

#[test]
fn stop_ends_the_childs_group_the_host_goes_and_a_second_stop_is_safe() {
    let (dir, a) = setup(Some(
        "(trap '' HUP; exec sleep 600) &\necho $! > \"$OUT/child.pid\"\nwait\n",
    ));
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let s = a
        .launch(&plan(dir.path(), &[("env.OUT", out.to_str().unwrap())]))
        .unwrap();
    wait_until("the child to start", || out.join("child.pid").exists());
    let child: u32 = read(&out.join("child.pid")).trim().parse().unwrap();
    assert!(alive(child));
    a.stop(&s).unwrap();
    a.stop(&s).unwrap();
    wait_until("the group to end", || !alive(child));
    wait_until("the host to end", || !dir.hosts.is_live(&s.0));
    assert!(!a.is_alive(&s).unwrap());
}

#[test]
fn two_launches_in_two_adapters_never_share_an_events_file_or_a_push_id() {
    let body = "echo '{\"push\":{\"branch\":\"work/u1\",\"sha\":\"abc123\"}}' >> \"$PF_PAPER_EVENTS\"\nsleep 600\n";
    let (dir, a) = setup(Some(body));
    let first = a.launch(&plan(dir.path(), &[])).unwrap();
    let mut first_events = Vec::new();
    wait_until("the first push", || {
        first_events.extend(a.drain_events(&first).unwrap());
        !pushes(&first_events).is_empty()
    });
    // A second controller lifetime: its adapter counts from the start again.
    let b = dir.adapter();
    let second = b.launch(&plan(dir.path(), &[])).unwrap();
    let mut second_events = Vec::new();
    wait_until("the second push", || {
        second_events.extend(b.drain_events(&second).unwrap());
        !pushes(&second_events).is_empty()
    });
    assert_ne!(first, second, "the unit never repeats");
    let (id1, id2) = (
        pushes(&first_events)[0].0.clone(),
        pushes(&second_events)[0].0.clone(),
    );
    assert_ne!(id1, id2, "a push ID is never reused");
    assert!(id1.starts_with(&first.0) && id2.starts_with(&second.0));
    let files = |unit: &str| {
        (
            dir.path().join("paper").join(format!("{unit}.events")),
            dir.path().join("paper").join(format!("{unit}.inbox")),
        )
    };
    let (e1, e2) = (files(&first.0), files(&second.0));
    assert_ne!(e1.0, e2.0);
    assert!(e1.0.exists() && e2.0.exists());
}
