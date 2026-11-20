//! AC-6 of `r2.s2.w4`: ADR-0011's kept-and-resent event test. An event the extension wrote while the
//! controller was down is read exactly once after the controller restarts, and an event the engine
//! took before the restart is never taken twice.
//!
//! Two adapters over two `TestHosts` on the same directories play two controller lifetimes: the
//! first adapter launches, reads and is dropped without `stop` (the controller died), and the host
//! and its child go on. A test "driver" plays `Driver::read` and `Engine::submit`: it records each
//! push by its ID. A replay is the same ID with the same content. A conflict is the same ID with
//! other content, and the test fails on it. The repositories are real scratch repositories with a
//! local bare remote; no test starts OMP or a model.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::hosts::{Dirs, Guard, TestHosts};
use common::*;
use pf_adapter_omp::OmpAdapter;
use pf_engine::port::agent::{
    AgentPort, RebindOutcome, SessionEvent, SessionRef, SessionState, Usage,
};

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

fn commit(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-m", file]);
}

/// A bare remote with `main`, a clone of it with the branches `topic` and `other` (each one commit
/// ahead of `main`), left on `topic`.
fn scratch_repo(root: &Path) -> (PathBuf, PathBuf) {
    let remote = root.join("remote.git");
    let work = root.join("clone");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare", "-b", "main"]);
    git(root, &["clone", remote.to_str().unwrap(), "clone"]);
    git(&work, &["checkout", "-b", "main"]);
    commit(&work, "a.txt");
    git(&work, &["push", "-u", "origin", "main"]);
    git(&work, &["checkout", "-b", "other"]);
    commit(&work, "o.txt");
    git(&work, &["checkout", "-b", "topic", "main"]);
    commit(&work, "b.txt");
    (remote, work)
}

fn bash_line(command: &str) -> String {
    serde_json::json!({"ev":"bash","t":10,"command":command,"exitCode":0}).to_string()
}

fn usage_line(input: u64, output: u64) -> String {
    serde_json::json!({
        "ev": "message_end", "t": 11, "role": "assistant",
        "usage": {"input": input, "output": output, "cacheRead": 0, "cacheWrite": 0,
                  "cost": {"total": 0.0}}
    })
    .to_string()
}

const UNREAD: &str = r#"git -C "$wt" push -u origin "$b""#;
const AGENT_END: &str = r#"{"ev":"agent_end","t":12}"#;

/// What `Engine::submit` and `Driver::read` do with what a drain returns: record each push by its
/// ID, count a replay, fail on a conflict, and keep the usage as the difference from the last read.
#[derive(Default)]
struct Driver {
    /// Each recorded push by ID.
    pushes: BTreeMap<String, (String, String)>,
    replays: usize,
    moves: Vec<(String, String, String)>,
    usage: Usage,
}

impl Driver {
    fn read(&mut self, adapter: &OmpAdapter, session: &SessionRef) {
        let events = adapter.drain_events(session).unwrap();
        self.record(events);
        let u = adapter.usage(session).unwrap();
        // The row only grows: the driver records the positive difference.
        self.usage.input_tokens = self.usage.input_tokens.max(u.input_tokens);
        self.usage.output_tokens = self.usage.output_tokens.max(u.output_tokens);
    }

    /// Record the events of one drain, as the driver commits them.
    fn record(&mut self, events: Vec<SessionEvent>) {
        for event in events {
            match event {
                SessionEvent::Push {
                    id, branch, sha, ..
                } => {
                    let content = (branch, sha);
                    match self.pushes.get(&id) {
                        None => {
                            self.pushes.insert(id, content);
                        }
                        Some(first) if *first == content => self.replays += 1,
                        Some(first) => panic!(
                            "Conflict: push {id} was recorded as {first:?} and now comes as {content:?}"
                        ),
                    }
                }
                SessionEvent::DefaultBranchMoved {
                    branch,
                    before,
                    after,
                    ..
                } => self.moves.push((branch, before, after)),
                SessionEvent::Activity | SessionEvent::Stalled { .. } => {}
            }
        }
    }

    fn ids(&self) -> Vec<String> {
        self.pushes.keys().cloned().collect()
    }
}

/// Two controller lifetimes over one set of directories.
struct Lifetimes {
    /// First, so that the guards stop the hosts before the directories go.
    guards: Vec<Guard>,
    dir: tempfile::TempDir,
    dirs: Dirs,
    omp: PathBuf,
    remote: PathBuf,
    work: PathBuf,
}

impl Lifetimes {
    fn new() -> Lifetimes {
        let dir = tempfile::tempdir().unwrap();
        let (remote, work) = scratch_repo(dir.path());
        let omp = stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN);
        Lifetimes {
            guards: Vec::new(),
            dir,
            dirs: Dirs::new(),
            omp,
            remote,
            work,
        }
    }

    /// A new controller lifetime: a host service and an adapter over the shared directories.
    fn lifetime(&mut self) -> (OmpAdapter, Arc<TestHosts>) {
        let hosts = TestHosts::new(&self.dirs);
        self.guards.push(Guard(hosts.clone()));
        let a = adapter_over(self.dir.path(), self.omp.clone(), hosts.clone());
        (a, hosts)
    }

    fn launch(&self, a: &OmpAdapter) -> SessionRef {
        a.launch(&plan(
            &self.work,
            &[
                ("push_dir", self.work.to_str().unwrap()),
                ("push_branch", "topic"),
                ("default_branch", "main"),
                ("remote", "origin"),
            ],
        ))
        .unwrap()
    }
}

