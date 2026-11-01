//! AC-10: a push whose target the rules could not read is measured in git, not read from the
//! output of the call (`r0.s1.w5`, Approach 9). The tests use real scratch repositories with a
//! local bare remote and a stub `omp`. They spend nothing and reach no network.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use pf_adapter_omp::OmpAdapter;
use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{AgentError, AgentPort, SessionEvent, SessionRef};

struct Started {
    dir: tempfile::TempDir,
    adapter: OmpAdapter,
    session: SessionRef,
    events: PathBuf,
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

fn commit(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-m", file]);
}

/// A bare remote with `main`, a clone `clone` of it with the branches `topic` and `other` (each
/// holding one more commit than `main`), and the clone left on `topic`. Nothing of `topic` or
/// `other` is pushed.
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

fn try_start(
    dir: tempfile::TempDir,
    work: &Path,
    extra: &[(&str, &str)],
) -> Result<Started, (tempfile::TempDir, AgentError)> {
    let omp = stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN);
    let adapter = adapter(dir.path(), omp);
    let mut settings = vec![
        ("push_dir", work.to_str().unwrap()),
        ("push_branch", "topic"),
        ("default_branch", "main"),
        ("remote", "origin"),
    ];
    settings.extend_from_slice(extra);
    match adapter.launch(&plan(work, &settings)) {
        Ok(session) => {
            let events = adapter.event_file(&session).unwrap();
            Ok(Started {
                dir,
                adapter,
                session,
                events,
            })
        }
        Err(e) => Err((dir, e)),
    }
}

fn start(dir: tempfile::TempDir, work: &Path, extra: &[(&str, &str)]) -> Started {
    match try_start(dir, work, extra) {
        Ok(s) => s,
        Err((_, e)) => panic!("launch failed: {e:?}"),
    }
}

fn drain(s: &Started) -> Vec<SessionEvent> {
    s.adapter.drain_events(&s.session).unwrap()
}

fn bash_line(command: &str, exit_code: i64) -> String {
    serde_json::json!({"ev":"bash","t":10,"command":command,"exitCode":exit_code}).to_string()
}

/// The seat's unread push, as `ossify`'s close writes it.
const UNREAD: &str = r#"git -C "$wt" push -u origin "$b""#;

type PushLine = (String, String, PushOutcome);

fn pushes(events: Vec<SessionEvent>) -> Vec<PushLine> {
    events
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
        .collect()
}

