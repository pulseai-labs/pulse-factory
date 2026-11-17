//! AC-3: a stage that declares `no_signal` gets a nudge when its seat ends a turn with no done
//! signal, and its `run` commands run before the unit parks. The driver runs on a `FakeAdapter`
//! and a `BareForge`. Nothing here starts a seat process, spends or reaches the network.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::rig::*;
use common::*;
use pf_adapter_fake::{FakeAdapter, Step};
use pf_engine::admission::PushOutcome;
use pf_engine::claim::sha256_hex;
use pf_engine::port::agent::*;
use pf_engine::port::store::{RequestKind, RequestStatus};
use pf_engine::status::Status;
use pf_forge_bare::PrChecks;
use serde_json::Value;

const UNIT: &str = "u1";
const NUDGE_TEXT: &str = "Finish u1: open the PR.";
const GENERIC: &str = "seat ended with no done signal";

/// The `no_signal` block of the stage `close`.
fn block(nudges: u32, run: &str) -> String {
    format!(
        "        no_signal:\n          nudge: \"Finish {{unit.id}}: open the PR.\"\n          nudges: {nudges}\n          run:\n{run}"
    )
}

/// A command that writes the stop file, as the shipped package's explainer will.
const WRITES_STOP: &str = "            - [mkdir, -p, \"{run.dir}/out/{unit.id}\"]\n            - [sh, -c, \"echo stranded-{unit.id} > {run.dir}/out/{unit.id}/stop\"]\n";
const WRITES_NOTHING: &str = "            - [true]\n";

/// The stop file of `close`, with a hook that leaves a mark when an answer comes.
const STOP_HOOK: &str = "        stop_file:\n          file: \"{run.dir}/out/{unit.id}/stop\"\n          on_answer:\n            - [touch, \"{run.dir}/hook-ran-{answer.request}\"]\n";

fn options(nudges: u32, run: &str) -> Options {
    Options {
        close_extra: block(nudges, run),
        close_stop: STOP_HOOK.to_string(),
        ..Options::default()
    }
}

/// Queue the unit with a PR, give its sessions `turns` turn ends and then the seat's push, run it
/// to its close stage and return the close session. The implement and verify sessions finish
/// their whole script at once. The close session applies a step each time the test says so.
fn at_close(r: Rig, turns: usize) -> (Rig, SessionRef) {
    let head = r.item(UNIT, 1, 1, PrChecks::Green);
    let mut script = vec![Step::TurnEnd; turns];
    script.push(Step::Push {
        branch: format!("work/{UNIT}"),
        sha: head,
        outcome: PushOutcome::Pushed,
    });
    r.script(UNIT, script);
    r.reach_close(UNIT);
    let s = r.session_ref(UNIT).expect("the close session is open");
    (r, s)
}

fn nudge_lines(r: &Rig) -> Vec<String> {
    r.ledger_lines(" nudge ")
}

fn read_request(r: &Rig) -> (RequestKind, String, Value) {
    let q = r
        .view()
        .open_request_of(UNIT)
        .cloned()
        .unwrap_or_else(|| panic!("no open request: {:?}", r.view().requests));
    let body: Value = serde_json::from_str(&q.body).unwrap();
    (q.kind, q.reason, body)
}

#[test]
fn one_nudge_is_delivered_and_recorded_and_the_next_turn_opens_the_pr() {
    let (r, s) = at_close(Rig::with(&options(1, WRITES_STOP), |_| {}), 2);
    r.fake.advance(&s, 1);
    let notes = r.tick();
    assert!(notes.is_empty(), "{notes:?}");

    // The same session got the rendered message, and the unit still runs.
    assert_eq!(r.fake.delivered(&s), vec![NUDGE_TEXT.to_string()]);
    assert_eq!(r.unit(UNIT).0, Status::Running);
    assert_eq!(r.open_sessions(UNIT), 1, "the same session goes on");
    assert_eq!(r.stage_node(UNIT, "close").len(), 1, "no new stage attempt");

    // One ledger line: run, unit, stage, role, model, session and the count.
    let lines = nudge_lines(&r);
    assert_eq!(lines.len(), 1, "{}", r.ledger_text());
    let row = r
        .view()
        .sessions
        .iter()
        .rfind(|x| x.unit_id == UNIT && !x.ended)
        .cloned()
        .unwrap();
    for part in [
        " r1 ",
        UNIT,
        "stage close",
        "role close",
        "model model-d",
        &format!("session {}", row.id),
        "nudge 1 of 1",
    ] {
        assert!(lines[0].contains(part), "`{part}` is not in: {}", lines[0]);
    }

    // The seat works again: the tick that follows delivers nothing more.
    r.tick();
    assert_eq!(r.fake.delivered(&s).len(), 1);
    assert_eq!(nudge_lines(&r).len(), 1);

    // The next turn opens the PR: the unit is done and merged.
    r.close(UNIT, 1);
    assert_eq!(r.unit(UNIT).0, Status::Done, "ledger: {}", r.ledger_text());
    assert_eq!(r.fake.delivered(&s).len(), 1);
    assert!(r.view().open_request_of(UNIT).is_none());
    assert!(!r.out(UNIT, "stop").exists(), "no run command ran");
}

