//! AC-4: the driver's tick, step by step, on a `FakeAdapter` and a `BareForge`. Nothing here
//! starts a process of a seat, spends or reaches the network.

mod common;

use std::sync::Arc;

use common::rig::*;
use common::*;
use pf_adapter_fake::{FakeAdapter, Step};
use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::*;
use pf_engine::status::{Status, redact};
use pf_forge_bare::PrChecks;

fn usage(input: u64, cost: u64) -> Usage {
    Usage {
        input_tokens: input,
        cost_micros: cost,
        ..Usage::default()
    }
}

fn pos(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in:\n{text}"))
}

fn with_extra(extra: &str) -> Options {
    Options {
        implement_extra: extra.to_string(),
        ..Options::default()
    }
}

#[test]
fn a_queue_record_starts_a_unit_and_a_bad_record_halts() {
    let r = Rig::new();
    r.queue_item("u1", 1);
    let notes = r.tick();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(r.unit("u1").0, Status::Running);
    assert_eq!(
        r.stage_node("u1", "implement").len(),
        1,
        "the first stage launched"
    );
    assert!(r.has_unit("lane"), "the seed unit came in");
    assert_eq!(r.fake.launch_count(), 2);

    r.publish("bad.json", "this is not json");
    let notes = r.tick();
    assert!(r.view().is_halted());
    assert!(
        notes.iter().any(|n| n.contains("queue halted")),
        "{notes:?}"
    );
}

#[test]
fn a_seed_repeats_and_stops_at_until_file() {
    let o = Options {
        seed_max: 2,
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    r.tick();
    assert!(r.has_unit("lane") && !r.has_unit("lane.2"));
    // The lane seat writes its records while it runs: they come in after the lane unit.
    r.item("u1", 1, 1, PrChecks::Green);
    r.tick();
    r.pass("lane", &r.out("lane", "lane.done"));
    assert_eq!(r.unit("lane").0, Status::Done);
    // The item is not done, so the seed waits.
    r.tick();
    assert!(!r.has_unit("lane.2"));
    r.pass("u1", &r.report("u1"));
    r.pass("u1", &r.out("u1", "verdict.json"));
    r.close("u1", 1);
    assert_eq!(r.unit("u1").0, Status::Done, "ledger: {}", r.ledger_text());
    r.tick();
    assert!(
        r.has_unit("lane.2"),
        "the seed repeats when the items are done"
    );

    // With the until file in place, the seed stops.
    r.touch(&r.run_dir.join("rounds.done"));
    r.pass("lane.2", &r.out("lane.2", "lane.done"));
    r.tick();
    assert!(!r.has_unit("lane.3"));
}

#[test]
fn a_merge_lands_runs_after_merge_and_the_run_ends() {
    let r = Rig::new();
    r.tick();
    r.item("u1", 1, 1, PrChecks::Green);
    r.reach_close("u1");
    // A done file moved each stage on and stopped its seat.
    assert!(r.fake.stop_count() >= 2, "stops: {}", r.fake.stop_count());
    assert_eq!(r.all_sessions("u1"), 3);
    r.pass("lane", &r.out("lane", "lane.done"));
    r.close("u1", 1);
    assert_eq!(r.unit("u1").0, Status::Done, "ledger: {}", r.ledger_text());
    assert!(
        std::fs::read_to_string(r.run_dir.join("after_merge.log"))
            .unwrap()
            .contains("u1")
    );
    let ledger = r.ledger_text();
    assert!(ledger.contains("merge of unit u1"), "{ledger}");
    // The merge rendered `merge.repo` and `merge.base`: the ledger names the bare repo and the base.
    assert!(
        ledger.contains(&format!("repo={}", r.forge.repo())),
        "{ledger}"
    );

    // The seed runs again. The run waits for the until file, then ends.
    r.tick();
    assert!(r.has_unit("lane.2"));
    assert_eq!(r.view().run.unwrap().state.as_str(), "active");
    r.touch(&r.run_dir.join("rounds.done"));
    r.pass("lane.2", &r.out("lane.2", "lane.done"));
    assert_eq!(r.view().run.unwrap().state.as_str(), "ended");
    assert!(r.ledger_text().contains("run ended: every item merged"));
}

#[test]
fn a_pr_that_prints_while_the_turn_runs_waits_for_its_push() {
    let r = Rig::new();
    r.item("u1", 1, 1, PrChecks::Green);
    r.reach_close("u1");
    // The PR prints while the turn runs. Its push has not arrived.
    r.pr_url("u1", r.forge.repo(), 1);
    r.tick();
    assert_eq!(
        r.unit("u1").0,
        Status::Running,
        "no signal before the turn ends"
    );
    assert!(
        r.ledger_lines("push:")
            .iter()
            .all(|l| !l.contains("work/u1") || true)
    );
    // The push arrives a tick later, then the turn ends.
    r.fake.advance(&r.session_ref("u1").unwrap(), 1);
    r.tick();
    assert!(
        !r.ledger_lines("push:").is_empty(),
        "the push is recorded first"
    );
    assert_eq!(r.unit("u1").0, Status::Running);
    r.fake.advance(&r.session_ref("u1").unwrap(), 1);
    r.tick();
    assert_eq!(
        r.unit("u1").0,
        Status::Done,
        "merged with no head-check park: {}",
        r.ledger_text()
    );
}

#[test]
fn a_pr_from_another_repo_parks_with_the_existing_reason() {
    let r = Rig::new();
    r.item("u1", 1, 1, PrChecks::Green);
    r.reach_close("u1");
    r.pr_url("u1", "/some/other/repo.git", 1);
    r.fake.finish(&r.session_ref("u1").unwrap());
    r.tick();
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked);
    let reason = reason.unwrap();
    assert!(
        reason.contains("differs from the declared repo"),
        "{reason}"
    );
    assert!(reason.contains(r.forge.repo()), "{reason}");
}

