//! AC-4: a launch that reaches no first turn fails fast, with the screen in its reason.
//! The first run (2026-10-31) left three seats at a folder-trust prompt for 48 minutes each.

mod common;

use std::sync::Arc;

use common::*;
use pf_adapter_omp::OmpAdapter;
use pf_engine::capacity::Settings;
use pf_engine::port::agent::{AgentError, AgentPort, RuleSet};
use pf_engine::port::forge::*;
use pf_engine::port::store::Store;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::{Engine, EngineConfig, MergeRule, StageDef, Status, Workflow};
use pf_store_sqlite::SqliteStore;

#[test]
fn a_silent_stub_fails_the_launch_at_the_timeout_and_the_process_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"echo $$ > "$DIR/pid"
echo 'Loading...'
exec sleep 600"#;
    let a = adapter(dir.path(), stub_omp(dir.path(), "18.11.3", body));
    let started = std::time::Instant::now();
    let err = a
        .launch(&plan(dir.path(), &[("first_turn_seconds", "1")]))
        .unwrap_err();
    let waited = started.elapsed();
    assert!(waited.as_secs_f64() >= 1.0, "it waited for the timeout");
    assert!(waited.as_secs() < 15, "it did not wait for long");
    let AgentError::Failed(why) = err else {
        panic!("expected Failed")
    };
    assert!(why.contains("no first turn in 1 s"), "{why}");
    assert!(
        why.contains("Loading"),
        "the screen tail is in the error: {why}"
    );
    let pid: u32 = std::fs::read_to_string(dir.path().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The process was ended before the error returned.
    for _ in 0..50 {
        if !alive(pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!alive(pid), "process {pid} is gone");
}

#[test]
fn a_stub_that_exits_before_a_first_turn_fails_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", "echo 'fatal: no API key'; exit 3"),
    );
    let started = std::time::Instant::now();
    let err = a.launch(&plan(dir.path(), &[])).unwrap_err();
    assert!(started.elapsed().as_secs() < 10, "it did not wait 20 s");
    let AgentError::Failed(why) = err else {
        panic!("expected Failed")
    };
    assert!(why.contains("exited with 3"), "{why}");
    assert!(why.contains("no API key"), "{why}");
}

#[test]
fn the_screen_tail_is_at_most_450_characters_and_keeps_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"i=0; while [ $i -lt 120 ]; do echo "line-$i filler filler filler"; i=$((i+1)); done
echo 'LAST QUESTION: trust this folder?'
exec sleep 600"#;
    let a = adapter(dir.path(), stub_omp(dir.path(), "18.11.3", body));
    let err = a
        .launch(&plan(dir.path(), &[("first_turn_seconds", "1")]))
        .unwrap_err();
    let AgentError::Failed(why) = err else {
        panic!("expected Failed")
    };
    let tail = why.split("screen: ").nth(1).unwrap();
    assert!(tail.chars().count() <= 450, "{}", tail.chars().count());
    assert!(tail.ends_with("trust this folder?"), "{tail}");
}

/// The engine adds `launch failed: ` and cuts a reason at 500 characters. A reason that is too
/// long would lose the end of the screen, where a prompt sits. This holds for both ways a launch
/// ends without a first turn: the timeout, and an exit.
#[test]
fn the_whole_reason_fits_the_engine_cap_and_keeps_the_last_line_of_the_screen() {
    let filler = r#"i=0; while [ $i -lt 120 ]; do echo "line-$i filler filler filler"; i=$((i+1)); done
echo 'LAST QUESTION: trust this folder?'"#;
    for (name, tail_of_script, first_turn) in [
        ("timeout", "exec sleep 600", "1"),
        ("exit", "sleep 1; exit 137", "20"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{filler}\n{tail_of_script}");
        let a = adapter(dir.path(), stub_omp(dir.path(), "18.11.3", &body));
        let err = a
            .launch(&plan(dir.path(), &[("first_turn_seconds", first_turn)]))
            .unwrap_err();
        let AgentError::Failed(why) = err else {
            panic!("expected Failed for the {name} case")
        };
        let reason = pf_engine::status::one_line_reason(&format!("launch failed: {why}"));
        assert!(
            reason.chars().count() <= 500,
            "{name}: {}",
            reason.chars().count()
        );
        assert!(
            reason.ends_with("LAST QUESTION: trust this folder?"),
            "{name}: the end of the screen was cut: {reason}"
        );
    }
}

#[test]
fn an_agent_start_in_time_reports_first_turn_started() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN),
    );
    let s = a
        .launch(&plan(dir.path(), &[("first_turn_seconds", "5")]))
        .unwrap();
    assert!(a.status(&s).unwrap().first_turn_started);
    assert!(a.is_alive(&s).unwrap());
}

// ---- a real Engine unit parks with a one-line reason

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

#[test]
fn a_trust_question_and_a_token_park_the_unit_and_the_token_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"echo "$PF_SEAT_TOKEN" > "$DIR/token"
echo "Do you trust the files in this folder? (y/n)"
echo "GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789"
echo "seat token $PF_SEAT_TOKEN"
exec sleep 600"#;
    let omp = stub_omp(dir.path(), "18.11.3", body);
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let agent: Arc<OmpAdapter> = Arc::new(adapter(dir.path(), omp));

    let mut stage = StageDef::new("build", "implementer", "zai/glm-5.3-flash");
    stage.runtime = "omp".into();
    stage.rules = RuleSet {
        name: "implementer".into(),
        prompts_off: true,
        deny: vec![],
    };
    for (k, v) in [
        ("cwd", work.to_str().unwrap()),
        ("message", "Go."),
        ("first_turn_seconds", "1"),
        ("stall_minutes", "10"),
    ] {
        stage.settings.insert(k.into(), v.into());
    }
    let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let cfg = EngineConfig {
        run_id: "run-1".to_string(),
        workflow: Workflow::single_track(
            "test-flow",
            vec![stage],
            Some(MergeRule::GreenChecksInDeclaredOrder),
            Settings::default(),
        ),
        ledger_path: dir.path().join("ledger.md"),
        state_dir: dir.path().join("state-engine"),
        run_dir: dir.path().join("run"),
        params: Default::default(),
    };
    let engine = Engine::start(cfg, store.clone(), agent.clone(), Arc::new(NoForge)).unwrap();
    engine.ingest(&mut OneUnit).unwrap();

    let step = engine.start_stage("u1").unwrap();
    let pf_engine::launch::LaunchStep::Parked { reason } = step else {
        panic!("expected the unit to park, got {step:?}");
    };
    assert!(!reason.contains('\n'), "one line: {reason}");
    assert!(reason.chars().count() <= 500, "{reason}");
    assert!(
        reason.starts_with("launch failed: no first turn in 1 s"),
        "{reason}"
    );
    assert!(
        reason.contains("Do you trust the files in this folder? (y/n)"),
        "{reason}"
    );
    assert!(
        !reason.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "{reason}"
    );
    let token = std::fs::read_to_string(dir.path().join("token")).unwrap();
    assert_eq!(token.trim().len(), 32);
    assert!(
        !reason.contains(token.trim()),
        "the seat token is gone from the reason: {reason}"
    );
    assert!(reason.contains("seat token [redacted]"), "{reason}");

    let unit = store
        .nodes("run-1")
        .unwrap()
        .into_iter()
        .find(|n| n.id == "u1")
        .unwrap();
    assert_eq!(unit.status, Status::Parked);
    assert_eq!(unit.reason.as_deref(), Some(reason.as_str()));
}
