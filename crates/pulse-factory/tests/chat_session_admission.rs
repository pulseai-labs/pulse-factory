//! AC-7 of `r1.s3.w2`: the admission that starts a chat session, the one that ends it, and the
//! admitted redaction of one chat item (ADR-0003, ADR-0015). A real controller runs in the test
//! process (`Live`), and the test calls the controller functions of Approach 6. Every repo is a
//! temporary directory made with `common::git`. A remote is a URL string set with `git remote add`
//! and nothing is fetched. No test launches `claude` or any adapter.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use common::ctl::*;
use common::*;
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, ChatItemKind, ChatSessionState, Commit, Committed, Effect,
    NEW_ID, NewAdmission, NewChatItem, NewChatSession, ORCHESTRATOR_AGENT, SessionWorktree, Store,
    ToolText, WorktreeCreatedBy,
};
use pulse_factory::controller::sessions::{
    ChatItemRedactArgs, ChatSessionEndArgs, ChatSessionStartArgs,
};
use pulse_factory::controller::worktrees::WorktreeLive;
use pulse_factory::controller::{Config, Controller};
use rusqlite::Connection;
use serde_json::Value;

const T: &str = "2026-11-08T12:00:00.000Z";

fn dir(live: &Live, name: &str) -> PathBuf {
    let d = live.sb.root.path().join("work").join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(&d).unwrap()
}

fn commit(d: &Path) {
    git(
        d,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "x",
        ],
    );
}

/// A git repo with one commit, under the sandbox.
fn repo(live: &Live, name: &str) -> PathBuf {
    let d = dir(live, name);
    git(&d, &["init", "-q", "-b", "main"]);
    commit(&d);
    d
}

fn path_str(p: &Path) -> String {
    p.display().to_string()
}

