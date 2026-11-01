//! AC-8: the fake adapter behaves as scripted, behind the same port, with no process and no
//! network. A run on it parks on a refused launch, on no first turn and on an unknown dialog.

use std::sync::Arc;

use pf_adapter_fake::{FakeAdapter, Scenario, Step};
use pf_engine::admission::PushOutcome;
use pf_engine::capacity::Settings;
use pf_engine::launch::LaunchStep;
use pf_engine::port::agent::*;
use pf_engine::port::forge::*;
use pf_engine::port::store::Store;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::{Engine, EngineConfig, MergeRule, StageDef, Status, Workflow};
use pf_store_sqlite::SqliteStore;

fn plan(unit: &str) -> LaunchPlan {
    LaunchPlan {
        run_id: "run-1".into(),
        unit_id: unit.into(),
        stage: "build".into(),
        role: "implementer".into(),
        model: "model-a".into(),
        runtime: "fake".into(),
        attempt: 1,
        settings: Default::default(),
    }
}

fn usage(n: u64) -> Usage {
    Usage {
        input_tokens: n,
        output_tokens: n / 10,
        cache_read_tokens: n * 2,
        cache_write_tokens: 1,
        cost_micros: n * 3,
    }
}

#[test]
fn a_script_gives_turn_ends_usage_and_pushes_in_order() {
    let fake = FakeAdapter::new();
    fake.script(
        "u1",
        Scenario::Script(vec![
            Step::Usage(usage(100)),
            Step::Push {
                branch: "work/u1".into(),
                sha: "a".repeat(40),
                outcome: PushOutcome::Pushed,
            },
            Step::TurnEnd,
            Step::Push {
                branch: "work/u1".into(),
                sha: "b".repeat(40),
                outcome: PushOutcome::Failed("rejected".into()),
            },
            Step::Usage(usage(50)),
        ]),
    );
    let s = fake.launch(&plan("u1")).unwrap();
    let st = fake.status(&s).unwrap();
    assert!(st.first_turn_started);
    assert_eq!(st.state, SessionState::Working);
    assert!(!st.turn_ended);
    assert!(fake.is_alive(&s).unwrap());

    fake.advance(&s, 3); // usage, push, turn end
    let st = fake.status(&s).unwrap();
    assert!(st.turn_ended);
    assert_eq!(st.state, SessionState::Idle);
    assert_eq!(fake.usage(&s).unwrap(), usage(100));
    let events = fake.drain_events(&s).unwrap();
    let pushes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Push { .. }))
        .collect();
    assert_eq!(pushes.len(), 1);
    assert!(
        fake.drain_events(&s).unwrap().is_empty(),
        "a drain takes the events"
    );

    fake.finish(&s);
    let u = fake.usage(&s).unwrap();
    assert_eq!(u.input_tokens, 150, "usage sums");
    let events = fake.drain_events(&s).unwrap();
    let ids: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Push { id, outcome, .. } => Some((id.clone(), outcome.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].1, PushOutcome::Failed("rejected".into()));
}

#[test]
fn a_delivery_reaches_an_idle_session_only() {
    let fake = FakeAdapter::new();
    fake.script("u1", Scenario::Script(vec![Step::TurnEnd]));
    let s = fake.launch(&plan("u1")).unwrap();
    assert!(fake.deliver(&s, "early").is_err());
    fake.advance(&s, 1);
    fake.deliver(&s, "next").unwrap();
    assert_eq!(fake.delivered(&s), vec!["next".to_string()]);
    fake.interrupt(&s).unwrap();
    assert_eq!(fake.interrupt_count(), 1);
}

