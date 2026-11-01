//! AC-13: a router holds one adapter per runtime name. It sends a launch to the plan's runtime,
//! tags the session so later calls find the same adapter, and refuses an unknown runtime, so the
//! launch parks with a reason that names it.
//!
//! The adapters here are two fakes under two names. `pf-engine` depends on no adapter crate.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use common::{FakeForge, FakeStore, TrackedSource, config_with, counting_clock, unit};
use pf_engine::capacity::Settings;
use pf_engine::launch::LaunchStep;
use pf_engine::ledger::{FsLedger, LedgerFile};
use pf_engine::port::agent::*;
use pf_engine::port::store::Store;
use pf_engine::{Engine, MergeRule, StageDef, Status, Workflow};

/// A fake adapter that logs every call. Both fakes number their sessions `proc-1`, `proc-2`, so
/// only the router's tag tells the two apart.
struct Named {
    name: &'static str,
    log: Mutex<Vec<String>>,
    next: AtomicU64,
    plugins: Vec<&'static str>,
    dead: Mutex<Vec<String>>,
}

impl Named {
    fn new(name: &'static str) -> Arc<Named> {
        Named::with_plugins(name, &[])
    }

    fn with_plugins(name: &'static str, plugins: &[&'static str]) -> Arc<Named> {
        Arc::new(Named {
            name,
            log: Mutex::new(Vec::new()),
            next: AtomicU64::new(0),
            plugins: plugins.to_vec(),
            dead: Mutex::new(Vec::new()),
        })
    }

    fn note(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn kill(&self, inner: &str) {
        self.dead.lock().unwrap().push(inner.to_string());
    }
}

impl AgentPort for Named {
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.note(format!("launch:{}", plan.runtime));
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(SessionRef(format!("proc-{n}")))
    }
    fn status(&self, s: &SessionRef) -> Result<SessionStatus, AgentError> {
        self.note(format!("status:{}", s.0));
        Ok(SessionStatus {
            state: SessionState::Idle,
            turn_ended: true,
            first_turn_started: true,
        })
    }
    fn deliver(&self, s: &SessionRef, m: &str) -> Result<(), AgentError> {
        self.note(format!("deliver:{}:{m}", s.0));
        Ok(())
    }
    fn interrupt(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.note(format!("interrupt:{}", s.0));
        Ok(())
    }
    fn usage(&self, s: &SessionRef) -> Result<Usage, AgentError> {
        self.note(format!("usage:{}", s.0));
        Ok(Usage {
            input_tokens: if self.name == "alpha" { 1 } else { 2 },
            ..Usage::default()
        })
    }
    fn answer_dialog(&self, s: &SessionRef, d: &Dialog) -> Result<DialogAnswer, AgentError> {
        self.note(format!("dialog:{}:{}", s.0, d.kind));
        Ok(DialogAnswer::Answered(self.name.to_string()))
    }
    fn apply_rules(&self, plan: &mut LaunchPlan, rules: &RuleSet) -> Result<(), AgentError> {
        self.note(format!("apply_rules:{}", rules.name));
        plan.settings
            .insert("rules_by".to_string(), self.name.to_string());
        Ok(())
    }
    fn remove_ask_tool(&self, plan: &mut LaunchPlan) -> Result<(), AgentError> {
        self.note("remove_ask_tool".to_string());
        plan.settings
            .insert("ask_by".to_string(), self.name.to_string());
        Ok(())
    }
    fn hand_off(&self, s: &SessionRef, successor: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.note(format!("hand_off:{}", s.0));
        self.launch(successor)
    }
    fn is_alive(&self, s: &SessionRef) -> Result<bool, AgentError> {
        self.note(format!("is_alive:{}", s.0));
        Ok(!self.dead.lock().unwrap().contains(&s.0))
    }
    fn resolve_plugin(&self, name: &str, _min: &str) -> Result<ResolvedPlugin, AgentError> {
        self.note(format!("resolve_plugin:{name}"));
        if self.plugins.contains(&name) {
            Ok(ResolvedPlugin {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                path: format!("/plugins/{}/{name}", self.name),
            })
        } else {
            Err(AgentError::Refused(format!(
                "{} has no plugin {name}",
                self.name
            )))
        }
    }
    fn drain_events(&self, s: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        self.note(format!("drain_events:{}", s.0));
        Ok(vec![SessionEvent::Stalled {
            screen_tail: self.name.to_string(),
        }])
    }
}

fn plan(runtime: &str) -> LaunchPlan {
    LaunchPlan {
        run_id: "run-1".into(),
        unit_id: "u1".into(),
        stage: "build".into(),
        role: "builder".into(),
        model: "model-a".into(),
        runtime: runtime.into(),
        attempt: 1,
        settings: BTreeMap::new(),
    }
}