/// `project add`, answering the new project's ID.
fn add(live: &Live, root: &Path) -> String {
    let out = live.cli(&["project", "add", &path_str(root), "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    v["project"]["id"].as_str().unwrap().to_string()
}

fn ledger_lines(live: &Live) -> Vec<String> {
    ledger_at(&live.sb)
}

fn ledger_at(sb: &Sandbox) -> Vec<String> {
    std::fs::read_to_string(sb.state.join("controller.ledger"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn admissions(c: &Controller) -> usize {
    c.store().admissions(CONTROLLER_SCOPE).unwrap().len()
}

fn start_args(project: &str, command_id: &str) -> ChatSessionStartArgs {
    ChatSessionStartArgs {
        project_id: project.to_string(),
        agent_id: ORCHESTRATOR_AGENT.to_string(),
        command_id: command_id.to_string(),
    }
}

fn end_args(session: &str, command_id: &str) -> ChatSessionEndArgs {
    ChatSessionEndArgs {
        session_id: session.to_string(),
        command_id: command_id.to_string(),
    }
}

fn redact_args(session: &str, seq: u64, command_id: &str) -> ChatItemRedactArgs {
    ChatItemRedactArgs {
        session_id: session.to_string(),
        seq,
        command_id: command_id.to_string(),
    }
}

/// A start that must be refused: one admission and one ledger line, and the reason.
fn refused_start(live: &Live, args: &ChatSessionStartArgs) -> String {
    let (a, l) = (admissions(&live.controller), ledger_lines(live).len());
    let why = live.controller.start_chat_session(args).unwrap_err();
    assert_eq!(admissions(&live.controller), a + 1, "one admission: {why}");
    let lines = ledger_lines(live);
    assert_eq!(lines.len(), l + 1, "one ledger line: {why}");
    assert!(
        lines
            .last()
            .unwrap()
            .contains("chat_session_start refused reason="),
        "{lines:?}"
    );
    why
}

fn item(session: &str, kind: ChatItemKind, payload: &str) -> NewChatItem {
    NewChatItem {
        session_id: session.to_string(),
        kind,
        payload: payload.to_string(),
        tool_input: None,
        tool_output: None,
        answers_seq: None,
        usage: None,
        created_at: T.to_string(),
    }
}

fn the_live_check(live: &Live, project: &str, path: &Path, row: Option<&str>) -> WorktreeLive {
    let store = live.controller.store();
    let p = store.project(project).unwrap().unwrap();
    let row = row.map(|id| store.worktree(id).unwrap().unwrap());
    live.controller
        .check_worktree(&p, &path_str(path), row.as_ref())
        .unwrap()
}

#[test]
fn the_first_start_records_the_main_worktree_and_a_folder_project_gets_its_own() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    git(
        &r,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    let p1 = add(&live, &r);
    let folder = dir(&live, "plain");
    let p2 = add(&live, &folder);
    let store = live.controller.store();

    // `project add` made no worktree row.
    assert!(store.worktrees(&p1).unwrap().is_empty());
    assert!(store.worktrees(&p2).unwrap().is_empty());
    assert!(store.live_chat_sessions().unwrap().is_empty());

    let lines_before = ledger_lines(&live).len();
    let started = live
        .controller
        .start_chat_session(&start_args(&p1, "start-1"))
        .unwrap();
    assert!(!started.replayed);
    assert_eq!(started.session.id, "S-1");
    assert_eq!(started.worktree.id, "W-1");
    assert_eq!(started.worktree.created_by, WorktreeCreatedBy::Root);
    assert_eq!(started.worktree.path, path_str(&r));
    assert_eq!(started.worktree.project_id, "P-1");
    assert!(started.worktree.gone_at.is_none() && started.worktree.removed_at.is_none());
    // The session starts from the agent's defaults: idle, and no runtime ref.
    let agent = store.agent(ORCHESTRATOR_AGENT).unwrap().unwrap();
    let s = &started.session;
    assert_eq!(s.state, ChatSessionState::Idle);
    assert_eq!(s.runtime_session_ref, None);
    assert_eq!(s.worktree_id, "W-1");
    assert_eq!(s.agent_id, ORCHESTRATOR_AGENT);
    assert_eq!(s.runtime, agent.runtime);
    assert_eq!(s.model, agent.model);
    assert_eq!(s.effort, agent.effort);
    assert_eq!(s.permission_mode, agent.permission_mode);
    assert!(s.ended_at.is_none());
    assert_eq!(store.worktrees(&p1).unwrap().len(), 1);

    // One ledger line, with the session, the worktree, the path and the command ID. The key did not
    // change, so the line holds no key.
    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), lines_before + 1, "{lines:#?}");
    assert!(
        lines.last().unwrap().ends_with(&format!(
            "chat_session_start S-1 admitted project=P-1 worktree=W-1 path={} agent=orchestrator command=start-1",
            r.display()
        )),
        "{}",
        lines.last().unwrap()
    );

    // A folder project's start records its folder.
    let second = live
        .controller
        .start_chat_session(&start_args(&p2, "start-2"))
        .unwrap();
    assert_eq!(second.session.id, "S-2");
    assert_eq!(second.worktree.id, "W-2");
    assert_eq!(second.worktree.created_by, WorktreeCreatedBy::Root);
    assert_eq!(second.worktree.path, path_str(&folder));
    assert!(
        ledger_lines(&live).last().unwrap().ends_with(&format!(
            "chat_session_start S-2 admitted project=P-2 worktree=W-2 path={} agent=orchestrator command=start-2",
            folder.display()
        )),
        "{:?}",
        ledger_lines(&live)
    );
}

#[test]
fn a_project_has_one_live_orchestrator_and_a_command_id_replays() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let p1 = add(&live, &r);
    let p2 = add(&live, &dir(&live, "plain"));
    let c = &live.controller;
    let first = c.start_chat_session(&start_args(&p1, "start-1")).unwrap();
    assert_eq!(first.session.id, "S-1");

    // A second start with a new command ID is refused, and it names the live session.
    let why = refused_start(&live, &start_args(&p1, "start-1b"));
    assert!(why.contains("S-1"), "{why}");
    assert_eq!(c.live_orchestrator(&p1).unwrap().unwrap().id, "S-1");
    assert!(c.live_orchestrator(&p2).unwrap().is_none());
    assert_eq!(c.store().chat_sessions(&p1).unwrap().len(), 1);

    // The first command ID with the same arguments is a replay: the first record, and no line.
    let (a, l) = (admissions(c), ledger_lines(&live));
    let again = c.start_chat_session(&start_args(&p1, "start-1")).unwrap();
    assert!(again.replayed);
    assert_eq!(again.session.id, "S-1");
    assert_eq!(again.worktree.id, "W-1");
    assert_eq!(admissions(c), a);
    assert_eq!(ledger_lines(&live), l);

    // A replay of a refusal is the same refusal, and it writes nothing either.
    let why2 = c
        .start_chat_session(&start_args(&p1, "start-1b"))
        .unwrap_err();
    assert_eq!(why2, why);
    assert_eq!(admissions(c), a);
    assert_eq!(ledger_lines(&live), l);

    // The same command ID with another project is a conflict that names the ID.
    let err = c
        .start_chat_session(&start_args(&p2, "start-1"))
        .unwrap_err();
    assert!(err.contains("start-1"), "{err}");
    assert!(err.contains("other arguments"), "{err}");
    assert_eq!(admissions(c), a);
    assert_eq!(ledger_lines(&live), l);
}

#[test]
fn a_new_start_after_an_end_is_a_new_session_with_no_runtime_ref() {
    let live = Live::start();
    let p1 = add(&live, &repo(&live, "gitty"));
    let c = &live.controller;
    c.start_chat_session(&start_args(&p1, "start-1")).unwrap();
    assert!(
        c.store()
            .set_chat_runtime_ref("S-1", "ref-of-the-first", T)
            .unwrap()
    );
    assert_eq!(
        c.store()
            .chat_session("S-1")
            .unwrap()
            .unwrap()
            .runtime_session_ref
            .as_deref(),
        Some("ref-of-the-first")
    );

    let l = ledger_lines(&live).len();
    let ended = c.end_chat_session(&end_args("S-1", "end-1")).unwrap();
    assert!(!ended.replayed);
    assert!(ended.session.ended_at.is_some());
    // The end keeps the ref: the old session's own record.
    assert_eq!(
        ended.session.runtime_session_ref.as_deref(),
        Some("ref-of-the-first")
    );
    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), l + 1);
    assert!(
        lines
            .last()
            .unwrap()
            .ends_with("chat_session_end S-1 admitted command=end-1"),
        "{}",
        lines.last().unwrap()
    );
    assert!(c.live_orchestrator(&p1).unwrap().is_none());

    // The same end again is a replay. An end of an ended session under a new ID is refused, with
    // one line. An unknown session is refused too. A bad command ID stores nothing.
    let a = admissions(c);
    assert!(
        c.end_chat_session(&end_args("S-1", "end-1"))
            .unwrap()
            .replayed
    );
    assert_eq!(admissions(c), a);
    let why = c.end_chat_session(&end_args("S-1", "end-2")).unwrap_err();
    assert!(why.contains("ended already"), "{why}");
    let why = c.end_chat_session(&end_args("S-9", "end-3")).unwrap_err();
    assert!(why.contains("no chat session S-9"), "{why}");
    assert_eq!(admissions(c), a + 2);
    assert_eq!(ledger_lines(&live).len(), l + 3);
    assert!(c.end_chat_session(&end_args("S-1", "bad id")).is_err());
    assert_eq!(admissions(c), a + 2, "a bad command ID stores nothing");

    // The new start is a new session in the same worktree row, with no runtime ref.
    let second = c.start_chat_session(&start_args(&p1, "start-2")).unwrap();
    assert!(!second.replayed);
    assert_eq!(second.session.id, "S-2");
    assert_eq!(second.worktree.id, "W-1", "the live row is used again");
    assert_eq!(second.session.runtime_session_ref, None);
    assert_eq!(c.store().worktrees(&p1).unwrap().len(), 1);
    assert_eq!(c.live_orchestrator(&p1).unwrap().unwrap().id, "S-2");

    // The first start's command ID still answers the first record, although the session has ended.
    let replay = c.start_chat_session(&start_args(&p1, "start-1")).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.session.id, "S-1");
    assert!(replay.session.ended_at.is_some());
}

