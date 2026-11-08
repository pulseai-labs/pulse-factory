//! AC-6 of `r1.s3.w2`: a project added before migration 0005 has no kind, and the controller gives
//! it one at its next start. The version-4 store file is built by hand, from the text of migrations
//! 0001 to 0004, as the binary of `r1.s2` left it. Then a controller opens it: a real child process
//! when the test reads stderr or the CLI, and `Controller::open` in the test process otherwise.
//! Every repo is a temporary directory, and nothing is fetched.

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::ctl::TestPorts;
use common::*;
use pf_adapter_fake::FakeAdapter;
use pf_engine::port::store::{CONTROLLER_SCOPE, ProjectKind, ProjectRow, Store};
use pulse_factory::controller::sessions::ChatSessionStartArgs;
use pulse_factory::controller::{Config, Controller};
use rusqlite::{Connection, params};
use serde_json::Value;

const MIGRATION_0001: &str = include_str!("../../pf-store-sqlite/migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../../pf-store-sqlite/migrations/0002_requests.sql");
const MIGRATION_0003: &str = include_str!("../../pf-store-sqlite/migrations/0003_projects.sql");
const MIGRATION_0004: &str = include_str!("../../pf-store-sqlite/migrations/0004_devices.sql");

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

fn canonical_dir(path: PathBuf) -> PathBuf {
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

/// The roots of the five projects that a version-4 store holds, as `project add` of `r1.s2` left
/// them: no kind and no key.
struct World {
    sb: Sandbox,
    /// `P-1`: the main worktree of a repo with an `origin`.
    main: PathBuf,
    /// `P-2`: a plain folder.
    plain: PathBuf,
    /// `P-3`: a linked worktree of another repo, so a root that git refuses to add.
    linked: PathBuf,
    /// `P-4`: a root that no longer exists.
    gone: PathBuf,
    /// `P-5`: removed.
    removed: PathBuf,
}

/// The store file at schema version 4 with these projects: `(id, name, root, removed_at)`.
fn version_four(state: &Path, projects: &[(&str, &str, &Path, Option<&str>)]) {
    pf_ipc::prepare_state_dir(state).unwrap();
    let conn = Connection::open(state.join("store.sqlite")).unwrap();
    for (version, name, sql) in [
        (1, "init", MIGRATION_0001),
        (2, "requests", MIGRATION_0002),
        (3, "projects", MIGRATION_0003),
        (4, "devices", MIGRATION_0004),
    ] {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, 't0')",
            params![version, name],
        )
        .unwrap();
    }
    for (id, name, root, removed) in projects {
        conn.execute(
            "INSERT INTO projects (id, name, root, created_at, removed_at) \
             VALUES (?1, ?2, ?3, 't2', ?4)",
            params![id, name, root.display().to_string(), removed],
        )
        .unwrap();
    }
}

fn world() -> World {
    let sb = Sandbox::new();
    let work = sb.root.path().join("work");
    let main = canonical_dir(work.join("main"));
    git(&main, &["init", "-q", "-b", "main"]);
    commit(&main);
    git(
        &main,
        &[
            "remote",
            "add",
            "origin",
            "https://user:tok@GitHub.com/Owner/Repo.git",
        ],
    );
    let plain = canonical_dir(work.join("plain"));
    let other = canonical_dir(work.join("other"));
    git(&other, &["init", "-q", "-b", "main"]);
    commit(&other);
    let linked = work.join("linked");
    git(
        &other,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );
    let linked = std::fs::canonicalize(linked).unwrap();
    let gone = work.join("gone");
    let removed = canonical_dir(work.join("removed"));
    version_four(
        &sb.state,
        &[
            ("P-1", "main", &main, None),
            ("P-2", "plain", &plain, None),
            ("P-3", "linked", &linked, None),
            ("P-4", "gone", &gone, None),
            ("P-5", "removed", &removed, Some("t3")),
        ],
    );
    World {
        sb,
        main,
        plain,
        linked,
        gone,
        removed,
    }
}

