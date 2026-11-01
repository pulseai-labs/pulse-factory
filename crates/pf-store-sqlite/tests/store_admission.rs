//! AC-16 (the `run-state-admission` property test, on the real store): for generated commands,
//! the same command ID and content returns the same record and no launch is duplicated; changed
//! content is a conflict; concurrent submissions of one command from several threads admit it
//! once; the transition, its command and its outbox line commit together or not at all.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pf_engine::capacity::Settings;
use pf_engine::port::agent::*;
use pf_engine::port::forge::*;
use pf_engine::port::store::*;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::{
    Command, CommandBody, Engine, EngineConfig, EngineError, MergeRule, StageDef, Status, Workflow,
};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;
use rusqlite::Connection;

// ------------------------------------------------------------------ fake ports

#[derive(Default)]
struct OkAgent(AtomicU64);

impl AgentPort for OkAgent {
    fn launch(&self, _: &LaunchPlan) -> Result<SessionRef, AgentError> {
        Ok(SessionRef(format!(
            "p{}",
            self.0.fetch_add(1, Ordering::SeqCst)
        )))
    }
    fn status(&self, _: &SessionRef) -> Result<SessionStatus, AgentError> {
        Ok(SessionStatus {
            state: SessionState::Working,
            turn_ended: false,
            first_turn_started: true,
        })
    }
    fn deliver(&self, _: &SessionRef, _: &str) -> Result<(), AgentError> {
        Ok(())
    }
    fn interrupt(&self, _: &SessionRef) -> Result<(), AgentError> {
        Ok(())
    }
    fn usage(&self, _: &SessionRef) -> Result<Usage, AgentError> {
        Ok(Usage::default())
    }
    fn answer_dialog(&self, _: &SessionRef, _: &Dialog) -> Result<DialogAnswer, AgentError> {
        Ok(DialogAnswer::Unknown)
    }
    fn apply_rules(&self, _: &mut LaunchPlan, _: &RuleSet) -> Result<(), AgentError> {
        Ok(())
    }
    fn remove_ask_tool(&self, _: &mut LaunchPlan) -> Result<(), AgentError> {
        Ok(())
    }
    fn hand_off(&self, _: &SessionRef, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.launch(p)
    }
    fn is_alive(&self, _: &SessionRef) -> Result<bool, AgentError> {
        Ok(true)
    }
    fn resolve_plugin(&self, name: &str, _: &str) -> Result<ResolvedPlugin, AgentError> {
        Err(AgentError::Refused(format!("no plugin {name}")))
    }
    fn drain_events(&self, _: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        Ok(Vec::new())
    }
}

struct NoForge;

impl ForgePort for NoForge {
    fn pr_status(&self, _: &PrRef) -> Result<PrStatus, ForgeError> {
        Err(ForgeError("no forge".into()))
    }
    fn merge(&self, _: &PrRef) -> Result<ForgeEffect, ForgeError> {
        Err(ForgeError("no forge".into()))
    }
    fn merge_landed(&self, _: &PrRef) -> Result<ForgeEffect, ForgeError> {
        Err(ForgeError("no forge".into()))
    }
}

struct Records(Vec<UnitRecord>);

impl UnitSource for Records {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        Ok(self.0.clone())
    }
}

// ------------------------------------------------------------------ helpers

fn workflow() -> Workflow {
    Workflow::single_track(
        "test-flow",
        vec![StageDef::new("build", "build-role", "model-a")],
        Some(MergeRule::GreenChecksInDeclaredOrder),
        Settings::default(),
    )
}

fn config(dir: &Path, run_id: &str) -> EngineConfig {
    EngineConfig {
        run_id: run_id.to_string(),
        workflow: workflow(),
        ledger_path: dir.join("ledger.md"),
        state_dir: dir.join("state"),
        run_dir: dir.join("run"),
        params: Default::default(),
    }
}