#[test]
fn a_seat_that_ends_twice_gets_one_nudge_then_the_run_command_makes_a_stop_request() {
    let (r, s) = at_close(Rig::with(&options(1, WRITES_STOP), |_| {}), 2);
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.fake.delivered(&s).len(), 1);

    // The turn ends again with no done signal: the command runs, then the unit parks.
    r.fake.advance(&s, 1);
    let notes = r.tick();
    assert_eq!(r.unit(UNIT).0, Status::Parked, "{notes:?}");
    assert_eq!(r.fake.delivered(&s).len(), 1, "no second nudge");
    assert_eq!(nudge_lines(&r).len(), 1);
    assert_eq!(r.open_sessions(UNIT), 0, "the session ended with the park");

    let (kind, reason, body) = read_request(&r);
    assert_eq!(kind, RequestKind::Stop);
    assert!(reason.contains("stranded-u1"), "{reason}");
    assert!(
        body["stop_text"].as_str().unwrap().contains("stranded-u1"),
        "{body}"
    );

    // The request carries the stop file's answer hook: an answer runs it.
    let generation = r
        .engine
        .claim("ann@host", &sha256_hex(&[7u8; 32]))
        .unwrap()
        .generation;
    let id = r.view().open_request_of(UNIT).unwrap().id.clone();
    let answered = r
        .driver
        .answer_request(&id, 1, generation, "Open the PR by hand.")
        .unwrap();
    assert!(
        answered.hook_ran && answered.hook_failed.is_none(),
        "{answered:?}"
    );
    assert!(
        r.run_dir.join(format!("hook-ran-{id}")).exists(),
        "the stop file's on_answer hook ran"
    );
    let request = r
        .view()
        .requests
        .iter()
        .find(|q| q.id == id)
        .cloned()
        .unwrap();
    assert_eq!(request.status, RequestStatus::Answered);
}

#[test]
fn a_run_command_that_writes_nothing_parks_as_today() {
    let (r, s) = at_close(Rig::with(&options(1, WRITES_NOTHING), |_| {}), 2);
    r.fake.advance(&s, 1);
    r.tick();
    r.fake.advance(&s, 1);
    let notes = r.tick();
    let (status, reason) = r.unit(UNIT);
    assert_eq!(status, Status::Parked, "{notes:?}");
    assert_eq!(reason.as_deref(), Some(GENERIC));
    let (kind, reason, _) = read_request(&r);
    assert_eq!((kind, reason.as_str()), (RequestKind::Park, GENERIC));
    assert!(!r.out(UNIT, "stop").exists());
}

#[test]
fn nudges_are_counted_per_session_up_to_the_cap() {
    let (r, s) = at_close(Rig::with(&options(2, WRITES_STOP), |_| {}), 3);
    for n in 1..=2 {
        r.fake.advance(&s, 1);
        r.tick();
        assert_eq!(r.fake.delivered(&s).len(), n);
        assert_eq!(r.unit(UNIT).0, Status::Running);
    }
    let lines = nudge_lines(&r);
    assert_eq!(lines.len(), 2, "{}", r.ledger_text());
    assert!(lines[0].contains("nudge 1 of 2") && lines[1].contains("nudge 2 of 2"));

    // The third turn end finds the nudges spent: the command runs and the unit parks.
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.fake.delivered(&s).len(), 2);
    assert_eq!(r.unit(UNIT).0, Status::Parked);
    assert_eq!(read_request(&r).0, RequestKind::Stop);
}

#[test]
fn a_stage_that_declares_no_run_command_parks_with_the_generic_reason_once_the_nudges_are_spent() {
    let o = Options {
        close_extra: "        no_signal:\n          nudge: \"Finish {unit.id}: open the PR.\"\n          nudges: 1\n"
            .to_string(),
        ..Options::default()
    };
    let (r, s) = at_close(Rig::with(&o, |_| {}), 2);
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.fake.delivered(&s).len(), 1);
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.unit(UNIT).1.as_deref(), Some(GENERIC));
    assert_eq!(r.unit(UNIT).0, Status::Parked);
}

type DeliverFn = Box<dyn Fn(&SessionRef, &str) -> Result<(), AgentError> + Send + Sync>;

/// The fake adapter, except that `deliver` does what the test says.
struct OnDeliver {
    inner: Arc<FakeAdapter>,
    deliver: DeliverFn,
}

