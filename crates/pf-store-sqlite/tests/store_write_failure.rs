//! AC-17: a forced write failure stops admission, writes `health.json` with the reason, and
//! later commands are refused.
//!
//! A SQLite trigger forces the failure, so the store fails for real, with no test hook in the
//! product code.

use std::path::Path;
use std::sync::Arc;

use pf_engine::capacity::Settings;
use pf_engine::port::agent::*;
use pf_engine::port::forge::*;
use pf_engine::port::store::Store;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::{Engine, EngineConfig, EngineError, MergeRule, StageDef, Status, Workflow};
use pf_store_sqlite::SqliteStore;
use rusqlite::Connection;

struct IdleAgent;

impl AgentPort for IdleAgent {
    fn launch(&self, _: &LaunchPlan) -> Result<SessionRef, AgentError> {
        Ok(SessionRef("p".into()))
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

struct OneUnit;

impl UnitSource for OneUnit {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        Ok(vec![UnitRecord::new("u1", "g1", 1, Default::default())])
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    store: Arc<SqliteStore>,
    engine: Engine,
}

impl Fixture {
    fn db(&self) -> std::path::PathBuf {
        self.dir.path().join("run.db")
    }
    fn health(&self) -> std::path::PathBuf {
        self.dir.path().join("state").join("health.json")
    }
    fn force_failure(&self, trigger: &str) {
        Connection::open(self.db())
            .unwrap()
            .execute_batch(trigger)
            .unwrap();
    }
    fn heal(&self) {
        Connection::open(self.db())
            .unwrap()
            .execute_batch("DROP TRIGGER IF EXISTS forced;")
            .unwrap();
    }
}

fn fixture_in(dir: tempfile::TempDir) -> Fixture {
    let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let cfg = EngineConfig {
        run_id: "run-1".to_string(),
        workflow: Workflow::single_track(
            "test-flow",
            vec![StageDef::new("build", "build-role", "model-a")],
            Some(MergeRule::GreenChecksInDeclaredOrder),
            Settings::default(),
        ),
        ledger_path: dir.path().join("ledger.md"),
        state_dir: dir.path().join("state"),
        run_dir: dir.path().join("run"),
        params: Default::default(),
    };
    let engine = Engine::start(cfg, store.clone(), Arc::new(IdleAgent), Arc::new(NoForge)).unwrap();
    engine.ingest(&mut OneUnit).unwrap();
    Fixture { dir, store, engine }
}

fn fixture() -> Fixture {
    fixture_in(tempfile::tempdir().unwrap())
}

fn admissions(f: &Fixture) -> usize {
    f.store.admissions("run-1").unwrap().len()
}

const FAIL_ADMISSION: &str = "CREATE TRIGGER forced BEFORE INSERT ON admissions BEGIN SELECT RAISE(ABORT, 'forced write failure'); END;";

#[test]
fn a_forced_write_failure_stops_admission_and_writes_health_json_with_the_reason() {
    let f = fixture();
    assert!(!f.health().exists(), "a healthy run writes no health file");
    assert_eq!(f.engine.degraded(), None);
    let before = admissions(&f);

    f.force_failure(FAIL_ADMISSION);
    let err = f.engine.park("u1", "stalled").unwrap_err();
    let EngineError::Degraded(reason) = err else {
        panic!("expected the engine to stop admitting work");
    };
    assert!(reason.contains("store write failed"), "{reason}");
    assert!(reason.contains("forced write failure"), "{reason}");
    assert_eq!(f.engine.degraded(), Some(reason.clone()));

    // health.json is outside the database file, and it carries the reason.
    let text = std::fs::read_to_string(f.health()).unwrap();
    let health: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(health["status"], "degraded");
    assert_eq!(health["run"], "run-1");
    assert_eq!(health["reason"], serde_json::Value::String(reason));
    assert!(health["at"].as_str().unwrap().ends_with('Z'));
    assert_ne!(f.health(), f.db());
    assert!(f.health().starts_with(f.dir.path().join("state")));
    assert_eq!(f.engine.health_path(), f.health());

    // The failed command left nothing in the store.
    f.heal();
    assert_eq!(admissions(&f), before);
    let unit = f
        .store
        .nodes("run-1")
        .unwrap()
        .into_iter()
        .find(|n| n.id == "u1")
        .unwrap();
    assert_eq!(unit.status, Status::Running);
}

#[test]
fn later_commands_are_refused_even_when_the_store_works_again() {
    let f = fixture();
    f.force_failure(FAIL_ADMISSION);
    f.engine.park("u1", "stalled").unwrap_err();
    f.heal();
    let before = admissions(&f);

    // Every kind of call is refused: a new command, a different command, a repeat of an old one.
    let results = [
        f.engine.halt("kill switch").map(|_| ()),
        f.engine.start_stage("u1").map(|_| ()),
        f.engine.park("u1", "stalled").map(|_| ()),
        f.engine.open_run().map(|_| ()),
        f.engine.ingest(&mut OneUnit).map(|_| ()),
    ];
    for result in results {
        assert!(
            matches!(result, Err(EngineError::Degraded(_))),
            "{result:?}"
        );
    }
    assert_eq!(
        admissions(&f),
        before,
        "a refused command wrote to the store"
    );
}

#[test]
fn a_failed_mark_of_a_ledger_line_stops_admission_too() {
    let f = fixture();
    f.force_failure(
        "CREATE TRIGGER forced BEFORE UPDATE ON ledger_outbox BEGIN SELECT RAISE(ABORT, 'forced mark failure'); END;",
    );
    let err = f.engine.halt("kill switch").unwrap_err();
    let EngineError::Degraded(reason) = err else {
        panic!("expected degraded")
    };
    assert!(reason.contains("marking ledger line"), "{reason}");
    assert!(reason.contains("forced mark failure"), "{reason}");
    assert!(f.health().exists());
    // The halt was committed before the mark failed, and it is not lost.
    f.heal();
    assert!(f.store.admission("run-1", "halt:run-1").unwrap().is_some());
    assert_eq!(
        f.store
            .unwritten_ledger_lines(&f.dir.path().join("ledger.md").display().to_string())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_restart_after_the_failure_recovers_the_ledger_line_and_admits_again() {
    let f = fixture();
    f.force_failure(
        "CREATE TRIGGER forced BEFORE UPDATE ON ledger_outbox BEGIN SELECT RAISE(ABORT, 'forced mark failure'); END;",
    );
    f.engine.halt("kill switch").unwrap_err();
    f.heal();

    let cfg = EngineConfig {
        run_id: "run-1".to_string(),
        workflow: Workflow::single_track(
            "test-flow",
            vec![StageDef::new("build", "build-role", "model-a")],
            Some(MergeRule::GreenChecksInDeclaredOrder),
            Settings::default(),
        ),
        ledger_path: f.dir.path().join("ledger.md"),
        state_dir: f.dir.path().join("state"),
        run_dir: f.dir.path().join("run"),
        params: Default::default(),
    };
    let restarted =
        Engine::start(cfg, f.store.clone(), Arc::new(IdleAgent), Arc::new(NoForge)).unwrap();
    assert_eq!(restarted.degraded(), None);
    let ledger = std::fs::read_to_string(f.dir.path().join("ledger.md")).unwrap();
    assert!(ledger.contains("run halted: kill switch"), "{ledger}");
    assert!(restarted.end_run("done").is_ok());
}

/// Run by the next test in a child process, so that the test can read the child's stderr.
#[test]
fn child_that_fails_a_write() {
    let Ok(dir) = std::env::var("PF_TEST_CHILD_DIR") else {
        return; // an ordinary run of the suite does nothing here
    };
    let f = fixture_in_path(Path::new(&dir));
    f.force_failure(FAIL_ADMISSION);
    let _ = f.engine.park("u1", "stalled");
}

fn fixture_in_path(path: &Path) -> Fixture {
    // A fresh directory inside the parent test's directory. The parent removes both.
    let dir = tempfile::Builder::new().tempdir_in(path).unwrap();
    fixture_in(dir)
}

#[test]
fn the_failure_also_goes_to_stderr_where_the_journal_collects_it() {
    let parent = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_that_fails_a_write",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PF_TEST_CHILD_DIR", parent.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("pf-engine: run run-1 degraded: store write failed"),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("forced write failure"),
        "stderr was:\n{stderr}"
    );
}