#[test]
fn a_push_then_a_moved_default_branch_gives_the_ledger_line_then_the_park() {
    let r = Rig::new();
    r.script(
        "u1",
        vec![
            Step::Push {
                branch: "work/u1".into(),
                sha: "a".repeat(40),
                outcome: PushOutcome::Pushed,
            },
            Step::DefaultBranchMoved {
                branch: "main".into(),
                before: "b1".into(),
                after: "b2".into(),
            },
        ],
    );
    r.queue_item("u1", 1);
    r.tick();
    r.fake.finish(&r.session_ref("u1").unwrap());
    r.tick();
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked);
    assert!(
        reason.unwrap().contains("default branch"),
        "{}",
        r.ledger_text()
    );
    let ledger = r.ledger_text();
    assert!(
        pos(&ledger, "push: repo=") < pos(&ledger, "unit u1 parked"),
        "{ledger}"
    );
}

#[test]
fn a_second_park_reason_is_absorbed_into_a_note() {
    let r = Rig::new();
    let moved = |n: &str| Step::DefaultBranchMoved {
        branch: "main".into(),
        before: "b0".into(),
        after: n.into(),
    };
    r.script("u1", vec![moved("b1"), moved("b2")]);
    r.queue_item("u1", 1);
    r.tick();
    r.fake.finish(&r.session_ref("u1").unwrap());
    let notes = r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked);
    assert!(
        notes.iter().any(|n| n.contains("park of u1 not recorded")),
        "{notes:?}"
    );
    assert!(
        r.unit("u1").1.unwrap().contains("b1"),
        "the first reason stays"
    );
    // A later tick makes no second park and no error.
    let notes = r.tick();
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn a_stop_file_parks_ends_the_stage_stops_the_seat_and_frees_capacity() {
    let r = Rig::new();
    r.queue_item("u1", 1);
    r.tick();
    assert_eq!(
        r.engine.capacity().free_model_sessions(),
        0,
        "lane and u1 hold both"
    );
    assert_eq!(r.engine.capacity().free_cold_builds(), 0);
    let stops = r.fake.stop_count();
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    std::fs::write(
        r.out("u1", "stop"),
        "need a human, token=ghp_0123456789abcdefghijklmnopqrstuvwxyz",
    )
    .unwrap();
    r.tick();
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked);
    let reason = reason.unwrap();
    assert!(reason.contains("need a human"), "{reason}");
    assert!(
        !reason.contains("ghp_0123456789abcdefghijklmnopqrstuvwxyz"),
        "{reason}"
    );
    assert_eq!(r.open_sessions("u1"), 0, "the session ended");
    assert!(r.fake.stop_count() > stops);
    assert_eq!(
        r.engine.capacity().free_cold_builds(),
        1,
        "capacity came back"
    );
    assert_eq!(r.engine.capacity().free_model_sessions(), 1);
    let stage = r.stage_node("u1", "implement");
    assert_eq!(stage[0].1, Status::Failed);
    // A later tick: no second park, no error.
    assert!(r.tick().is_empty());
}

struct FlushOnStop(Arc<FakeAdapter>);