fn start(dir: &Path, store: Arc<SqliteStore>, run_id: &str) -> Engine {
    Engine::start(
        config(dir, run_id),
        store,
        Arc::new(OkAgent::default()),
        Arc::new(NoForge),
    )
    .unwrap()
}

fn two_units() -> Records {
    Records(vec![
        UnitRecord::new("u1", "g1", 1, Default::default()),
        UnitRecord::new("u2", "g1", 2, Default::default()),
    ])
}

fn park(id: &str, unit: &str, reason: &str) -> Command {
    Command {
        id: id.to_string(),
        body: CommandBody::Park {
            unit_id: unit.to_string(),
            reason: reason.to_string(),
        },
    }
}

fn launch(id: &str, unit: &str, attempt: u32) -> Command {
    Command {
        id: id.to_string(),
        body: CommandBody::Launch {
            unit_id: unit.to_string(),
            stage: "build".to_string(),
            role: "build-role".to_string(),
            model: "model-a".to_string(),
            attempt,
        },
    }
}

fn count(path: &Path, sql: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

// ------------------------------------------------------------------ the property test

#[derive(Clone, Debug)]
enum Gen {
    Park(usize, usize),
    Halt(usize),
    Launch(usize),
}

fn arb_op() -> impl Strategy<Value = (usize, Gen)> {
    (
        0..5usize,
        prop_oneof![
            (0..2usize, 0..3usize).prop_map(|(u, r)| Gen::Park(u, r)),
            (0..3usize).prop_map(Gen::Halt),
            (0..2usize).prop_map(Gen::Launch),
        ],
    )
}

fn build(id: usize, op: &Gen) -> Command {
    let id = format!("c{id}");
    match op {
        Gen::Park(u, r) => park(&id, &format!("u{}", u + 1), &format!("reason {r}")),
        Gen::Halt(r) => Command {
            id,
            body: CommandBody::Halt {
                reason: format!("halt {r}"),
            },
        },
        Gen::Launch(u) => launch(&id, &format!("u{}", u + 1), 1),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn generated_commands_keep_the_admission_rules_on_the_real_store(
        ops in proptest::collection::vec(arb_op(), 1..12)
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
        let engine = start(dir.path(), store.clone(), "run-1");
        engine.ingest(&mut two_units()).unwrap();

        let mut seen: HashMap<String, (String, AdmissionRow)> = HashMap::new();
        for (id, op) in &ops {
            let command = build(*id, op);
            let body = serde_json::to_string(&command.body).unwrap();
            let result = engine.submit(command.clone());
            match seen.get(&command.id) {
                Some((first_body, first_row)) if *first_body == body => {
                    let again = result.expect("a repeat of an admitted command succeeds");
                    prop_assert!(again.replayed);
                    prop_assert_eq!(&again.row, first_row);
                }
                Some(_) => prop_assert_eq!(
                    result.unwrap_err(),
                    EngineError::Conflict { command_id: command.id.clone() }
                ),
                None => match result {
                    Ok(adm) => {
                        prop_assert!(!adm.replayed);
                        seen.insert(command.id.clone(), (body, adm.row));
                    }
                    Err(EngineError::Rejected(_)) => {}
                    Err(other) => prop_assert!(false, "unexpected error {:?}", other),
                },
            }
        }
        let admissions = store.admissions("run-1").unwrap();
        let mut ids: Vec<&str> = admissions.iter().map(|a| a.command_id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        prop_assert_eq!(before, ids.len());
        for u in ["u1", "u2"] {
            let launched = admissions.iter().filter(|a| a.kind == "launch"
                && a.outcome == AdmissionOutcome::Admitted
                && a.unit_id.as_deref() == Some(u)).count();
            prop_assert!(launched <= 1, "unit {} launched {} times", u, launched);
        }
        // Every transition that wrote a ledger line has its line in the file and its row marked.
        let unwritten = store.unwritten_ledger_lines(&dir.path().join("ledger.md").display().to_string()).unwrap();
        prop_assert!(unwritten.is_empty());
    }
}

// ------------------------------------------------------------------ the rules, one by one

#[test]
fn the_same_id_and_content_returns_the_same_record_and_changed_content_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let engine = start(dir.path(), store.clone(), "run-1");
    engine.ingest(&mut two_units()).unwrap();

    let first = engine.submit(launch("l1", "u1", 1)).unwrap();
    let again = engine.submit(launch("l1", "u1", 1)).unwrap();
    assert!(again.replayed);
    assert_eq!(first.row, again.row);
    assert_eq!(first.row.unit_id.as_deref(), Some("u1"));
    assert_eq!(first.row.role.as_deref(), Some("build-role"));
    assert_eq!(first.row.model.as_deref(), Some("model-a"));
    assert_eq!(first.row.attempt, Some(1));
    // The stored row is the receipt.
    assert_eq!(store.admission("run-1", "l1").unwrap().unwrap(), first.row);

    let err = engine.submit(launch("l1", "u2", 1)).unwrap_err();
    assert_eq!(
        err,
        EngineError::Conflict {
            command_id: "l1".to_string()
        }
    );
    let launches = store
        .admissions("run-1")
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "launch")
        .count();
    assert_eq!(launches, 1);
}

#[test]
fn concurrent_submissions_of_one_command_through_the_engine_admit_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let engine = Arc::new(start(dir.path(), store.clone(), "run-1"));
    engine.ingest(&mut two_units()).unwrap();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let engine = engine.clone();
            std::thread::spawn(move || engine.submit(launch("same", "u1", 1)).unwrap())
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|a| !a.replayed).count(),
        1,
        "admitted once"
    );
    assert!(
        results.windows(2).all(|w| w[0].row == w[1].row),
        "one record for all"
    );
    let launches = store
        .admissions("run-1")
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "launch")
        .count();
    assert_eq!(launches, 1);
    assert_eq!(store.sessions("run-1").unwrap().len(), 1);
}