#[test]
fn a_linked_worktree_that_is_deleted_is_gone_once_and_its_row_stays() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let p1 = add(&live, &r);
    let store = live.controller.store().clone();

    // A linked worktree of the project. No verb records one in this release, so the row is made
    // as a later release's commit would make it, through the store port.
    let linked = dir(&live, "linked-parent").join("feature");
    git(
        &r,
        &["worktree", "add", "-q", &path_str(&linked), "-b", "feature"],
    );
    let linked = std::fs::canonicalize(&linked).unwrap();
    let committed = store
        .commit(Commit {
            run_id: CONTROLLER_SCOPE.to_string(),
            command_id: "link-1".to_string(),
            content: "{}".to_string(),
            admission: NewAdmission {
                kind: "chat_session_start".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: Some(NEW_ID.to_string()),
                role: None,
                model: None,
                attempt: None,
                admitted_at: T.to_string(),
            },
            effects: vec![Effect::StartChatSession(NewChatSession {
                project_id: p1.clone(),
                agent_id: ORCHESTRATOR_AGENT.to_string(),
                worktree: SessionWorktree::Record {
                    path: path_str(&linked),
                    created_by: WorktreeCreatedBy::External,
                },
                created_at: T.to_string(),
            })],
            ledger: None,
        })
        .unwrap();
    assert!(
        matches!(committed, Committed::Applied { .. }),
        "{committed:?}"
    );
    let row = store.live_worktree_at(&path_str(&linked)).unwrap().unwrap();
    assert_eq!(row.id, "W-1");
    assert_eq!(row.created_by, WorktreeCreatedBy::External);

    // Live while it is there, with its branch.
    assert_eq!(
        the_live_check(&live, &p1, &linked, Some("W-1")),
        WorktreeLive::Live {
            branch: Some("feature".to_string()),
            detached: false
        }
    );
    assert!(store.worktree("W-1").unwrap().unwrap().gone_at.is_none());

    // Deleted: gone. The first read that sees it gone marks the row, the second leaves the mark.
    std::fs::remove_dir_all(&linked).unwrap();
    assert!(matches!(
        the_live_check(&live, &p1, &linked, Some("W-1")),
        WorktreeLive::Gone(_)
    ));
    let marked = store.worktree("W-1").unwrap().unwrap();
    assert!(marked.gone_at.is_some(), "{marked:?}");
    assert_eq!(marked.path, path_str(&linked), "the row stays");
    assert!(marked.removed_at.is_none());
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(matches!(
        the_live_check(&live, &p1, &linked, Some("W-1")),
        WorktreeLive::Gone(_)
    ));
    assert_eq!(
        store.worktree("W-1").unwrap().unwrap().gone_at,
        marked.gone_at,
        "a second check leaves gone_at as it was"
    );
    // Git still lists the deleted directory, as prunable, until `git worktree prune`. It counts as
    // not listed, so a path that is back as a plain directory is no worktree either.
    std::fs::create_dir_all(&linked).unwrap();
    assert!(matches!(
        the_live_check(&live, &p1, &linked, None),
        WorktreeLive::Gone(_)
    ));
    // Nothing of this made an admission or a ledger line.
    assert!(
        ledger_lines(&live)
            .iter()
            .all(|l| !l.contains("chat_session_start")),
        "{:?}",
        ledger_lines(&live)
    );
}