fn moved(events: &[SessionEvent]) -> Vec<(String, String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::DefaultBranchMoved {
                branch,
                before,
                after,
                ..
            } => Some((branch.clone(), before.clone(), after.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn one_variable_push_gives_one_push_for_that_branch_with_its_real_sha() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let topic = git(&work, &["rev-parse", "topic"]);
    git(&work, &["push", "-u", "origin", "topic"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    assert_eq!(
        pushes(drain(&s)),
        vec![("topic".to_string(), topic, PushOutcome::Pushed)]
    );
}

#[test]
fn two_variable_branches_give_two_pushes_ending_with_push_branch() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let topic = git(&work, &["rev-parse", "topic"]);
    let other = git(&work, &["rev-parse", "other"]);
    git(&work, &["push", "origin", "topic", "other"]);
    append(
        &s.events,
        &[&bash_line(
            r#"git -C "$wt" push origin "$a" && git -C "$wt" push origin "$b""#,
            0,
        )],
    );
    assert_eq!(
        pushes(drain(&s)),
        vec![
            ("other".to_string(), other, PushOutcome::Pushed),
            ("topic".to_string(), topic, PushOutcome::Pushed),
        ]
    );
}

#[test]
fn a_call_that_moves_no_ref_gives_one_push_for_push_branch() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let topic = git(&work, &["rev-parse", "topic"]);
    // Nothing is pushed: `git push` found nothing to do, or it failed before it moved a ref.
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    assert_eq!(
        pushes(drain(&s)),
        vec![("topic".to_string(), topic, PushOutcome::Pushed)]
    );
    // A failed call is the same one line, and the exit code is its outcome.
    append(&s.events, &[&bash_line(UNREAD, 1)]);
    let got = pushes(drain(&s));
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "topic");
    assert!(matches!(&got[0].2, PushOutcome::Failed(why) if why.contains("exit code 1")));
}

#[test]
fn a_call_that_moves_another_ref_ends_with_push_branch() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let topic = git(&work, &["rev-parse", "topic"]);
    let other = git(&work, &["rev-parse", "other"]);
    // Only `other` moves. `topic` is not pushed, yet `push_branch` closes the call.
    git(&work, &["push", "origin", "other"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    assert_eq!(
        pushes(drain(&s)),
        vec![
            ("other".to_string(), other, PushOutcome::Pushed),
            ("topic".to_string(), topic.clone(), PushOutcome::Pushed),
        ]
    );
    // A ref that did not move since the last read is not reported again.
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    assert_eq!(
        pushes(drain(&s)),
        vec![("topic".to_string(), topic, PushOutcome::Pushed)]
    );
}

#[test]
fn a_branch_that_does_not_resolve_gives_the_sha_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[("push_branch", "no-such-branch")]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    assert_eq!(
        pushes(drain(&s)),
        vec![(
            "no-such-branch".to_string(),
            "unknown".to_string(),
            PushOutcome::Pushed
        )]
    );
}

#[test]
fn text_in_the_output_changes_nothing_and_the_extension_sends_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let topic = git(&work, &["rev-parse", "topic"]);
    // A line that looks like a push summary, in a field that the extension no longer sends.
    let forged = serde_json::json!({
        "ev": "bash", "t": 10, "command": UNREAD, "exitCode": 0,
        "output": "To remote\n   1111111..2222222  topic -> feature-x\n"
    })
    .to_string();
    append(&s.events, &[&forged]);
    assert_eq!(
        pushes(drain(&s)),
        vec![("topic".to_string(), topic, PushOutcome::Pushed)]
    );
    // The extension's bash event holds the command and the exit code only.
    let src = pf_adapter_omp::EXTENSION_SOURCE;
    let emit = src
        .lines()
        .find(|l| l.contains("ev: 'bash'"))
        .expect("the extension emits a bash event");
    assert!(!emit.contains("output"), "{emit}");
    assert!(!src.contains("OUTPUT_LIMIT"));
    assert!(!src.contains("event.content"));
}