#[test]
fn concurrent_commits_of_one_command_to_the_store_apply_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let commit = |reason: &str| Commit {
        run_id: "run-1".to_string(),
        command_id: "same".to_string(),
        content: format!("{{\"reason\":\"{reason}\"}}"),
        admission: NewAdmission {
            kind: "test".to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t".to_string(),
        },
        effects: vec![],
        ledger: Some(LedgerEntry {
            ledger_file: "ledger.md".to_string(),
            body: "one line".to_string(),
        }),
    };

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            let c = commit("a");
            std::thread::spawn(move || store.commit(c).unwrap())
        })
        .collect();
    let results: Vec<Committed> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let applied = results
        .iter()
        .filter(|r| matches!(r, Committed::Applied { .. }))
        .count();
    let replayed = results
        .iter()
        .filter(|r| matches!(r, Committed::Replayed(_)))
        .count();
    assert_eq!((applied, replayed), (1, 7));
    assert_eq!(store.admissions("run-1").unwrap().len(), 1);
    assert_eq!(
        store.unwritten_ledger_lines("ledger.md").unwrap().len(),
        1,
        "one outbox line"
    );

    // Other content under the used ID is a conflict, and it writes nothing.
    let conflict = store.commit(commit("b")).unwrap();
    assert!(matches!(conflict, Committed::Conflict(_)));
    assert_eq!(store.admissions("run-1").unwrap().len(), 1);
    assert_eq!(store.unwritten_ledger_lines("ledger.md").unwrap().len(), 1);
}