/// The fake adapter, except that a message never reaches the session.
fn no_deliver(fake: Arc<FakeAdapter>) -> Arc<dyn AgentPort> {
    Arc::new(OnDeliver {
        inner: fake,
        deliver: Box::new(|_, _| Err(AgentError::Failed("the session has ended".to_string()))),
    })
}

impl AgentPort for OnDeliver {
    fn launch(&self, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.inner.launch(p)
    }
    fn status(&self, s: &SessionRef) -> Result<SessionStatus, AgentError> {
        self.inner.status(s)
    }
    fn deliver(&self, s: &SessionRef, m: &str) -> Result<(), AgentError> {
        (self.deliver)(s, m)
    }
    fn interrupt(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.interrupt(s)
    }
    fn usage(&self, s: &SessionRef) -> Result<Usage, AgentError> {
        self.inner.usage(s)
    }
    fn answer_dialog(&self, s: &SessionRef, d: &Dialog) -> Result<DialogAnswer, AgentError> {
        self.inner.answer_dialog(s, d)
    }
    fn apply_rules(&self, p: &mut LaunchPlan, r: &RuleSet) -> Result<(), AgentError> {
        self.inner.apply_rules(p, r)
    }
    fn remove_ask_tool(&self, p: &mut LaunchPlan) -> Result<(), AgentError> {
        self.inner.remove_ask_tool(p)
    }
    fn hand_off(&self, s: &SessionRef, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.inner.hand_off(s, p)
    }
    fn is_alive(&self, s: &SessionRef) -> Result<bool, AgentError> {
        self.inner.is_alive(s)
    }
    fn stop(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.stop(s)
    }
    fn resolve_plugin(&self, n: &str, v: &str) -> Result<ResolvedPlugin, AgentError> {
        self.inner.resolve_plugin(n, v)
    }
    fn drain_events(&self, s: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        self.inner.drain_events(s)
    }
}

#[test]
fn a_failed_delivery_writes_a_note_and_goes_straight_to_the_run_command() {
    let rig = Rig::build(&options(2, WRITES_STOP), |_| {}, no_deliver);
    let (r, s) = at_close(rig, 2);
    r.fake.advance(&s, 1);
    let notes = r.tick();

    // No nudge was counted or recorded, and the command ran in the same tick.
    assert!(nudge_lines(&r).is_empty(), "{}", r.ledger_text());
    assert!(
        notes.iter().any(|n| n.contains(UNIT)
            && n.contains("nudge")
            && n.contains("the session has ended")),
        "{notes:?}"
    );
    assert_eq!(r.unit(UNIT).0, Status::Parked, "{notes:?}");
    let (kind, _, body) = read_request(&r);
    assert_eq!(kind, RequestKind::Stop);
    assert!(body["stop_text"].as_str().unwrap().contains("stranded-u1"));
}

#[test]
fn under_halt_no_nudge_is_delivered() {
    let (r, s) = at_close(Rig::with(&options(2, WRITES_STOP), |_| {}), 2);
    r.engine.halt("the operator halted the run").unwrap();
    r.fake.advance(&s, 1);
    let notes = r.tick();
    assert!(r.fake.delivered(&s).is_empty(), "{notes:?}");
    assert!(nudge_lines(&r).is_empty(), "{}", r.ledger_text());
    assert!(
        notes.iter().any(|n| n.contains(UNIT) && n.contains("halt")),
        "the skip is noted: {notes:?}"
    );
    // The unit parks as it would have with no nudge declared, with the command's explanation.
    assert_eq!(r.unit(UNIT).0, Status::Parked, "{notes:?}");
    assert_eq!(read_request(&r).0, RequestKind::Stop);
}

#[test]
fn a_failing_run_command_parks_as_today_and_is_named_in_a_note() {
    let (r, s) = at_close(Rig::with(&options(1, "            - [false]\n"), |_| {}), 2);
    r.fake.advance(&s, 1);
    r.tick();
    r.fake.advance(&s, 1);
    let notes = r.tick();
    assert!(
        notes
            .iter()
            .any(|n| n.contains(UNIT) && n.contains("command 1") && n.contains("(false)")),
        "{notes:?}"
    );
    let (status, reason) = r.unit(UNIT);
    assert_eq!((status, reason.as_deref()), (Status::Parked, Some(GENERIC)));
    assert_eq!(read_request(&r).0, RequestKind::Park);
}

#[test]
fn a_run_command_stops_at_its_timeout_and_the_park_does_not_wait_past_it() {
    let run = "            - argv: [sleep, \"30\"]\n              timeout_seconds: 1\n";
    let (r, s) = at_close(Rig::with(&options(1, run), |_| {}), 2);
    r.fake.advance(&s, 1);
    r.tick();
    r.fake.advance(&s, 1);
    let started = Instant::now();
    let notes = r.tick();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the park waited {:?}",
        started.elapsed()
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("command 1") && n.contains("timed out")),
        "{notes:?}"
    );
    assert_eq!(r.unit(UNIT).1.as_deref(), Some(GENERIC));
}