fn ports() -> Arc<TestPorts> {
    Arc::new(TestPorts {
        fake: Arc::new(FakeAdapter::new()),
        plans: Default::default(),
        gate: Default::default(),
        pids: Default::default(),
        block_run_json: AtomicBool::new(false),
    })
}

/// Open the controller of this sandbox's state directory in the test process.
fn open(sb: &Sandbox) -> Arc<Controller> {
    Controller::open(
        Config::new(sb.state.clone(), 3_600_000, BIN.into()),
        ports(),
    )
    .unwrap()
}

fn ledger(sb: &Sandbox) -> Vec<String> {
    std::fs::read_to_string(sb.state.join("controller.ledger"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn project(c: &Controller, id: &str) -> ProjectRow {
    c.store().project(id).unwrap().unwrap()
}

/// A controller as a child process, so that its stderr can be read.
struct Running<'a> {
    sb: &'a Sandbox,
    child: Child,
}

impl<'a> Running<'a> {
    fn start(sb: &'a Sandbox) -> Running<'a> {
        let child = sb
            .cmd(&["controller", "--tick-ms", "3600000"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the controller");
        let running = Running { sb, child };
        for _ in 0..400 {
            if pf_ipc::call(&sb.state, "runs", serde_json::json!({})).is_ok() {
                return running;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("the controller did not answer");
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        self.sb.cmd(args).output().expect("run pulse-factory")
    }

    fn list_json(&self) -> Value {
        let out = self.cli(&["project", "list", "--json"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        serde_json::from_str(&stdout(&out)).unwrap()
    }

    /// Stop the controller and give what it wrote to stderr.
    fn stop(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut text = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut text);
        }
        text
    }
}

#[test]
fn the_backfill_sets_what_it_detects_and_reports_what_it_cannot() {
    let w = world();
    let running = Running::start(&w.sb);

    // The ledger: one admitted line for each project that was detected, and one refused line for
    // each that was not. The removed project has none.
    let lines = ledger(&w.sb);
    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert!(
        lines[0].contains(
            "project_kind P-1 admitted kind=git remote_key=github.com/Owner/Repo command=backfill-kind:P-1"
        ),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains(
            "project_kind P-2 admitted kind=folder remote_key=none command=backfill-kind:P-2"
        ),
        "{}",
        lines[1]
    );
    for (line, id) in [(&lines[2], "P-3"), (&lines[3], "P-4")] {
        assert!(line.contains("project_kind refused reason="), "{line}");
        assert!(line.contains(&format!("project {id}:")), "{line}");
        assert!(
            line.ends_with(&format!("command=backfill-kind:{id}:refused")),
            "{line}"
        );
    }
    assert!(
        lines[2].contains("the root is a linked worktree"),
        "{}",
        lines[2]
    );
    assert!(
        lines.iter().all(|l| !l.contains("P-5")),
        "a removed project is never backfilled: {lines:#?}"
    );
    let all = lines.join("\n");
    assert!(
        !all.contains("tok") && !all.contains("https://"),
        "no URL in the ledger"
    );

    // `project list`: the kind and the key, the cause where there is none, and the text marker.
    let v = running.list_json();
    let listed = v["projects"].as_array().unwrap();
    assert_eq!(listed.len(), 4, "the removed project is not listed: {v}");
    assert_eq!(listed[0]["kind"], "git");
    assert_eq!(listed[0]["remote_key"], "github.com/Owner/Repo");
    assert_eq!(listed[0]["kind_error"], Value::Null);
    assert_eq!(listed[1]["kind"], "folder");
    assert_eq!(listed[1]["remote_key"], Value::Null);
    assert_eq!(listed[1]["kind_error"], Value::Null);
    assert_eq!(listed[2]["kind"], Value::Null);
    let cause = listed[2]["kind_error"].as_str().unwrap().to_string();
    assert!(
        cause.contains("the root is a linked worktree")
            && cause.contains(
                &w.linked
                    .parent()
                    .unwrap()
                    .join("other")
                    .display()
                    .to_string()
            ),
        "{cause}"
    );
    assert_eq!(listed[3]["kind"], Value::Null);
    let gone_cause = listed[3]["kind_error"].as_str().unwrap().to_string();
    assert!(
        gone_cause.contains("does not exist") && gone_cause.contains(&w.gone.display().to_string()),
        "{gone_cause}"
    );

    let out = running.cli(&["project", "list"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    let rows: Vec<&str> = text.lines().collect();
    assert_eq!(rows.len(), 4, "{text}");
    assert_eq!(rows[0], format!("P-1  main  {}", w.main.display()));
    assert_eq!(rows[1], format!("P-2  plain  {}", w.plain.display()));
    assert!(
        rows[2].ends_with(&format!("  (no kind: {cause})")),
        "{}",
        rows[2]
    );
    assert!(
        rows[3].ends_with(&format!("  (no kind: {gone_cause})")),
        "{}",
        rows[3]
    );

    // The unit's journal: one line for each project that has no kind, and none for any other.
    let journal = running.stop();
    let no_kind: Vec<&str> = journal
        .lines()
        .filter(|l| l.contains("has no kind"))
        .collect();
    assert_eq!(no_kind.len(), 2, "{journal}");
    assert!(
        no_kind[0].starts_with("pulse-factory: project P-3 has no kind: "),
        "{}",
        no_kind[0]
    );
    assert!(
        no_kind[1].starts_with("pulse-factory: project P-4 has no kind: "),
        "{}",
        no_kind[1]
    );
    assert!(!journal.contains("tok"), "{journal}");
}

#[test]
fn a_second_open_adds_no_line_and_leaves_the_removed_project_alone() {
    let w = world();
    let removed_before = {
        let c = open(&w.sb);
        project(&c, "P-5")
    };
    let after_first = ledger(&w.sb);
    assert_eq!(after_first.len(), 4, "{after_first:#?}");
    let admissions_first = {
        let c = open(&w.sb);
        c.store().admissions(CONTROLLER_SCOPE).unwrap().len()
    };
    // The second open (above) and a third add no line and no admission: the detected ones are set,
    // and each refusal is replayed.
    let c = open(&w.sb);
    assert_eq!(ledger(&w.sb), after_first, "a later open adds no line");
    assert_eq!(
        c.store().admissions(CONTROLLER_SCOPE).unwrap().len(),
        admissions_first,
        "a later open adds no admission"
    );
    assert_eq!(project(&c, "P-1").kind, Some(ProjectKind::Git));
    assert_eq!(project(&c, "P-2").kind, Some(ProjectKind::Folder));
    assert_eq!(project(&c, "P-3").kind, None);
    assert_eq!(project(&c, "P-4").kind, None);
    assert_eq!(
        project(&c, "P-5"),
        removed_before,
        "the removed project is untouched"
    );
    assert_eq!(project(&c, "P-5").kind, None);
    assert_eq!(project(&c, "P-5").remote_key, None);
    assert!(
        w.removed.is_dir(),
        "the test kept the removed project's root"
    );
}

#[test]
fn a_remove_of_a_project_with_no_kind_answers_a_null_kind_error() {
    // The contract (`docs/contracts/cli.md`, JSON): `list` gives the cause, `add` and `remove` do not.
    let w = world();
    let running = Running::start(&w.sb);
    let listed = running.list_json();
    assert!(
        listed["projects"][2]["kind_error"].is_string(),
        "list gives the cause of P-3: {listed}"
    );
    let out = running.cli(&["project", "remove", "P-3", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["project"]["id"], "P-3", "{v}");
    assert_eq!(v["project"]["kind"], Value::Null, "{v}");
    assert_eq!(v["project"]["kind_error"], Value::Null, "{v}");
    assert!(v["project"]["removed_at"].is_string(), "{v}");
    running.stop();
}

#[test]
fn a_chat_session_start_on_a_project_with_no_kind_refuses_with_the_cause() {
    let w = world();
    let c = open(&w.sb);
    let lines = ledger(&w.sb).len();
    let args = ChatSessionStartArgs {
        project_id: "P-3".to_string(),
        agent_id: "orchestrator".to_string(),
        command_id: "start-1".to_string(),
    };
    let why = c.start_chat_session(&args).unwrap_err();
    assert!(why.starts_with("project P-3 has no kind ("), "{why}");
    assert!(why.contains("the root is a linked worktree"), "{why}");
    assert!(why.ends_with("); remove it and add it again"), "{why}");
    // A refusal is one admission and one line, and it starts nothing.
    let after = ledger(&w.sb);
    assert_eq!(after.len(), lines + 1, "{after:#?}");
    assert!(
        after
            .last()
            .unwrap()
            .contains("chat_session_start refused reason="),
        "{after:#?}"
    );
    assert!(c.store().live_chat_sessions().unwrap().is_empty());
    assert!(c.store().worktrees("P-3").unwrap().is_empty());

    // A project whose kind the backfill set starts as usual.
    let ok = ChatSessionStartArgs {
        project_id: "P-2".to_string(),
        agent_id: "orchestrator".to_string(),
        command_id: "start-2".to_string(),
    };
    assert!(c.start_chat_session(&ok).is_ok());
}

#[test]
fn a_cause_the_operator_fixes_gives_the_kind_at_the_next_open_with_one_admitted_line() {
    let w = world();
    drop(open(&w.sb));
    let before = ledger(&w.sb);
    assert_eq!(before.len(), 4, "{before:#?}");

    // The linked worktree is replaced by a plain folder at the same path.
    let other = w.linked.parent().unwrap().join("other");
    git(
        &other,
        &["worktree", "remove", "--force", w.linked.to_str().unwrap()],
    );
    std::fs::create_dir_all(&w.linked).unwrap();

    let c = open(&w.sb);
    assert_eq!(project(&c, "P-3").kind, Some(ProjectKind::Folder));
    assert_eq!(project(&c, "P-3").remote_key, None);
    let after = ledger(&w.sb);
    assert_eq!(after.len(), before.len() + 1, "{after:#?}");
    assert!(
        after.last().unwrap().contains(
            "project_kind P-3 admitted kind=folder remote_key=none command=backfill-kind:P-3"
        ),
        "{}",
        after.last().unwrap()
    );
    // The earlier refusal stays in the ledger as it was.
    assert_eq!(after[..before.len()], before[..]);
    // The project that is still gone stays without a kind, and writes nothing.
    assert_eq!(project(&c, "P-4").kind, None);
    drop(c);
    assert_eq!(ledger(&w.sb), after, "the next open adds no line");
}

#[test]
fn an_unwritable_controller_ledger_skips_the_backfill_and_says_why() {
    let w = world();
    // A directory takes the ledger's place, so the controller ledger cannot start.
    std::fs::create_dir(w.sb.state.join("controller.ledger")).unwrap();
    let running = Running::start(&w.sb);
    let v = running.list_json();
    let journal = running.stop();
    assert!(
        journal.contains("the project kind backfill is skipped"),
        "{journal}"
    );
    assert!(
        journal.contains("controller commands are stopped"),
        "{journal}"
    );
    for p in v["projects"].as_array().unwrap() {
        assert_eq!(p["kind"], Value::Null, "nothing was backfilled: {p}");
    }
    let c = open(&w.sb);
    assert!(
        c.store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
            .iter()
            .all(|a| a.kind != "project_kind"),
        "no project_kind admission"
    );
    for id in ["P-1", "P-2", "P-3", "P-4"] {
        assert_eq!(project(&c, id).kind, None, "{id}");
    }
}