fn rules(name: &str) -> RuleSet {
    RuleSet {
        name: name.into(),
        ..RuleSet::default()
    }
}

fn two() -> (AgentRouter, Arc<Named>, Arc<Named>) {
    let alpha = Named::new("alpha");
    let beta = Named::new("beta");
    let mut router = AgentRouter::new();
    router.register("alpha", alpha.clone()).unwrap();
    router.register("beta", beta.clone()).unwrap();
    (router, alpha, beta)
}

fn refused(result: Result<impl std::fmt::Debug, AgentError>) -> String {
    match result {
        Err(AgentError::Refused(why)) => why,
        other => panic!("the call must be refused: {other:?}"),
    }
}

#[test]
fn a_launch_goes_to_the_adapter_of_the_plans_runtime() {
    let (router, alpha, beta) = two();
    router.launch(&plan("beta")).unwrap();
    router.launch(&plan("beta")).unwrap();
    router.launch(&plan("alpha")).unwrap();
    assert_eq!(alpha.log(), vec!["launch:alpha"]);
    assert_eq!(beta.log(), vec!["launch:beta", "launch:beta"]);
}

#[test]
fn the_session_is_tagged_and_later_calls_find_the_same_adapter() {
    let (router, alpha, beta) = two();
    // Both fakes number their first session `proc-1`: the tag is all that tells them apart.
    let a = router.launch(&plan("alpha")).unwrap();
    let b = router.launch(&plan("beta")).unwrap();
    assert_ne!(a, b);
    assert!(a.0.starts_with("alpha:"), "{a:?}");
    assert!(b.0.starts_with("beta:"), "{b:?}");

    router.status(&b).unwrap();
    router.deliver(&b, "hello").unwrap();
    router.interrupt(&a).unwrap();
    assert_eq!(router.usage(&a).unwrap().input_tokens, 1);
    assert_eq!(router.usage(&b).unwrap().input_tokens, 2);
    let dialog = Dialog {
        kind: "trust".into(),
        text: "Trust this folder?".into(),
    };
    assert_eq!(
        router.answer_dialog(&b, &dialog).unwrap(),
        DialogAnswer::Answered("beta".into())
    );
    assert_eq!(
        router.drain_events(&a).unwrap(),
        vec![SessionEvent::Stalled {
            screen_tail: "alpha".into()
        }]
    );

    // The adapter sees its own reference, without the tag.
    assert_eq!(
        alpha.log(),
        vec![
            "launch:alpha",
            "interrupt:proc-1",
            "usage:proc-1",
            "drain_events:proc-1"
        ]
    );
    assert_eq!(
        beta.log(),
        vec![
            "launch:beta",
            "status:proc-1",
            "deliver:proc-1:hello",
            "usage:proc-1",
            "dialog:proc-1:trust"
        ]
    );
}

#[test]
fn liveness_is_asked_of_the_adapter_that_owns_the_session() {
    let (router, alpha, _beta) = two();
    let a = router.launch(&plan("alpha")).unwrap();
    let b = router.launch(&plan("beta")).unwrap();
    alpha.kill("proc-1");
    assert!(!router.is_alive(&a).unwrap());
    assert!(router.is_alive(&b).unwrap());
}

#[test]
fn rules_and_the_ask_tool_are_edited_by_the_adapter_of_the_plans_runtime() {
    let (router, alpha, beta) = two();
    let mut p = plan("beta");
    router.apply_rules(&mut p, &rules("implementer")).unwrap();
    router.remove_ask_tool(&mut p).unwrap();
    assert_eq!(p.settings["rules_by"], "beta");
    assert_eq!(p.settings["ask_by"], "beta");
    assert_eq!(
        beta.log(),
        vec!["apply_rules:implementer", "remove_ask_tool"]
    );
    assert!(alpha.log().is_empty());
}

#[test]
fn a_hand_off_stays_with_the_runtime_of_the_session() {
    let (router, alpha, beta) = two();
    let a = router.launch(&plan("alpha")).unwrap();
    let next = router.hand_off(&a, &plan("alpha")).unwrap();
    assert!(next.0.starts_with("alpha:"), "{next:?}");
    assert_ne!(next, a);
    assert_eq!(
        alpha.log(),
        vec!["launch:alpha", "hand_off:proc-1", "launch:alpha"]
    );

    // A successor under another runtime is refused, and nothing is launched for it.
    let why = refused(router.hand_off(&a, &plan("beta")));
    assert!(why.contains("alpha") && why.contains("beta"), "{why}");
    assert!(beta.log().is_empty());
}