#[test]
fn a_root_that_is_deleted_refuses_a_start_and_a_root_that_is_back_gets_a_new_row() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let p1 = add(&live, &r);
    let p2 = add(&live, &dir(&live, "plain"));
    let c = &live.controller;
    assert_eq!(
        c.start_chat_session(&start_args(&p1, "s1"))
            .unwrap()
            .worktree
            .id,
        "W-1"
    );
    assert_eq!(
        c.start_chat_session(&start_args(&p2, "s2"))
            .unwrap()
            .worktree
            .id,
        "W-2"
    );
    c.end_chat_session(&end_args("S-1", "e1")).unwrap();

    std::fs::remove_dir_all(&r).unwrap();
    let why = refused_start(&live, &start_args(&p1, "s3"));
    assert!(
        why.starts_with(&format!("the worktree {} is gone", r.display())),
        "{why}"
    );
    let w1 = c.store().worktree("W-1").unwrap().unwrap();
    assert!(w1.gone_at.is_some(), "the start marked W-1 gone: {w1:?}");
    assert!(
        c.live_orchestrator(&p1).unwrap().is_none(),
        "nothing started"
    );

    // The root is made again as a repo: the next start records a new row.
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    commit(&r);
    let back = c.start_chat_session(&start_args(&p1, "s4")).unwrap();
    assert_eq!(back.worktree.id, "W-3", "{back:?}");
    assert_eq!(back.session.worktree_id, "W-3");
    assert_eq!(back.worktree.path, path_str(&r));
    let all = c.store().worktrees(&p1).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].gone_at.is_some(), "W-1 stays gone");
    assert!(all[1].gone_at.is_none());
}