fn rebound(b: &OmpAdapter, s: &SessionRef) -> RebindOutcome {
    b.rebind(s).unwrap()
}

#[test]
fn an_event_written_while_the_controller_was_down_is_read_once_and_nothing_is_taken_twice() {
    let mut l = Lifetimes::new();
    let (a, _hosts_a) = l.lifetime();
    let s = l.launch(&a);
    let events = a.event_file(&s).unwrap();
    let child = a.process_id(&s).expect("a child");
    let mut driver = Driver::default();

    // Lifetime 1: a usage line and a literal push, read and recorded.
    append(
        &events,
        &[&usage_line(10, 2), &bash_line("git push origin topic")],
    );
    driver.read(&a, &s);
    assert_eq!(driver.ids(), vec![format!("{}-push-1", s.0)]);
    let topic = git(&l.work, &["rev-parse", "topic"]);
    assert_eq!(
        driver.pushes[&format!("{}-push-1", s.0)],
        ("topic".to_string(), topic.clone())
    );
    assert_eq!(driver.usage.input_tokens, 10);
    // The controller dies: no `stop`, the adapter is gone, the host and its child stay.
    drop(a);

    // The extension goes on writing while no adapter runs: a usage line, a literal push, a push with
    // an unread target and a turn end.
    let other = git(&l.work, &["rev-parse", "other"]);
    append(
        &events,
        &[
            &usage_line(5, 1),
            &bash_line("git push origin other"),
            &bash_line(UNREAD),
            AGENT_END,
        ],
    );

    // Lifetime 2.
    let (b, _hosts_b) = l.lifetime();
    assert_eq!(rebound(&b, &s), RebindOutcome::Survived);
    assert_eq!(b.process_id(&s), Some(child), "the same child");
    driver.read(&b, &s);

    // The two new pushes come once each, with new IDs.
    let ids = driver.ids();
    assert_eq!(
        ids,
        vec![
            format!("{}-push-1", s.0),
            format!("{}-push-2", s.0),
            format!("{}-push-3", s.0),
        ]
    );
    assert_eq!(driver.pushes[&ids[1]], ("other".to_string(), other));
    assert_eq!(driver.pushes[&ids[2]], ("topic".to_string(), topic));
    // The first push came again, and only as a replay: the same ID with the same content.
    assert_eq!(driver.replays, 1, "the kept push is resent once");
    // Usage over both lifetimes is the sum of all usage lines, once.
    let u = b.usage(&s).unwrap();
    assert_eq!((u.input_tokens, u.output_tokens), (15, 3));
    assert_eq!(
        (driver.usage.input_tokens, driver.usage.output_tokens),
        (15, 3)
    );
    // The turn ended while the controller was down.
    let status = b.status(&s).unwrap();
    assert!(status.turn_ended);
    assert_eq!(status.state, SessionState::Idle);

    // Nothing is read twice: the next drains hand over nothing, and the replay does not come again.
    driver.read(&b, &s);
    driver.read(&b, &s);
    assert_eq!(driver.ids().len(), 3);
    assert_eq!(driver.replays, 1);
    // A line the extension writes after the rebind is read once.
    let topic_again = git(&l.work, &["rev-parse", "topic"]);
    append(&events, &[&bash_line("git push origin topic")]);
    driver.read(&b, &s);
    assert_eq!(driver.ids().len(), 4);
    assert_eq!(
        driver.pushes[&format!("{}-push-4", s.0)],
        ("topic".to_string(), topic_again)
    );
    b.stop(&s).unwrap();
}

#[test]
fn a_third_lifetime_after_two_drains_resends_nothing() {
    let mut l = Lifetimes::new();
    let (a, _) = l.lifetime();
    let s = l.launch(&a);
    let events = a.event_file(&s).unwrap();
    let mut driver = Driver::default();
    append(&events, &[&bash_line("git push origin topic")]);
    driver.read(&a, &s);
    // The next drain is the acknowledge: it drops what the first one handed over.
    driver.read(&a, &s);
    drop(a);
    let (b, _) = l.lifetime();
    assert_eq!(rebound(&b, &s), RebindOutcome::Survived);
    driver.read(&b, &s);
    assert_eq!(driver.ids().len(), 1);
    assert_eq!(driver.replays, 0, "an acknowledged push is not resent");
    b.stop(&s).unwrap();
}