#[test]
fn an_unchanged_default_branch_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    git(&work, &["push", "-u", "origin", "topic"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let events = drain(&s);
    assert!(moved(&events).is_empty(), "{events:?}");
}

#[test]
fn a_default_branch_moved_to_a_sha_the_seat_holds_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let before = git(&remote, &["rev-parse", "main"]);
    // The seat itself pushes `topic` over `main`: the remote's `main` moves to a commit that the
    // seat's repository holds.
    git(&work, &["push", "origin", "topic:main"]);
    let after = git(&remote, &["rev-parse", "main"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let events = drain(&s);
    assert_eq!(moved(&events), vec![("main".to_string(), before, after)]);
}

#[test]
fn a_default_branch_moved_to_a_sha_the_seat_lacks_reports_nothing_and_becomes_the_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let (remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    // Someone else moves `main`, from another clone.
    let elsewhere = s.dir.path().join("elsewhere");
    git(
        s.dir.path(),
        &["clone", remote.to_str().unwrap(), "elsewhere"],
    );
    commit(&elsewhere, "c.txt");
    git(&elsewhere, &["push", "origin", "HEAD:main"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let events = drain(&s);
    assert!(moved(&events).is_empty(), "{events:?}");

    // The seat now holds that commit. The next unread push compares against the new value, so
    // `main` has not moved since the last read.
    git(&work, &["fetch", "origin"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let events = drain(&s);
    assert!(moved(&events).is_empty(), "{events:?}");

    // A further move to a commit the seat holds is reported, against the new value.
    let mid = git(&remote, &["rev-parse", "main"]);
    git(&work, &["push", "--force", "origin", "topic:main"]);
    let after = git(&remote, &["rev-parse", "main"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let events = drain(&s);
    assert_eq!(moved(&events), vec![("main".to_string(), mid, after)]);
}

#[test]
fn a_branch_named_x_main_is_not_main() {
    let dir = tempfile::tempdir().unwrap();
    let (remote, work) = scratch_repo(dir.path());
    // `x/main` exists on the remote at launch, with another SHA than `main`.
    git(&work, &["push", "origin", "other:x/main"]);
    let s = start(dir, &work, &[]);
    let main_sha = git(&remote, &["rev-parse", "main"]);
    // `x/main` moves. `main` does not.
    git(&work, &["push", "--force", "origin", "topic:x/main"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let events = drain(&s);
    assert!(moved(&events).is_empty(), "{events:?}");
    // The launch read `main`, not `x/main`: a real move of `main` names `main`'s own SHA.
    git(&work, &["push", "origin", "topic:main"]);
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let m = moved(&drain(&s));
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].1, main_sha);
}

#[test]
fn a_failed_read_reports_the_event_with_an_empty_after() {
    let dir = tempfile::tempdir().unwrap();
    let (remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let before = git(&remote, &["rev-parse", "main"]);
    // The remote cannot be read any more.
    std::fs::rename(&remote, s.dir.path().join("remote.gone")).unwrap();
    append(&s.events, &[&bash_line(UNREAD, 0)]);
    let events = drain(&s);
    assert_eq!(
        moved(&events),
        vec![("main".to_string(), before, String::new())]
    );
}

#[test]
fn a_launch_time_failed_read_refuses_the_launch() {
    // The default branch is not on the remote.
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let Err((_dir, e)) = try_start(dir, &work, &[("default_branch", "no-such-branch")]) else {
        panic!("the launch should have been refused");
    };
    assert!(
        matches!(&e, AgentError::Refused(why) if why.contains("no-such-branch")),
        "{e:?}"
    );

    // The remote cannot be read.
    let dir = tempfile::tempdir().unwrap();
    let (remote, work) = scratch_repo(dir.path());
    std::fs::rename(&remote, dir.path().join("remote.gone")).unwrap();
    let Err((_dir, e)) = try_start(dir, &work, &[]) else {
        panic!("the launch should have been refused");
    };
    assert!(matches!(e, AgentError::Refused(_)), "{e:?}");
}

#[test]
fn a_checkout_of_a_variable_then_a_push_runs_the_check() {
    let dir = tempfile::tempdir().unwrap();
    let (remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let before = git(&remote, &["rev-parse", "main"]);
    git(&work, &["push", "origin", "topic:main"]);
    let after = git(&remote, &["rev-parse", "main"]);
    append(
        &s.events,
        &[&bash_line(r#"git checkout "$b" && git push"#, 0)],
    );
    let events = drain(&s);
    assert_eq!(moved(&events), vec![("main".to_string(), before, after)]);
    // `origin/main` moved too, so it is a line of its own, and `push_branch` ends the call.
    let lines = pushes(events);
    assert_eq!(
        lines.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
        vec!["main", "topic"]
    );
}

#[test]
fn a_push_that_names_its_branch_and_directory_keeps_the_named_branch_path() {
    let dir = tempfile::tempdir().unwrap();
    let (_remote, work) = scratch_repo(dir.path());
    let s = start(dir, &work, &[]);
    let other = git(&work, &["rev-parse", "other"]);
    git(&work, &["push", "origin", "other"]);
    append(
        &s.events,
        &[&bash_line(
            &format!("git -C {} push origin other", work.display()),
            0,
        )],
    );
    // One line for the named branch, and no measurement of `push_branch`.
    assert_eq!(
        pushes(drain(&s)),
        vec![("other".to_string(), other, PushOutcome::Pushed)]
    );
}