#[test]
fn a_root_without_its_git_directory_and_a_deleted_folder_refuse_as_gone() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let folder = dir(&live, "plain");
    let p1 = add(&live, &r);
    let p2 = add(&live, &folder);
    let c = &live.controller;
    c.start_chat_session(&start_args(&p1, "s1")).unwrap();
    c.start_chat_session(&start_args(&p2, "s2")).unwrap();
    c.end_chat_session(&end_args("S-1", "e1")).unwrap();
    c.end_chat_session(&end_args("S-2", "e2")).unwrap();

    std::fs::rename(r.join(".git"), r.join(".git-off")).unwrap();
    let why = refused_start(&live, &start_args(&p1, "s3"));
    assert!(
        why.contains(&format!("the worktree {} is gone", r.display())),
        "{why}"
    );
    assert!(
        c.store()
            .worktree("W-1")
            .unwrap()
            .unwrap()
            .gone_at
            .is_some()
    );

    std::fs::remove_dir_all(&folder).unwrap();
    let why = refused_start(&live, &start_args(&p2, "s4"));
    assert!(
        why.contains(&format!("the worktree {} is gone", folder.display())),
        "{why}"
    );
    assert!(
        c.store()
            .worktree("W-2")
            .unwrap()
            .unwrap()
            .gone_at
            .is_some()
    );

    // A root that is gone before its first start has no row to mark, and is refused the same.
    let other = dir(&live, "never-started");
    let p3 = add(&live, &other);
    std::fs::remove_dir_all(&other).unwrap();
    let why = refused_start(&live, &start_args(&p3, "s5"));
    assert!(why.contains("is gone"), "{why}");
    assert!(c.store().worktrees(&p3).unwrap().is_empty());
}