#[test]
fn a_drain_that_the_driver_never_recorded_is_handed_over_again_and_recorded_once() {
    let mut l = Lifetimes::new();
    let (a, _) = l.lifetime();
    let s = l.launch(&a);
    let events = a.event_file(&s).unwrap();
    append(
        &events,
        &[
            &bash_line("git push origin topic"),
            &bash_line("git push origin other"),
        ],
    );
    // A drains, and the controller dies before the driver commits what it got.
    let lost = a.drain_events(&s).unwrap();
    assert_eq!(
        lost.iter()
            .filter(|e| matches!(e, SessionEvent::Push { .. }))
            .count(),
        2
    );
    drop(a);

    let (b, _) = l.lifetime();
    assert_eq!(rebound(&b, &s), RebindOutcome::Survived);
    let mut driver = Driver::default();
    driver.read(&b, &s);
    assert_eq!(
        driver.ids(),
        vec![format!("{}-push-1", s.0), format!("{}-push-2", s.0)]
    );
    assert_eq!(driver.replays, 0, "each push is recorded once");
    // And it is not handed over a second time.
    driver.read(&b, &s);
    assert_eq!(driver.ids().len(), 2);
    assert_eq!(driver.replays, 0);
    b.stop(&s).unwrap();
}

#[test]
fn a_default_branch_move_made_while_no_adapter_ran_is_reported_once() {
    let mut l = Lifetimes::new();
    let (a, _) = l.lifetime();
    let s = l.launch(&a);
    let events = a.event_file(&s).unwrap();
    let before = git(&l.remote, &["rev-parse", "main"]);
    drop(a);

    // While no adapter runs, someone moves the default branch of the remote to a commit the seat's
    // clone holds, and the seat pushes with an unread target.
    git(&l.work, &["push", "origin", "topic:main"]);
    let after = git(&l.remote, &["rev-parse", "main"]);
    assert_ne!(before, after);
    append(&events, &[&bash_line(UNREAD)]);

    let (b, _) = l.lifetime();
    assert_eq!(rebound(&b, &s), RebindOutcome::Survived);
    let mut driver = Driver::default();
    driver.read(&b, &s);
    assert_eq!(
        driver.moves,
        vec![("main".to_string(), before, after.clone())],
        "the baseline of the launch is what survived the restart"
    );
    // Reported once: the baseline moved on.
    append(&events, &[&bash_line(UNREAD)]);
    driver.read(&b, &s);
    assert_eq!(driver.moves.len(), 1, "{:?}", driver.moves);
    b.stop(&s).unwrap();
}

#[test]
fn a_survived_host_with_no_stored_position_is_unreachable() {
    let mut l = Lifetimes::new();
    let (a, hosts_a) = l.lifetime();
    let s = l.launch(&a);
    drop(a);
    hosts_a.forget_position(&s.0);
    let (b, _) = l.lifetime();
    let RebindOutcome::Unreachable { why } = rebound(&b, &s) else {
        panic!("a host with no position must be unreachable");
    };
    assert!(why.contains("no stored position"), "{why}");
    assert!(why.contains(&s.0), "{why}");
    // The adapter did not take the session.
    assert_eq!(b.is_alive(&s), Ok(false));
    // The host runs still: the engine parks the unit and w3's orphan sweep stops it.
    let (c, hosts_c) = l.lifetime();
    assert!(hosts_c.is_live(&s.0));
    drop(c);
}

#[test]
fn a_gone_host_is_lost() {
    let mut l = Lifetimes::new();
    let (a, hosts_a) = l.lifetime();
    let s = l.launch(&a);
    drop(a);
    hosts_a.stop_all();
    let (b, _) = l.lifetime();
    let RebindOutcome::Lost { cause } = rebound(&b, &s) else {
        panic!("a gone host must be lost");
    };
    assert!(!cause.is_empty());
    assert_eq!(b.is_alive(&s), Ok(false));
}

#[test]
fn a_host_that_exited_while_the_controller_was_down_is_survived_and_reads_as_ended() {
    let mut l = Lifetimes::new();
    let stub = stub_omp(
        l.dir.path(),
        "18.11.3",
        r#"echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
while [ ! -e "$DIR/go" ]; do sleep 0.05; done
echo '{"ev":"agent_end","t":3}' >> "$PF_EVENT_FILE"
exit 0"#,
    );
    l.omp = stub;
    let (a, hosts_a) = l.lifetime();
    let s = l.launch(&a);
    let unit = s.0.clone();
    drop(a);
    std::fs::write(l.dir.path().join("go"), b"").unwrap();
    // The child ends while no adapter runs. The host holds the exit for the next controller.
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while hosts_a.is_live(&unit)
        && std::fs::read_to_string(
            l.dir
                .path()
                .join("state/sessions")
                .join(&unit)
                .join("events.jsonl"),
        )
        .map_or(true, |t| !t.contains("agent_end"))
        && std::time::Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));

    let (b, _) = l.lifetime();
    assert_eq!(rebound(&b, &s), RebindOutcome::Survived);
    let mut driver = Driver::default();
    let started = std::time::Instant::now();
    while b.is_alive(&s) == Ok(true) && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    driver.read(&b, &s);
    let status = b.status(&s).unwrap();
    assert_eq!(status.state, SessionState::Ended);
    assert!(
        status.turn_ended,
        "the turn end written while the controller was down is read"
    );
    assert_eq!(b.is_alive(&s), Ok(false));
    assert_eq!(b.process_id(&s), None);
}