#[test]
fn push_ids_differ_between_events() {
    let fake = FakeAdapter::new();
    let step = || Step::Push {
        branch: "b".into(),
        sha: "c".repeat(40),
        outcome: PushOutcome::Pushed,
    };
    fake.script("u1", Scenario::Script(vec![step(), step()]));
    let s = fake.launch(&plan("u1")).unwrap();
    fake.finish(&s);
    let ids: Vec<String> = fake
        .drain_events(&s)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::Push { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn a_stall_and_a_moved_default_branch_are_events() {
    let fake = FakeAdapter::new();
    fake.script(
        "u1",
        Scenario::Script(vec![
            Step::Stalled("Press enter to continue".into()),
            Step::DefaultBranchMoved {
                branch: "main".into(),
                before: "1".repeat(40),
                after: "2".repeat(40),
            },
            Step::Activity,
        ]),
    );
    let s = fake.launch(&plan("u1")).unwrap();
    fake.finish(&s);
    let events = fake.drain_events(&s).unwrap();
    assert!(events.contains(&SessionEvent::Stalled {
        screen_tail: "Press enter to continue".into()
    }));
    assert!(
        events.iter().any(
            |e| matches!(e, SessionEvent::DefaultBranchMoved { branch, .. } if branch == "main")
        )
    );
    assert!(events.contains(&SessionEvent::Activity));
}

#[test]
fn a_refused_launch_a_launch_with_no_first_turn_and_an_unknown_dialog_fail_with_their_reason() {
    let fake = FakeAdapter::new();
    fake.script(
        "refused",
        Scenario::RefusedLaunch("OMP 19.0.0 is not qualified".into()),
    );
    fake.script(
        "silent",
        Scenario::NoFirstTurn {
            screen: "Do you trust this folder?  GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789"
                .into(),
        },
    );
    fake.script(
        "dialog",
        Scenario::UnknownDialog(Dialog {
            kind: "update".into(),
            text: "A new version is ready. Restart now?".into(),
        }),
    );
    assert_eq!(
        fake.launch(&plan("refused")).unwrap_err(),
        AgentError::Refused("OMP 19.0.0 is not qualified".into())
    );
    match fake.launch(&plan("silent")).unwrap_err() {
        AgentError::Failed(why) => {
            assert!(why.starts_with("no first turn"), "{why}");
            assert!(why.contains("Do you trust this folder?"), "{why}");
            assert!(
                !why.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
                "{why}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    match fake.launch(&plan("dialog")).unwrap_err() {
        AgentError::Failed(why) => {
            assert!(
                why.contains("unknown dialog") && why.contains("Restart now?"),
                "{why}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    // No launch of a failed scenario left a session.
    assert_eq!(fake.launch_count(), 0);
}

#[test]
fn a_dialog_the_fake_does_not_know_is_unknown_and_a_known_one_is_answered() {
    let fake = FakeAdapter::new();
    fake.know_dialog("trust", "y");
    let s = fake.launch(&plan("u1")).unwrap();
    let ask = |kind: &str| {
        fake.answer_dialog(
            &s,
            &Dialog {
                kind: kind.into(),
                text: "?".into(),
            },
        )
        .unwrap()
    };
    assert_eq!(ask("trust"), DialogAnswer::Answered("y".into()));
    assert_eq!(ask("surprise"), DialogAnswer::Unknown);
}

#[test]
fn plugins_rules_and_the_ask_tool_follow_the_port() {
    let fake = FakeAdapter::new();
    fake.add_plugin("ossify", "1.32.2", "/plugins/ossify");
    let p = fake.resolve_plugin("ossify", "1.26.0").unwrap();
    assert_eq!(
        (p.version.as_str(), p.path.as_str()),
        ("1.32.2", "/plugins/ossify")
    );
    assert!(matches!(
        fake.resolve_plugin("ossify", "2.0.0"),
        Err(AgentError::Refused(_))
    ));
    assert!(matches!(
        fake.resolve_plugin("nope", "1.0.0"),
        Err(AgentError::Refused(_))
    ));
    let mut plan = plan("u1");
    let rules = RuleSet {
        name: "implementer".into(),
        prompts_off: true,
        deny: vec![],
    };
    fake.apply_rules(&mut plan, &rules).unwrap();
    fake.remove_ask_tool(&mut plan).unwrap();
    assert_eq!(plan.settings["rule_set"], "implementer");
    assert_eq!(plan.settings["ask_tool"], "off");
}

#[test]
fn hand_off_starts_a_successor_and_ends_the_old_session() {
    let fake = FakeAdapter::new();
    let s1 = fake.launch(&plan("u1")).unwrap();
    let s2 = fake.hand_off(&s1, &plan("u1")).unwrap();
    assert_ne!(s1, s2);
    assert!(!fake.is_alive(&s1).unwrap());
    assert!(fake.is_alive(&s2).unwrap());
}

// ---- a run on the fake adapter parks, each unit with its reason

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

struct Units;
impl UnitSource for Units {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        Ok(["refused", "silent", "dialog", "fine"]
            .iter()
            .enumerate()
            .map(|(i, id)| UnitRecord::new(*id, format!("g{i}"), i as u32 + 1, Default::default()))
            .collect())
    }
}

#[test]
fn a_run_on_the_fake_adapter_parks_on_each_failed_launch_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeAdapter::new());
    fake.script(
        "refused",
        Scenario::RefusedLaunch("OMP 19.0.0 is not qualified".into()),
    );
    fake.script(
        "silent",
        Scenario::NoFirstTurn {
            screen: "Do you trust this folder?".into(),
        },
    );
    fake.script(
        "dialog",
        Scenario::UnknownDialog(Dialog {
            kind: "update".into(),
            text: "Restart now?".into(),
        }),
    );
    fake.script("fine", Scenario::Script(vec![Step::TurnEnd]));

    let mut stage = StageDef::new("build", "implementer", "model-a");
    stage.runtime = "fake".into();
    stage.rules = RuleSet {
        name: "implementer".into(),
        prompts_off: true,
        deny: vec![],
    };
    let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let cfg = EngineConfig {
        run_id: "run-1".into(),
        workflow: Workflow::single_track(
            "test-flow",
            vec![stage],
            Some(MergeRule::GreenChecksInDeclaredOrder),
            Settings::default(),
        ),
        ledger_path: dir.path().join("ledger.md"),
        state_dir: dir.path().join("state"),
        run_dir: dir.path().join("run"),
        params: Default::default(),
    };
    let engine = Engine::start(cfg, store.clone(), fake.clone(), Arc::new(NoForge)).unwrap();
    engine.ingest(&mut Units).unwrap();

    let reason_of = |unit: &str| match engine.start_stage(unit).unwrap() {
        LaunchStep::Parked { reason } => reason,
        other => panic!("{unit}: expected a park, got {other:?}"),
    };
    assert_eq!(
        reason_of("refused"),
        "launch refused: OMP 19.0.0 is not qualified"
    );
    let silent = reason_of("silent");
    assert!(
        silent.starts_with("launch failed: no first turn") && silent.contains("trust this folder"),
        "{silent}"
    );
    let dialog = reason_of("dialog");
    assert!(
        dialog.contains("unknown dialog") && dialog.contains("Restart now?"),
        "{dialog}"
    );
    assert!(matches!(
        engine.start_stage("fine").unwrap(),
        LaunchStep::Launched { .. }
    ));

    let nodes = store.nodes("run-1").unwrap();
    for id in ["refused", "silent", "dialog"] {
        let u = nodes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(u.status, Status::Parked, "{id}");
        assert!(
            u.reason
                .as_deref()
                .is_some_and(|r| !r.contains('\n') && !r.is_empty())
        );
    }
    assert_eq!(
        nodes.iter().find(|n| n.id == "fine").unwrap().status,
        Status::Running
    );
}

/// The engine adds `launch failed: ` and cuts a reason at 500 characters. A cut takes the end of
/// the screen, where a prompt sits, so the fake keeps the end and fits the whole reason, as the
/// real adapter does.
#[test]
fn a_long_screen_keeps_its_last_line_and_the_whole_reason_fits_the_engine_cap() {
    let fake = FakeAdapter::new();
    let mut screen = String::new();
    for i in 0..120 {
        screen.push_str(&format!("line-{i} filler filler filler\n"));
    }
    screen.push_str("LAST QUESTION: trust this folder?");
    fake.script("long", Scenario::NoFirstTurn { screen });
    let AgentError::Failed(why) = fake.launch(&plan("long")).unwrap_err() else {
        panic!("expected Failed")
    };
    let reason = pf_engine::status::one_line_reason(&format!("launch failed: {why}"));
    assert!(reason.chars().count() <= 500, "{}", reason.chars().count());
    assert!(
        reason.ends_with("LAST QUESTION: trust this folder?"),
        "{reason}"
    );
}
