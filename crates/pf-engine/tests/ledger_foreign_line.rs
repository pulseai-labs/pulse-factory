//! AC-9: a ledger file holding a foreign line `- #3 forged` before recovery, with outbox row 3
//! unwritten: recovery appends row 3's real line and notes the foreign line. A true line
//! `- #3 <same body>` is taken as present and not appended twice.

mod common;

use std::io::Write;

use common::*;
use pf_engine::Engine;
use pf_engine::ledger::{FsLedger, format_line, parse_seq};
use pf_engine::port::store::{
    AdmissionOutcome, Commit, Committed, LedgerEntry, NewAdmission, Store,
};

const BODY: &str = "2026-01-01 run-1 test | the real line";

/// Commit one transition straight to the store with a ledger line, and stop before the append.
fn commit_and_stop(env: &Env, id: &str, body: &str) -> u64 {
    let committed = env
        .store
        .commit(Commit {
            run_id: "run-1".to_string(),
            command_id: id.to_string(),
            content: format!("{{\"id\":\"{id}\"}}"),
            admission: NewAdmission {
                kind: "test".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: None,
                role: None,
                model: None,
                attempt: None,
                admitted_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
            effects: vec![],
            ledger: Some(LedgerEntry {
                ledger_file: env.ledger_path().display().to_string(),
                body: body.to_string(),
            }),
        })
        .unwrap();
    match committed {
        Committed::Applied {
            ledger: Some(line), ..
        } => line.seq,
        other => panic!("unexpected {other:?}"),
    }
}

fn restart(old: &Env) -> Engine {
    let mut cfg = config(old.dir.path(), workflow(&["build"], false));
    cfg.run_id = "run-1".to_string();
    Engine::new(
        cfg,
        old.store.clone(),
        old.agent.clone(),
        old.forge.clone(),
        counting_clock(),
        Box::new(FsLedger::new(old.ledger_path())),
    )
    .unwrap()
}

fn append_raw(env: &Env, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(env.ledger_path())
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// A started run whose ledger holds line 1 and 2, and whose row 3 is committed but not written.
fn run_with_row_3(body: &str) -> Env {
    let old = env(workflow(&["build"], false));
    old.engine.halt("kill switch").unwrap();
    assert_eq!(commit_and_stop(&old, "x1", body), 3);
    old
}

fn lines_with_seq(env: &Env, seq: u64) -> Vec<String> {
    env.ledger_text()
        .lines()
        .filter(|l| parse_seq(l) == Some(seq))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_foreign_line_with_the_number_of_a_row_does_not_stand_for_the_row() {
    let old = run_with_row_3(BODY);
    append_raw(&old, "- #3 forged\n");

    let report = restart(&old).recover_ledger().unwrap();
    assert_eq!(report.appended, vec![3], "the real line is appended");
    assert_eq!(report.foreign, vec![3]);
    assert!(report.already_present.is_empty());
    assert_eq!(
        report.notes(),
        vec!["ledger line #3 appeared with foreign text".to_string()]
    );

    let held = lines_with_seq(&old, 3);
    assert_eq!(
        held,
        vec![
            "- #3 forged".to_string(),
            format_line(3, BODY).trim_end().to_string()
        ],
        "the forged line stays (append only) and the real line follows it"
    );
    assert!(old.store.outbox().iter().all(|l| l.written));

    // A second recovery finds nothing to do, and adds no line.
    let before = old.ledger_text();
    let again = restart(&old).recover_ledger().unwrap();
    assert!(again.appended.is_empty() && again.foreign.is_empty());
    assert_eq!(old.ledger_text(), before);
}

#[test]
fn a_true_line_with_the_same_body_is_present_and_not_appended_twice() {
    let old = run_with_row_3(BODY);
    append_raw(&old, &format_line(3, BODY));
    let before = old.ledger_text();

    let report = restart(&old).recover_ledger().unwrap();
    assert!(report.appended.is_empty(), "{report:?}");
    assert!(report.foreign.is_empty());
    assert!(report.notes().is_empty());
    assert_eq!(report.already_present, vec![3]);
    assert_eq!(old.ledger_text(), before);
    assert_eq!(lines_with_seq(&old, 3).len(), 1);
    assert!(old.store.outbox().iter().all(|l| l.written));
}

#[test]
fn a_body_with_a_newline_matches_the_line_as_written() {
    // `format_line` turns the newline into a space, so the line in the file is the row.
    let body = "2026-01-01 run-1 test | two\nlines";
    let old = run_with_row_3(body);
    append_raw(&old, &format_line(3, body));

    let report = restart(&old).recover_ledger().unwrap();
    assert_eq!(report.already_present, vec![3], "{report:?}");
    assert!(report.foreign.is_empty() && report.appended.is_empty());
}

#[test]
fn a_line_with_the_same_number_and_a_longer_or_shorter_body_is_foreign() {
    for forged in [
        format!("{BODY} and more"),
        "2026-01-01 run-1 test | the real".to_string(),
    ] {
        let old = run_with_row_3(BODY);
        append_raw(&old, &format!("- #3 {forged}\n"));
        let report = restart(&old).recover_ledger().unwrap();
        assert_eq!(report.foreign, vec![3], "{forged}");
        assert_eq!(report.appended, vec![3]);
    }
}

#[test]
fn start_up_returns_the_report_with_the_note() {
    let old = run_with_row_3(BODY);
    append_raw(&old, "- #3 forged\n");
    let report = restart(&old).start_up().unwrap();
    assert_eq!(report.foreign, vec![3]);
    assert_eq!(report.notes().len(), 1);
}