#[test]
fn a_plugin_is_resolved_by_the_first_adapter_that_has_it() {
    let alpha = Named::with_plugins("alpha", &["shared"]);
    let beta = Named::with_plugins("beta", &["shared", "only-beta"]);
    let mut router = AgentRouter::new();
    router.register("beta", beta.clone()).unwrap();
    router.register("alpha", alpha.clone()).unwrap();

    // Adapters are tried in name order, whatever the order of registration.
    assert_eq!(
        router.resolve_plugin("shared", "1.0.0").unwrap().path,
        "/plugins/alpha/shared"
    );
    assert_eq!(
        router.resolve_plugin("only-beta", "1.0.0").unwrap().path,
        "/plugins/beta/only-beta"
    );
    let why = refused(router.resolve_plugin("nowhere", "1.0.0"));
    assert!(why.contains("nowhere"), "{why}");
}

#[test]
fn a_router_with_no_adapter_resolves_no_plugin() {
    let router = AgentRouter::new();
    refused(router.resolve_plugin("any", "1.0.0"));
}

#[test]
fn an_unknown_runtime_is_refused_by_name() {
    let (router, alpha, beta) = two();
    let why = refused(router.launch(&plan("ghost")));
    assert!(why.contains("no adapter for runtime \"ghost\""), "{why}");
    let mut p = plan("ghost");
    assert!(refused(router.apply_rules(&mut p, &rules("r"))).contains("ghost"));
    assert!(refused(router.remove_ask_tool(&mut p)).contains("ghost"));
    assert!(alpha.log().is_empty() && beta.log().is_empty());
}

#[test]
fn a_session_with_an_unknown_or_missing_tag_is_refused() {
    let (router, alpha, beta) = two();
    for raw in ["ghost:proc-1", "proc-1", ""] {
        let s = SessionRef(raw.to_string());
        refused(router.status(&s));
        refused(router.interrupt(&s));
        refused(router.is_alive(&s));
        refused(router.drain_events(&s));
    }
    assert!(alpha.log().is_empty() && beta.log().is_empty());
}

#[test]
fn a_runtime_name_with_a_colon_or_no_text_or_a_second_registration_is_refused() {
    let mut router = AgentRouter::new();
    let a = Named::new("alpha");
    assert!(matches!(
        router.register("a:b", a.clone()),
        Err(AgentError::Refused(_))
    ));
    assert!(matches!(
        router.register("", a.clone()),
        Err(AgentError::Refused(_))
    ));
    router.register("alpha", a.clone()).unwrap();
    assert!(matches!(
        router.register("alpha", a),
        Err(AgentError::Refused(_))
    ));
}

// ------------------------------------------------------------------ with a real engine

fn engine_on(router: AgentRouter, runtime: &str) -> (Engine, Arc<FakeStore>, tempfile::TempDir) {
    let mut stage = StageDef::new("build", "builder", "model-a");
    stage.runtime = runtime.to_string();
    let wf = Workflow::single_track(
        "router-flow",
        vec![stage],
        Some(MergeRule::GreenChecksInDeclaredOrder),
        Settings::default(),
    );
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    let ledger: Box<dyn LedgerFile> = Box::new(FsLedger::new(dir.path().join("ledger.md")));
    let engine = Engine::new(
        config_with(dir.path(), wf, BTreeMap::new()),
        store.clone(),
        Arc::new(router),
        FakeForge::new(),
        counting_clock(),
        ledger,
    )
    .unwrap();
    engine.start_up().unwrap();
    engine
        .ingest(&mut TrackedSource {
            track: "main".into(),
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    (engine, store, dir)
}

#[test]
fn the_engine_launches_a_stage_on_the_adapter_of_its_runtime() {
    let (router, alpha, beta) = two();
    let (engine, store, _dir) = engine_on(router, "beta");
    match engine.start_stage("u1").unwrap() {
        LaunchStep::Launched { process_ref, .. } => {
            assert!(process_ref.starts_with("beta:"), "{process_ref}");
        }
        other => panic!("the launch must start: {other:?}"),
    }
    assert!(alpha.log().is_empty());
    assert!(beta.log().iter().any(|l| l == "launch:beta"));
    let session = &store.sessions("run-1").unwrap()[0];
    assert!(session.process_ref.as_deref().unwrap().starts_with("beta:"));
}

#[test]
fn an_unknown_runtime_parks_the_unit_with_a_reason_that_names_it() {
    let (router, alpha, beta) = two();
    let (engine, store, _dir) = engine_on(router, "ghost");
    let reason = match engine.start_stage("u1").unwrap() {
        LaunchStep::Parked { reason } => reason,
        other => panic!("the launch must park: {other:?}"),
    };
    assert!(reason.contains("ghost"), "{reason}");
    assert!(reason.contains("no adapter for runtime"), "{reason}");
    let node = store.node("run-1", "u1").unwrap();
    assert_eq!(node.status, Status::Parked);
    assert!(node.reason.unwrap().contains("ghost"));
    assert!(alpha.log().is_empty() && beta.log().is_empty());
}
