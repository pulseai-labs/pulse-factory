//! AC-11: a simulated stop after the commit and before the append: recovery adds the line once,
//! in order. A torn last line is truncated, and its outbox row is appended whole. A simulated
//! stop after the append and before the mark: recovery adds no second line. A failed commit
//! leaves no line in the file. A failed append stops admission. A second run on the same file
//! continues the numbers, and recovery still finds its unwritten lines.

mod common;

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::*;
use pf_engine::ledger::{FsLedger, LedgerFile, format_line, parse_seq};
use pf_engine::port::store::{
    AdmissionOutcome, Commit, Committed, LedgerEntry, NewAdmission, Store,
};
use pf_engine::{Engine, EngineError};

/// A ledger file whose append can be made to fail.
struct FlakyLedger {
    inner: FsLedger,
    fail: Arc<AtomicBool>,
}

impl LedgerFile for FlakyLedger {
    fn truncate_torn_tail(&mut self) -> std::io::Result<bool> {
        self.inner.truncate_torn_tail()
    }
    fn seqs(&mut self) -> std::io::Result<BTreeSet<u64>> {
        self.inner.seqs()
    }
    fn append(&mut self, line: &str) -> std::io::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("disk full"));
        }
        self.inner.append(line)
    }
}

/// Commit one transition straight to the store, with a ledger line, and stop before the append.
/// This is what a controller leaves behind when it dies right after the commit.
fn commit_and_stop(env: &Env, run: &str, id: &str, body: &str) -> u64 {
    let ledger_file = env.ledger_path().display().to_string();
    let committed = env
        .store
        .commit(Commit {
            run_id: run.to_string(),
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
                ledger_file,
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

fn restart(old: &Env, run_id: &str) -> Engine {
    let mut cfg = config(old.dir.path(), workflow(&["build"], false));
    cfg.run_id = run_id.to_string();
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

fn seqs_in(env: &Env) -> Vec<u64> {
    env.ledger_text().lines().filter_map(parse_seq).collect()
}

fn all_written(env: &Env) -> bool {
    env.store.outbox().iter().all(|l| l.written)
}

#[test]
fn a_stop_after_the_commit_and_before_the_append_loses_no_line() {
    let old = env(workflow(&["build"], false));
    assert_eq!(seqs_in(&old), vec![1]); // the line of the opened run
    let s2 = commit_and_stop(
        &old,
        "run-1",
        "x1",
        "2026-01-01 run-1 test | first lost line",
    );
    let s3 = commit_and_stop(
        &old,
        "run-1",
        "x2",
        "2026-01-01 run-1 test | second lost line",
    );
    assert_eq!((s2, s3), (2, 3));
    assert_eq!(seqs_in(&old), vec![1], "the lines are not in the file yet");

    let second = restart(&old, "run-1");
    let report = second.recover_ledger().unwrap();
    assert_eq!(report.appended, vec![2, 3]);
    assert!(!report.truncated_torn_tail);
    assert_eq!(seqs_in(&old), vec![1, 2, 3], "added once, in order");
    assert!(old.ledger_text().contains("first lost line"));
    assert!(all_written(&old));

    // Recovering again, or starting up again, adds nothing.
    let before = old.ledger_text();
    let again = second.recover_ledger().unwrap();
    assert!(again.appended.is_empty());
    second.start_up().unwrap();
    restart(&old, "run-1").start_up().unwrap();
    assert_eq!(old.ledger_text(), before);
}

#[test]
fn a_torn_last_line_is_truncated_and_its_outbox_row_is_appended_whole() {
    let old = env(workflow(&["build"], false));
    let seq = commit_and_stop(
        &old,
        "run-1",
        "x1",
        "2026-01-01 run-1 test | a line that tears",
    );
    // The controller died in the middle of the append: half a line, no newline.
    let whole = format_line(seq, "2026-01-01 run-1 test | a line that tears");
    let torn = &whole[..whole.len() / 2];
    assert!(!torn.ends_with('\n'));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(old.ledger_path())
        .unwrap();
    file.write_all(torn.as_bytes()).unwrap();
    drop(file);
    let first_line = old.ledger_text().lines().next().unwrap().to_string();

    let report = restart(&old, "run-1").recover_ledger().unwrap();
    assert!(report.truncated_torn_tail);
    assert_eq!(report.appended, vec![seq]);

    let text = old.ledger_text();
    assert!(text.ends_with('\n'));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!(lines[0], first_line);
    assert_eq!(
        format!("{}\n", lines[1]),
        whole,
        "the row is appended whole"
    );
    assert!(all_written(&old));
}

#[test]
fn a_torn_tail_with_no_row_behind_it_is_cut_and_nothing_is_appended() {
    let old = env(workflow(&["build"], false));
    let good = old.ledger_text();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(old.ledger_path())
        .unwrap();
    file.write_all(b"- #9 2026-01-01 run-1 garb").unwrap();
    drop(file);
    let report = restart(&old, "run-1").recover_ledger().unwrap();
    assert!(report.truncated_torn_tail);
    assert!(report.appended.is_empty());
    assert_eq!(old.ledger_text(), good);
}

#[test]
fn a_stop_after_the_append_and_before_the_mark_adds_no_second_line() {
    let old = env(workflow(&["build"], false));
    old.engine.halt("kill switch").unwrap();
    assert_eq!(seqs_in(&old), vec![1, 2]);
    let before = std::fs::read(old.ledger_path()).unwrap();

    // The append reached the disk. The mark did not.
    old.store.unmark(2);
    assert!(!all_written(&old));

    let report = restart(&old, "run-1").recover_ledger().unwrap();
    assert!(report.appended.is_empty());
    assert_eq!(report.already_present, vec![2]);
    assert_eq!(std::fs::read(old.ledger_path()).unwrap(), before);
    assert!(
        all_written(&old),
        "recovery marks the row it found in the file"
    );
}

#[test]
fn a_failed_commit_leaves_no_line_in_the_file() {
    let old = env(workflow(&["build"], false));
    old.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    let file_before = std::fs::read(old.ledger_path()).unwrap();
    let outbox_before = old.store.outbox();

    old.store.fail_writes.store(true, Ordering::SeqCst);
    let err = old.engine.park("u1", "stalled").unwrap_err();
    assert!(matches!(err, EngineError::Degraded(_)), "{err:?}");
    assert_eq!(std::fs::read(old.ledger_path()).unwrap(), file_before);
    assert_eq!(old.store.outbox(), outbox_before);
    assert_eq!(old.status_of("u1").0, pf_engine::Status::Running);
}

#[test]
fn a_failed_append_stops_admission_and_a_restart_recovers_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    let fail = Arc::new(AtomicBool::new(false));
    let path = dir.path().join("ledger.md");
    let engine = Engine::new(
        config(dir.path(), workflow(&["build"], false)),
        store.clone(),
        FakeAgent::new(),
        FakeForge::new(),
        counting_clock(),
        Box::new(FlakyLedger {
            inner: FsLedger::new(&path),
            fail: fail.clone(),
        }),
    )
    .unwrap();
    engine.start_up().unwrap();
    assert_eq!(engine.degraded(), None);

    fail.store(true, Ordering::SeqCst);
    let err = engine.halt("kill switch").unwrap_err();
    let EngineError::Degraded(why) = err else {
        panic!("expected the engine to stop admitting work");
    };
    assert!(why.contains("ledger append failed: disk full"), "{why}");
    assert_eq!(engine.degraded(), Some(why.clone()));

    // Admission is stopped, even after the disk works again, and nothing new reaches the store.
    fail.store(false, Ordering::SeqCst);
    let admissions = store.admissions("run-1").unwrap().len();
    let err = engine.end_run("done").unwrap_err();
    assert!(matches!(err, EngineError::Degraded(_)));
    assert_eq!(store.admissions("run-1").unwrap().len(), admissions);
    let health = std::fs::read_to_string(engine.health_path()).unwrap();
    assert!(health.contains("ledger append failed"), "{health}");

    // The halt was committed, and its line is in the outbox. A restart appends it.
    let unwritten = store
        .unwritten_ledger_lines(&path.display().to_string())
        .unwrap();
    assert_eq!(unwritten.len(), 1);
    let restarted = Engine::new(
        config(dir.path(), workflow(&["build"], false)),
        store.clone(),
        FakeAgent::new(),
        FakeForge::new(),
        counting_clock(),
        Box::new(FsLedger::new(&path)),
    )
    .unwrap();
    restarted.start_up().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("run halted: kill switch"), "{text}");
    assert_eq!(
        text.lines().filter_map(parse_seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn a_second_run_on_the_same_file_continues_the_numbers_and_recovers_the_first_runs_lines() {
    let old = env(workflow(&["build"], false));
    old.engine.halt("kill switch").unwrap();
    assert_eq!(seqs_in(&old), vec![1, 2]);
    // The first run's controller dies with one more committed line unwritten.
    let lost = commit_and_stop(
        &old,
        "run-1",
        "x1",
        "2026-01-01 run-1 test | lost with the first run",
    );
    assert_eq!(lost, 3);

    // A second run starts on the same store and the same ledger file.
    let second = restart(&old, "run-2");
    second.start_up().unwrap();
    let text = old.ledger_text();
    let seqs: Vec<u64> = text.lines().filter_map(parse_seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4], "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[2].contains("lost with the first run"), "{}", lines[2]);
    assert!(lines[3].contains(" run-2 open_run "), "{}", lines[3]);

    second.halt("second kill switch").unwrap();
    assert_eq!(seqs_in(&old), vec![1, 2, 3, 4, 5]);
    assert!(all_written(&old));
}