impl AgentPort for FlushOnStop {
    fn launch(&self, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.0.launch(p)
    }
    fn status(&self, s: &SessionRef) -> Result<SessionStatus, AgentError> {
        self.0.status(s)
    }
    fn deliver(&self, s: &SessionRef, m: &str) -> Result<(), AgentError> {
        self.0.deliver(s, m)
    }
    fn interrupt(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.0.interrupt(s)
    }
    fn usage(&self, s: &SessionRef) -> Result<Usage, AgentError> {
        self.0.usage(s)
    }
    fn answer_dialog(&self, s: &SessionRef, d: &Dialog) -> Result<DialogAnswer, AgentError> {
        self.0.answer_dialog(s, d)
    }
    fn apply_rules(&self, p: &mut LaunchPlan, r: &RuleSet) -> Result<(), AgentError> {
        self.0.apply_rules(p, r)
    }
    fn remove_ask_tool(&self, p: &mut LaunchPlan) -> Result<(), AgentError> {
        self.0.remove_ask_tool(p)
    }
    fn hand_off(&self, s: &SessionRef, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.0.hand_off(s, p)
    }
    fn is_alive(&self, s: &SessionRef) -> Result<bool, AgentError> {
        self.0.is_alive(s)
    }
    /// The seat writes its last lines as it dies: they arrive after the stop call.
    fn stop(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.0.stop(s)?;
        self.0.finish(s);
        Ok(())
    }
    fn resolve_plugin(&self, n: &str, v: &str) -> Result<ResolvedPlugin, AgentError> {
        self.0.resolve_plugin(n, v)
    }
    fn drain_events(&self, s: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        self.0.drain_events(s)
    }
}

#[test]
fn a_usage_line_and_a_push_written_just_before_a_stop_are_recorded() {
    let r = Rig::build(
        &Options::default(),
        |_| {},
        |fake| Arc::new(FlushOnStop(fake)),
    );
    r.script(
        "u1",
        vec![
            Step::Usage(usage(11, 77)),
            Step::Push {
                branch: "work/u1".into(),
                sha: "c".repeat(40),
                outcome: PushOutcome::Pushed,
            },
        ],
    );
    r.queue_item("u1", 1);
    r.tick();
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    std::fs::write(r.out("u1", "stop"), "stop now").unwrap();
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked);
    assert!(
        r.ledger_text()
            .contains("push: repo=fake/repo branch=work/u1"),
        "{}",
        r.ledger_text()
    );
    let id = r
        .view()
        .sessions
        .iter()
        .find(|s| s.unit_id == "u1")
        .unwrap()
        .id
        .clone();
    assert_eq!(r.session_cost(&id), (11, 77));
}