#[test]
fn the_other_refusals_are_one_admission_and_one_line_each() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let p1 = add(&live, &r);
    let p2 = add(&live, &dir(&live, "gone-soon"));
    let c = &live.controller;

    assert!(refused_start(&live, &start_args("P-9", "u1")).contains("no project P-9"));
    let out: Output = live.cli(&["project", "remove", &p2]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(refused_start(&live, &start_args(&p2, "u2")).contains("is removed"));
    let mut a = start_args(&p1, "u3");
    a.agent_id = "nobody".to_string();
    assert!(refused_start(&live, &a).contains("no agent nobody"));

    // An agent that is not the Orchestrator has no chat session in this release. No verb makes
    // another agent, so the row is put in the table by hand.
    let conn = Connection::open(live.sb.state.join("store.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO agents VALUES ('helper', 'Helper', 'Helps', 0, 'accent', 'helper', NULL, \
         'claude-code', 'm', 'high', 'default', '[]', '{}', '{}', 't0')",
        [],
    )
    .unwrap();
    drop(conn);
    let mut a = start_args(&p1, "u4");
    a.agent_id = "helper".to_string();
    assert!(
        refused_start(&live, &a)
            .contains("only the Orchestrator has chat sessions in this release")
    );

    // A bad command ID stores nothing: no admission, no line, no session.
    let (n, l) = (admissions(c), ledger_lines(&live).len());
    for bad in ["", "has space", "semi;colon", &"x".repeat(129)] {
        assert!(
            c.start_chat_session(&start_args(&p1, bad)).is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(admissions(c), n);
    assert_eq!(ledger_lines(&live).len(), l);
    assert!(c.store().chat_sessions(&p1).unwrap().is_empty());
    assert!(c.store().worktrees(&p1).unwrap().is_empty());
}

#[test]
fn a_project_with_a_live_session_cannot_be_removed_until_the_session_ends() {
    let live = Live::start();
    let p1 = add(&live, &repo(&live, "gitty"));
    let c = &live.controller;
    c.start_chat_session(&start_args(&p1, "s1")).unwrap();
    c.end_chat_session(&end_args("S-1", "e1")).unwrap();
    c.start_chat_session(&start_args(&p1, "s2")).unwrap();

    let out = live.cli(&["project", "remove", &p1]);
    assert_eq!(code(&out), 1, "{}", stdout(&out));
    assert!(
        stderr(&out).contains("project P-1 has a live chat session S-2; end it first"),
        "{}",
        stderr(&out)
    );
    assert!(
        c.store()
            .project(&p1)
            .unwrap()
            .unwrap()
            .removed_at
            .is_none()
    );

    c.end_chat_session(&end_args("S-2", "e2")).unwrap();
    let out = live.cli(&["project", "remove", &p1]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let w1 = c.store().worktree("W-1").unwrap().unwrap();
    assert!(
        w1.removed_at.is_some(),
        "the project's worktree row is removed: {w1:?}"
    );
    // A removed project starts nothing.
    assert!(refused_start(&live, &start_args(&p1, "s3")).contains("is removed"));
}

#[test]
fn the_branch_is_main_for_the_root_and_detached_after_a_detach() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let p1 = add(&live, &r);
    live.controller
        .start_chat_session(&start_args(&p1, "s1"))
        .unwrap();
    assert_eq!(
        the_live_check(&live, &p1, &r, Some("W-1")),
        WorktreeLive::Live {
            branch: Some("main".to_string()),
            detached: false
        }
    );
    git(&r, &["checkout", "-q", "--detach"]);
    assert_eq!(
        the_live_check(&live, &p1, &r, Some("W-1")),
        WorktreeLive::Live {
            branch: None,
            detached: true
        }
    );
    // A folder has no branch.
    let folder = dir(&live, "plain");
    let p2 = add(&live, &folder);
    assert_eq!(
        the_live_check(&live, &p2, &folder, None),
        WorktreeLive::Live {
            branch: None,
            detached: false
        }
    );
    assert!(
        live.controller
            .store()
            .worktree("W-1")
            .unwrap()
            .unwrap()
            .gone_at
            .is_none()
    );
}

#[test]
fn a_start_stores_a_changed_remote_key_in_its_own_commit_and_never_a_url() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    git(
        &r,
        &["remote", "add", "origin", "https://github.com/o/old.git"],
    );
    let p1 = add(&live, &r);
    let c = &live.controller;
    let stored = |c: &Controller| c.store().project(&p1).unwrap().unwrap();
    assert_eq!(stored(c).remote_key.as_deref(), Some("github.com/o/old"));

    let secret = "https://user:tok@github.com/o/new.git";
    git(&r, &["remote", "set-url", "origin", secret]);
    // A list reads the key live and writes nothing.
    let n = admissions(c);
    let out = live.cli(&["project", "list", "--json"]);
    assert!(stdout(&out).contains("github.com/o/new"));
    assert_eq!(admissions(c), n);
    assert_eq!(stored(c).remote_key.as_deref(), Some("github.com/o/old"));

    let started = c.start_chat_session(&start_args(&p1, "s1")).unwrap();
    assert_eq!(stored(c).remote_key.as_deref(), Some("github.com/o/new"));
    let line = ledger_lines(&live).last().unwrap().clone();
    assert!(
        line.ends_with(&format!(
            "chat_session_start S-1 admitted project=P-1 worktree=W-1 path={} agent=orchestrator remote_key=github.com/o/new command=s1",
            r.display()
        )),
        "{line}"
    );

    // A replay of that start writes nothing and changes nothing.
    let (n, l, row) = (admissions(c), ledger_lines(&live), stored(c));
    let again = c.start_chat_session(&start_args(&p1, "s1")).unwrap();
    assert!(again.replayed);
    assert_eq!(again.session.id, started.session.id);
    assert_eq!((admissions(c), ledger_lines(&live), stored(c)), (n, l, row));

    // A start with the key unchanged changes no project field, and its line holds no key.
    c.end_chat_session(&end_args("S-1", "e1")).unwrap();
    let row = stored(c);
    c.start_chat_session(&start_args(&p1, "s2")).unwrap();
    assert_eq!(stored(c), row);
    let line = ledger_lines(&live).last().unwrap().clone();
    assert!(!line.contains("remote_key="), "{line}");
    assert!(line.ends_with("agent=orchestrator command=s2"), "{line}");

    // A failed read leaves the stored key as it is: the repo is broken, and the session start is
    // refused as gone, so the key is not touched either way.
    std::fs::rename(r.join(".git"), r.join(".git-off")).unwrap();
    c.end_chat_session(&end_args("S-2", "e2")).unwrap();
    let _ = refused_start(&live, &start_args(&p1, "s3"));
    assert_eq!(stored(c).remote_key.as_deref(), Some("github.com/o/new"));

    // No remote URL is in an answer, a ledger line or a row.
    let mut seen = vec![
        ledger_lines(&live).join("\n"),
        format!("{:?}", c.store().admissions(CONTROLLER_SCOPE).unwrap()),
        format!("{:?}", c.store().projects().unwrap()),
        format!("{:?}", c.store().worktrees(&p1).unwrap()),
        format!("{:?}", c.store().chat_sessions(&p1).unwrap()),
        format!("{started:?}"),
        stdout(&out),
        stderr(&out),
    ];
    seen.push(stdout(&live.cli(&["project", "list"])));
    for text in &seen {
        for needle in [":tok@", "tok@github", secret, "user:", "https://"] {
            assert!(!text.contains(needle), "{needle} appears in {text}");
        }
    }
}

#[test]
fn a_restart_leaves_a_live_session_idle_with_its_ref_and_closes_its_pending_request() {
    let live = Live::start();
    let p1 = add(&live, &repo(&live, "gitty"));
    let c = &live.controller;
    c.start_chat_session(&start_args(&p1, "s1")).unwrap();
    c.end_chat_session(&end_args("S-1", "e1")).unwrap();
    c.start_chat_session(&start_args(&p1, "s2")).unwrap();
    let store = c.store();
    assert!(
        store
            .set_chat_session_state("S-2", ChatSessionState::Working, T)
            .unwrap()
    );
    assert!(store.set_chat_runtime_ref("S-2", "ref-2", T).unwrap());
    store
        .append_chat_item(item(
            "S-2",
            ChatItemKind::OperatorMessage,
            "{\"text\":\"hello\"}",
        ))
        .unwrap();
    let request = store
        .append_chat_item(item(
            "S-2",
            ChatItemKind::ApprovalRequest,
            "{\"tool\":\"Bash\"}",
        ))
        .unwrap();
    // A question that was answered before the restart stays as it was.
    let question = store
        .append_chat_item(item(
            "S-2",
            ChatItemKind::QuestionRequest,
            "{\"q\":\"which?\"}",
        ))
        .unwrap();
    let mut answer = item(
        "S-2",
        ChatItemKind::QuestionAnswer,
        "{\"outcome\":\"answered\"}",
    );
    answer.answers_seq = Some(question.seq);
    store.append_chat_item(answer).unwrap();
    let before = store.chat_session("S-2").unwrap().unwrap();
    assert_eq!(before.state, ChatSessionState::Working);
    assert_eq!(store.pending_requests("S-2").unwrap().len(), 1);
    let lines = ledger_lines(&live);
    let n = admissions(c);

    // A fresh controller on the same state directory, as `project_verbs.rs` reopens one.
    let c2 = Controller::open(
        Config::new(live.sb.state.clone(), 3_600_000, BIN.into()),
        live.ports.clone(),
    )
    .unwrap();
    let store = c2.store();
    let after = store.chat_session("S-2").unwrap().unwrap();
    assert!(after.ended_at.is_none(), "the session is still live");
    assert_eq!(after.state, ChatSessionState::Idle);
    assert_eq!(after.runtime_session_ref.as_deref(), Some("ref-2"));
    assert_eq!(
        (&after.model, &after.effort, &after.permission_mode),
        (&before.model, &before.effort, &before.permission_mode)
    );
    assert_eq!(after.id, before.id);
    assert_eq!(store.live_chat_sessions().unwrap().len(), 1);

    assert!(store.pending_requests("S-2").unwrap().is_empty());
    let items = store.chat_items_after("S-2", 0, 100).unwrap();
    let lost: Vec<_> = items
        .iter()
        .filter(|i| i.kind == "approval_answer")
        .collect();
    assert_eq!(lost.len(), 1, "{items:#?}");
    assert_eq!(lost[0].answers_seq, Some(request.seq));
    let payload: Value = serde_json::from_str(&lost[0].payload).unwrap();
    assert_eq!(payload["outcome"], "lost");
    assert_eq!(
        items.iter().filter(|i| i.kind == "question_answer").count(),
        1,
        "the answered question gets no second answer"
    );

    // The restart made no command: no line, no admission.
    assert_eq!(ledger_at(&live.sb), lines);
    assert_eq!(admissions(&c2), n);

    // A new start for the project is refused and names the session that waits for `--resume`.
    let why = c2.start_chat_session(&start_args(&p1, "s3")).unwrap_err();
    assert!(why.contains("S-2"), "{why}");
    assert_eq!(c2.live_orchestrator(&p1).unwrap().unwrap().id, "S-2");

    // A second restart adds nothing more.
    let items_before = store.chat_items_after("S-2", 0, 100).unwrap().len();
    drop(c2);
    let c3 = Controller::open(
        Config::new(live.sb.state.clone(), 3_600_000, BIN.into()),
        live.ports.clone(),
    )
    .unwrap();
    assert_eq!(
        c3.store().chat_items_after("S-2", 0, 100).unwrap().len(),
        items_before
    );
}

#[test]
fn a_redaction_is_an_admitted_command_that_holds_none_of_the_items_text() {
    let live = Live::start();
    let p1 = add(&live, &repo(&live, "gitty"));
    let c = &live.controller;
    c.start_chat_session(&start_args(&p1, "s1")).unwrap();
    let store = c.store();
    let secret = "sk-live-0123456789";
    let mut call = item(
        "S-1",
        ChatItemKind::ToolCall,
        &format!("{{\"tool\":\"Bash\",\"note\":\"{secret}\"}}"),
    );
    call.tool_input = Some(ToolText::uncapped(format!("export KEY={secret}")));
    call.tool_output = Some(ToolText::uncapped(format!("KEY={secret}")));
    let stored = store.append_chat_item(call).unwrap();
    assert_eq!(stored.seq, 1);
    let request = store
        .append_chat_item(item("S-1", ChatItemKind::ApprovalRequest, "{}"))
        .unwrap();

    let l = ledger_lines(&live).len();
    let done = c
        .redact_chat_item(&redact_args("S-1", stored.seq, "redact-1"))
        .unwrap();
    assert!(!done.replayed);
    assert_eq!(done.item.payload, "{\"redacted\":true}");
    assert_eq!(done.item.tool_input.as_deref(), Some("[redacted]"));
    assert_eq!(done.item.tool_output.as_deref(), Some("[redacted]"));
    assert!(done.item.redacted_at.is_some());
    assert_eq!(done.item.kind, "tool_call");
    assert_eq!(done.item.seq, 1);
    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), l + 1, "{lines:#?}");
    let line = lines.last().unwrap();
    assert!(
        line.ends_with("chat_item_redact S-1 seq=1 admitted command=redact-1"),
        "{line}"
    );
    assert!(!line.contains(secret) && !line.contains("Bash"), "{line}");
    for a in store.admissions(CONTROLLER_SCOPE).unwrap() {
        assert!(!format!("{a:?}").contains(secret), "{a:?}");
    }

    // The same command ID answers the first record and writes no line.
    let n = admissions(c);
    let again = c
        .redact_chat_item(&redact_args("S-1", stored.seq, "redact-1"))
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.item, done.item);
    assert_eq!(ledger_lines(&live).len(), l + 1);
    assert_eq!(admissions(c), n);

    // A second redaction with a new ID is refused, with one refused line.
    let why = c
        .redact_chat_item(&redact_args("S-1", stored.seq, "redact-2"))
        .unwrap_err();
    assert!(why.contains("redacted"), "{why}");
    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), l + 2);
    assert!(
        lines
            .last()
            .unwrap()
            .contains("chat_item_redact refused reason="),
        "{}",
        lines.last().unwrap()
    );
    assert!(!lines.last().unwrap().contains(secret));
    // An unknown item or session is refused too.
    assert!(
        c.redact_chat_item(&redact_args("S-1", 99, "redact-3"))
            .is_err()
    );
    assert!(
        c.redact_chat_item(&redact_args("S-9", 1, "redact-4"))
            .is_err()
    );
    assert!(
        c.redact_chat_item(&redact_args("S-1", 1, "bad id"))
            .is_err()
    );
    assert_eq!(
        ledger_lines(&live).len(),
        l + 4,
        "a bad command ID writes no line"
    );

    // A redacted request stays a request, so it stays pending until its answer.
    c.redact_chat_item(&redact_args("S-1", request.seq, "redact-5"))
        .unwrap();
    assert_eq!(store.pending_requests("S-1").unwrap().len(), 1);
    let mut answer = item(
        "S-1",
        ChatItemKind::ApprovalAnswer,
        "{\"outcome\":\"answered\"}",
    );
    answer.answers_seq = Some(request.seq);
    store.append_chat_item(answer).unwrap();
    assert!(store.pending_requests("S-1").unwrap().is_empty());

    // The redaction covers an ended session too.
    c.end_chat_session(&end_args("S-1", "e1")).unwrap();
    let after_end = c
        .redact_chat_item(&redact_args("S-1", answer_seq(store), "redact-6"))
        .unwrap();
    assert_eq!(after_end.item.payload, "{\"redacted\":true}");
    assert!(
        c.redact_chat_item(&redact_args("S-1", request.seq, "redact-7"))
            .is_err(),
        "the request was redacted already"
    );
}

/// The `seq` of the last item of `S-1`.
fn answer_seq(store: &Arc<pf_store_sqlite::SqliteStore>) -> u64 {
    store
        .chat_items_after("S-1", 0, 100)
        .unwrap()
        .last()
        .unwrap()
        .seq
}
