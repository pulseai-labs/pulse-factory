//! AC-18 of `r2.s3.w1`: an operator can end a project's clear grant in the state where nothing else
//! can be recorded. With the controller's commands stopped (its ledger file cannot be appended, as
//! `api_pairing.rs::a_stopped_controller_ledger_still_revokes_a_device_at_once` makes it), `grant
//! revoke` commits to the store at once and its ledger line waits in the outbox for the next start.
//! `grant clear` is refused there with the stopped reason and writes no row.
//!
//! The seam of the store's precondition: after that revoke, and before the next start, a commit that
//! carries `RequireLiveGrant` for the revoked generation is `Rejected` and writes nothing. The test
//! commits it through its own handle on the same store file. The controller is a real process.

mod common;

use std::path::Path;

use common::*;
use pf_engine::port::store::{
    AdmissionOutcome, Commit, Committed, Effect, LedgerEntry, NewAdmission, RunRow, RunState, Store,
};
use pf_store_sqlite::SqliteStore;
use serde_json::Value;

const LEDGER: &str = "controller.ledger";

fn json_of(out: &std::process::Output) -> Value {
    assert_eq!(code(out), 0, "{}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

fn cli(sb: &Sandbox, args: &[&str]) -> std::process::Output {
    sb.cmd(args).output().expect("run pulse-factory")
}

fn grants(sb: &Sandbox) -> Vec<Value> {
    json_of(&cli(sb, &["grant", "list", "--json"]))["grants"]
        .as_array()
        .unwrap()
        .clone()
}

fn ledger_lines(state: &Path) -> Vec<String> {
    std::fs::read_to_string(state.join(LEDGER))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A commit in the scope of run `run-1` that writes the run row and carries the precondition for
/// `project_id` at `generation`, as a settle will commit it.
fn guarded_commit(
    ledger_name: &str,
    command_id: &str,
    project_id: &str,
    generation: u64,
) -> Commit {
    Commit {
        run_id: "run-1".to_string(),
        command_id: command_id.to_string(),
        content: format!("{{\"project_id\":\"{project_id}\",\"generation\":{generation}}}"),
        admission: NewAdmission {
            kind: "settle_probe".to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t7".to_string(),
        },
        effects: vec![
            Effect::PutRun(RunRow {
                id: "run-1".to_string(),
                workflow: "wf".to_string(),
                ledger_file: ledger_name.to_string(),
                state: RunState::Active,
                halted_reason: None,
                project_id: None,
                created_at: "t7".to_string(),
                updated_at: "t7".to_string(),
            }),
            Effect::RequireLiveGrant {
                project_id: project_id.to_string(),
                generation,
            },
        ],
        ledger: Some(LedgerEntry {
            ledger_file: ledger_name.to_string(),
            body: format!("settle_probe {project_id} command={command_id}"),
        }),
    }
}

#[test]
fn a_revoke_commits_while_the_controller_is_stopped_and_its_line_follows_at_the_next_start() {
    let sb = Sandbox::new();
    let ctl = Ctl::start(&sb, 3_600_000);
    let alpha = sb.root.path().join("work/alpha");
    std::fs::create_dir_all(&alpha).unwrap();
    let out = cli(&sb, &["project", "add", &alpha.display().to_string()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // The grant is given while the ledger is healthy.
    let out = cli(
        &sb,
        &[
            "grant",
            "clear",
            "--project",
            "P-1",
            "--command-id",
            "g-clear-1",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "clear grant given: P-1 orchestrator generation 1\n"
    );
    let rows = grants(&sb);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["live"], true, "{rows:?}");

    // Stop the controller's commands: a directory is in the ledger file's place, a project add
    // commits and its append fails, and the next command is refused as stopped.
    let ledger = sb.state.join(LEDGER);
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let beta = sb.root.path().join("work/beta");
    std::fs::create_dir_all(&beta).unwrap();
    let out = cli(&sb, &["project", "add", &beta.display().to_string()]);
    assert_eq!(code(&out), 1, "{}", stdout(&out));
    assert!(stderr(&out).contains("committed"), "{}", stderr(&out));
    let again = cli(&sb, &["project", "remove", "P-2"]);
    assert!(stderr(&again).contains("stopped"), "{}", stderr(&again));

    // The revoke commits all the same.
    let out = cli(
        &sb,
        &[
            "grant",
            "revoke",
            "--project",
            "P-1",
            "--command-id",
            "g-revoke-1",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "clear grant ended: P-1 generation 1\n");
    let rows = grants(&sb);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["live"], false, "{rows:?}");
    assert_eq!(rows[0]["revoked_by"], "cli", "{rows:?}");
    // The listing shows no live grant, and `requests` holds no `grants` key.
    let list = json_of(&cli(&sb, &["requests", "--json"]));
    assert!(list.get("grants").is_none(), "{list}");

    // A new grant is refused there with the stopped reason, and writes no row.
    let out = cli(
        &sb,
        &[
            "grant",
            "clear",
            "--project",
            "P-1",
            "--command-id",
            "g-clear-2",
        ],
    );
    assert_ne!(
        code(&out),
        0,
        "a grant was given while the commands were stopped"
    );
    assert!(stderr(&out).contains("stopped"), "{}", stderr(&out));
    assert_eq!(grants(&sb).len(), 1, "a row was written");

    // The seam: a commit that carries the precondition for the revoked generation is rejected
    // through a second handle on the same store file, and writes nothing.
    // The store names the ledger file by its path.
    let name = ledger.display().to_string();
    let store = SqliteStore::open(sb.state.join("store.sqlite")).unwrap();
    let waiting_before = store.unwritten_ledger_lines(&name).unwrap();
    assert!(
        waiting_before.iter().any(|l| l
            .body
            .contains("grant_revoke P-1 generation=1 by=cli command=g-revoke-1")),
        "the revoke's line does not wait in the outbox: {waiting_before:?}"
    );
    match store
        .commit(guarded_commit(&name, "probe-1", "P-1", 1))
        .unwrap()
    {
        Committed::Rejected(why) => {
            assert!(why.contains("holds no live clear grant"), "{why}");
        }
        other => panic!("the commit that needs the revoked grant was not rejected: {other:?}"),
    }
    assert!(store.admissions("run-1").unwrap().is_empty());
    assert!(store.run("run-1").unwrap().is_none());
    assert_eq!(store.unwritten_ledger_lines(&name).unwrap(), waiting_before);
    // A generation that is not there yet is rejected too: no row is written for it either.
    assert!(matches!(
        store
            .commit(guarded_commit(&name, "probe-2", "P-1", 2))
            .unwrap(),
        Committed::Rejected(_)
    ));
    assert!(store.admissions("run-1").unwrap().is_empty());
    drop(store);

    // The next start, with a good file, appends the revoke's line once.
    std::fs::remove_dir(&ledger).unwrap();
    drop(ctl);
    let _ctl = Ctl::start(&sb, 3_600_000);
    let lines = ledger_lines(&sb.state);
    let revokes: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("grant_revoke"))
        .collect();
    assert_eq!(revokes.len(), 1, "{lines:#?}");
    assert!(
        revokes[0].contains("grant_revoke P-1 generation=1 by=cli command=g-revoke-1"),
        "{}",
        revokes[0]
    );
    assert!(
        !lines.iter().any(|l| l.contains("g-clear-2")),
        "a refused grant has a line: {lines:#?}"
    );
    // The row is still ended, and a restart did not give a grant back.
    let rows = grants(&sb);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["live"], false, "{rows:?}");
}