#[test]
fn a_command_that_wrote_the_stop_file_before_it_failed_still_parks_on_the_stop_file() {
    let run = "            - [mkdir, -p, \"{run.dir}/out/{unit.id}\"]\n            - [sh, -c, \"echo half-written > {run.dir}/out/{unit.id}/stop; exit 3\"]\n";
    let (r, s) = at_close(Rig::with(&options(1, run), |_| {}), 2);
    r.fake.advance(&s, 1);
    r.tick();
    r.fake.advance(&s, 1);
    let notes = r.tick();
    assert!(notes.iter().any(|n| n.contains("command 2")), "{notes:?}");
    assert_eq!(read_request(&r).0, RequestKind::Stop);
}

#[test]
fn a_stage_without_the_key_parks_as_it_did_before() {
    let (r, s) = at_close(Rig::new(), 1);
    r.fake.advance(&s, 1);
    r.tick();
    assert!(r.fake.delivered(&s).is_empty());
    let (status, reason) = r.unit(UNIT);
    assert_eq!((status, reason.as_deref()), (Status::Parked, Some(GENERIC)));
    assert_eq!(read_request(&r).0, RequestKind::Park);
    assert!(nudge_lines(&r).is_empty());
}

#[test]
fn a_seats_own_stop_file_wins_over_the_nudge() {
    let (r, s) = at_close(Rig::with(&options(2, WRITES_NOTHING), |_| {}), 2);
    r.touch(&r.out(UNIT, "stop"));
    std::fs::write(r.out(UNIT, "stop"), "I need a ruling.").unwrap();
    r.fake.advance(&s, 1);
    r.tick();
    assert!(
        r.fake.delivered(&s).is_empty(),
        "no nudge after a stop file"
    );
    assert_eq!(r.unit(UNIT).0, Status::Parked);
    let (kind, _, body) = read_request(&r);
    assert_eq!(kind, RequestKind::Stop);
    assert_eq!(body["stop_text"], "I need a ruling.");
}

#[test]
fn a_relaunch_after_a_stall_starts_a_new_count() {
    let (r, s) = at_close(Rig::with(&options(1, WRITES_STOP), |_| {}), 2);
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.fake.delivered(&s).len(), 1);
    // The nudged seat then does nothing: the stall rule relaunches the stage once.
    r.tick();
    r.advance(21 * 60);
    r.tick();
    let attempts = r.stage_node(UNIT, "close");
    assert_eq!(attempts.len(), 2, "{attempts:?}");
    assert_eq!(r.unit(UNIT).0, Status::Running);

    // The new session has its own count: it is nudged once more.
    let s2 = r.session_ref(UNIT).expect("the relaunched session");
    assert_ne!(s2.0, s.0);
    r.fake.advance(&s2, 1);
    r.tick();
    assert_eq!(r.fake.delivered(&s2), vec![NUDGE_TEXT.to_string()]);
    assert_eq!(r.fake.delivered(&s).len(), 1);
    assert_eq!(nudge_lines(&r).len(), 2, "{}", r.ledger_text());
    assert!(nudge_lines(&r)[1].contains("nudge 1 of 1"));
}

#[test]
fn a_nudge_delivered_late_in_a_tick_restarts_the_stall_clock() {
    // Once armed, each delivery moves the driver's clock 30 minutes on, as a tick that ran long
    // would: past the close stage's 20-minute stall limit.
    let clock: Arc<OnceLock<Arc<AtomicU64>>> = Arc::default();
    let armed = clock.clone();
    let rig = Rig::build(
        &options(1, WRITES_STOP),
        |_| {},
        move |fake| {
            let inner = fake.clone();
            Arc::new(OnDeliver {
                inner: fake,
                deliver: Box::new(move |s, m| {
                    inner.deliver(s, m)?;
                    if let Some(now) = armed.get() {
                        now.fetch_add(30 * 60, Ordering::SeqCst);
                    }
                    Ok(())
                }),
            })
        },
    );
    let (r, s) = at_close(rig, 2);
    clock.set(r.now.clone()).unwrap();
    r.fake.advance(&s, 1);
    r.tick();
    assert_eq!(r.fake.delivered(&s).len(), 1);

    // The seat answers the nudge between two samples: its next turn opens the PR and ends. The
    // stall rule must not take the seat that just answered.
    r.close(UNIT, 1);
    let attempts = r.stage_node(UNIT, "close");
    assert_eq!(
        attempts.len(),
        1,
        "the stall rule relaunched a seat that had just answered its nudge: {attempts:?}"
    );
    assert_eq!(r.unit(UNIT).0, Status::Done, "ledger: {}", r.ledger_text());
}