#[test]
fn the_transition_its_command_and_its_outbox_line_commit_together_or_not_at_all() {
    // Each trigger makes one of the three writes fail, so the commit fails in the middle.
    let triggers = [
        (
            "outbox",
            "CREATE TRIGGER t BEFORE INSERT ON ledger_outbox BEGIN SELECT RAISE(ABORT, 'forced outbox failure'); END;",
        ),
        (
            "node",
            "CREATE TRIGGER t BEFORE UPDATE ON nodes BEGIN SELECT RAISE(ABORT, 'forced node failure'); END;",
        ),
        (
            "admission",
            "CREATE TRIGGER t BEFORE INSERT ON admissions BEGIN SELECT RAISE(ABORT, 'forced admission failure'); END;",
        ),
    ];
    for (name, trigger) in triggers {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("run.db");
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        let engine = start(dir.path(), store.clone(), "run-1");
        engine.ingest(&mut two_units()).unwrap();
        engine.submit(launch("l1", "u1", 1)).unwrap();
        let before = (
            count(&db, "SELECT count(*) FROM admissions"),
            count(&db, "SELECT count(*) FROM ledger_outbox"),
            store.nodes("run-1").unwrap(),
        );

        Connection::open(&db)
            .unwrap()
            .execute_batch(trigger)
            .unwrap();
        let err = engine.park("u1", "stalled").unwrap_err();
        assert!(matches!(err, EngineError::Degraded(_)), "{name}: {err:?}");

        let after = (
            count(&db, "SELECT count(*) FROM admissions"),
            count(&db, "SELECT count(*) FROM ledger_outbox"),
            store.nodes("run-1").unwrap(),
        );
        assert_eq!(before, after, "{name}: a failed commit left a trace");
        assert!(
            store.admission("run-1", "park:u1").unwrap().is_none(),
            "{name}"
        );
        let unit = store
            .nodes("run-1")
            .unwrap()
            .into_iter()
            .find(|n| n.id == "u1")
            .unwrap();
        assert_eq!(unit.status, Status::Running, "{name}");
    }
}

#[test]
fn rows_and_the_outbox_survive_a_reopen_and_numbers_rise_across_runs_per_ledger_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.db");
    {
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        let engine = start(dir.path(), store.clone(), "run-1");
        engine.ingest(&mut two_units()).unwrap();
        engine.start_stage("u1").unwrap();
        engine.halt("kill switch").unwrap();
        let run = store.run("run-1").unwrap().unwrap();
        assert_eq!(run.halted_reason.as_deref(), Some("kill switch"));
        assert_eq!(run.state, RunState::Active);
    }

    // The same file opened again: the engine finds its run and replays `open-run`.
    let store = Arc::new(SqliteStore::open(&db).unwrap());
    let engine = start(dir.path(), store.clone(), "run-1");
    assert!(engine.open_run().unwrap().replayed);
    let session = &store.sessions("run-1").unwrap()[0];
    assert_eq!(session.id, "u1/build#1");
    assert!(session.process_ref.is_some());

    // A second run on the same ledger file continues the numbers.
    let second = start(dir.path(), store.clone(), "run-2");
    second.halt("another kill switch").unwrap();
    let ledger = dir.path().join("ledger.md").display().to_string();
    let seqs: Vec<i64> = {
        let conn = Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare("SELECT seq FROM ledger_outbox WHERE ledger_file = ?1 ORDER BY seq")
            .unwrap();
        stmt.query_map([&ledger], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(seqs.len() >= 5, "{seqs:?}");
    assert_eq!(
        seqs,
        (1..=i64::try_from(seqs.len()).unwrap()).collect::<Vec<_>>()
    );
    assert!(store.unwritten_ledger_lines(&ledger).unwrap().is_empty());

    // Another ledger file counts from 1.
    let other = Commit {
        run_id: "run-1".to_string(),
        command_id: "elsewhere".to_string(),
        content: "{}".to_string(),
        admission: NewAdmission {
            kind: "test".to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t".to_string(),
        },
        effects: vec![],
        ledger: Some(LedgerEntry {
            ledger_file: "other.md".to_string(),
            body: "x".to_string(),
        }),
    };
    match store.commit(other).unwrap() {
        Committed::Applied {
            ledger: Some(line), ..
        } => assert_eq!(line.seq, 1),
        other => panic!("{other:?}"),
    }
    store.mark_ledger_written("other.md", 1).unwrap();
    assert!(store.unwritten_ledger_lines("other.md").unwrap().is_empty());
}