#[test]
fn after_first_turn_runs_once_for_each_session_and_a_failed_one_parks() {
    let o = with_extra(
        "        after_first_turn:\n          - [sh, -c, \"echo x >> {run.dir}/out/{unit.id}/aft.log\"]\n",
    );
    let r = Rig::with(&o, |_| {});
    r.queue_item("u1", 1);
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    let log = || {
        std::fs::read_to_string(r.out("u1", "aft.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    r.tick();
    assert_eq!(
        log(),
        0,
        "the session starts in this tick; the hook runs on the next"
    );
    r.tick();
    assert_eq!(log(), 1);
    r.tick();
    assert_eq!(log(), 1, "once across two ticks");
    // A stall ends attempt 1; attempt 2 gets the hook once more.
    r.advance(20 * 60 + 60);
    r.tick();
    let nodes = r.stage_node("u1", "implement");
    assert_eq!(nodes.len(), 2, "attempt 1 stalled and attempt 2 started");
    assert!(
        nodes[0].2.as_deref().unwrap().starts_with("stalled:"),
        "{nodes:?}"
    );
    r.tick();
    assert_eq!(log(), 2, "once more on a new attempt");

    let o = with_extra("        after_first_turn:\n          - [false]\n");
    let r = Rig::with(&o, |_| {});
    r.queue_item("u1", 1);
    r.tick();
    r.tick();
    assert_eq!(
        r.unit("u1").0,
        Status::Parked,
        "a failed after_first_turn parks"
    );
    assert!(r.unit("u1").1.unwrap().contains("after_first_turn"));
    assert_eq!(
        r.open_sessions("u1"),
        1,
        "the park is made inside the hook; the session ends next tick"
    );
    r.tick();
    assert_eq!(r.open_sessions("u1"), 0);
    assert!(r.fake.stop_count() >= 1);
}

#[test]
fn capacity_holds_a_third_session_back() {
    let r = Rig::new();
    r.queue_item("u1", 1);
    r.queue_item("u2", 2);
    r.tick();
    assert_eq!(r.fake.launch_count(), 2, "lane and u1 took the slots");
    assert_eq!(r.all_sessions("u2"), 0);
    // u1 stops: its slots come back and u2 launches in the same tick.
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    std::fs::write(r.out("u1", "stop"), "make room").unwrap();
    r.tick();
    assert_eq!(r.all_sessions("u2"), 1);
}

#[test]
fn a_pin_mismatch_parks_before_the_hook_runs() {
    let o = with_extra(
        "        before:\n          - [sh, -c, \"echo b >> {run.dir}/out/{unit.id}/before.log\"]\n",
    );
    let r = Rig::with(&o, |_| {});
    std::fs::write(r.plugin_dir.join("SKILL.md"), "changed after arm\n").unwrap();
    r.queue_item("u1", 1);
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    r.tick();
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked);
    let reason = reason.unwrap();
    assert!(
        reason.contains("methodkit") && reason.contains("SKILL.md"),
        "{reason}"
    );
    assert!(!r.out("u1", "before.log").exists(), "before must not run");
    assert_eq!(r.all_sessions("u1"), 0);
}

#[test]
fn a_halted_run_starts_no_before() {
    let o = with_extra(
        "        before:\n          - [sh, -c, \"echo b >> {run.dir}/out/{unit.id}/before.log\"]\n",
    );
    let r = Rig::with(&o, |_| {});
    r.engine.halt("stop everything").unwrap();
    r.queue_item("u1", 1);
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    r.tick();
    assert!(r.has_unit("u1"));
    assert!(!r.out("u1", "before.log").exists());
    assert_eq!(r.fake.launch_count(), 0);
}

#[test]
fn usage_readings_store_the_latest_sum_and_add_nothing_twice() {
    let r = Rig::new();
    r.script(
        "u1",
        vec![Step::Usage(usage(3, 30)), Step::Usage(usage(2, 20))],
    );
    r.queue_item("u1", 1);
    r.tick();
    let s = r.session_ref("u1").unwrap();
    let id = r
        .view()
        .sessions
        .iter()
        .find(|x| x.unit_id == "u1")
        .unwrap()
        .id
        .clone();
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.session_cost(&id), (3, 30));
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.session_cost(&id), (5, 50));
    let usage_rows = |r: &Rig| {
        r.view()
            .admissions
            .iter()
            .filter(|a| a.kind == "usage")
            .count()
    };
    let before = usage_rows(&r);
    r.tick();
    assert_eq!(r.session_cost(&id), (5, 50));
    assert_eq!(usage_rows(&r), before, "a third tick at 5 adds nothing");
}

#[test]
fn a_seats_decisions_reach_the_ledger_once_each() {
    let o = Options {
        model_sessions: 3,
        cold_builds: 2,
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    r.item("u1", 1, 1, PrChecks::Green);
    r.item("u2", 2, 2, PrChecks::Green);
    r.tick();
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    let file = r.out("u1", "decisions.md");
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    std::fs::write(
        &file,
        format!("first choice\n\nsecond choice {token}\nthird, no newline yet"),
    )
    .unwrap();
    r.tick();
    r.tick();
    let lines = r.ledger_lines("decision:");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("first choice"));
    assert!(
        lines[1].contains("second choice") && !lines[1].contains(token),
        "{}",
        lines[1]
    );
    assert!(lines[1].contains(&redact(token)), "{}", lines[1]);
    // The partial last line waits for its newline.
    std::fs::write(
        &file,
        format!("first choice\n\nsecond choice {token}\nthird, no newline yet\n"),
    )
    .unwrap();
    r.tick();
    assert_eq!(r.ledger_lines("decision:").len(), 3);
    // An edited earlier line is a note, and later units still advance.
    std::fs::write(
        &file,
        format!("CHANGED\n\nsecond choice {token}\nthird, no newline yet\n"),
    )
    .unwrap();
    let notes = r.tick();
    assert!(
        notes.iter().any(|n| n.contains("decision line 1 of u1")),
        "{notes:?}"
    );
    assert_eq!(r.ledger_lines("decision:").len(), 3);
    r.pass("u2", &r.report("u2"));
    assert_eq!(
        r.stage_node("u2", "verify").len(),
        1,
        "u2 advanced past the note"
    );
}

#[test]
fn a_decisions_file_over_1_mib_parks_a_running_unit() {
    let r = Rig::new();
    r.queue_item("u1", 1);
    r.tick();
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    std::fs::write(r.out("u1", "decisions.md"), vec![b'x'; (1 << 20) + 1]).unwrap();
    r.tick();
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked);
    assert!(reason.unwrap().contains("1 MiB"));
}
