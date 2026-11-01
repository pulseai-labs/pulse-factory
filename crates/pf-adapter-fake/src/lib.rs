//! `pf-adapter-fake`: a scripted agent adapter behind the same port as a real one. It starts no
//! process and uses no network, so it is the default for every test (`seat-launch-spend`).
//!
//! A scenario is chosen by unit ID. A launch fails as the scenario says, or it gives a session
//! that follows a script. The script advances only when the test says so (`advance`, `finish`),
//! so a test sees each state between two steps.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{
    AgentError, AgentPort, Dialog, DialogAnswer, LaunchPlan, ResolvedPlugin, RuleSet, SessionEvent,
    SessionRef, SessionState, SessionStatus, Usage,
};
use pf_engine::status::{REASON_MAX_CHARS, one_line_reason, redact};

/// One step of a script.
#[derive(Clone, Debug)]
pub enum Step {
    /// The turn ends and no other follows. The session is idle.
    TurnEnd,
    /// The session spent this much. It adds to the usage so far.
    Usage(Usage),
    /// The seat pushed. The event gets a fresh ID.
    Push {
        branch: String,
        sha: String,
        outcome: PushOutcome,
    },
    /// The session did something.
    Activity,
    /// The session showed no activity within its limit.
    Stalled(String),
    /// A push left an unread target and the default branch moved.
    DefaultBranchMoved {
        branch: String,
        before: String,
        after: String,
    },
}

/// What a launch for one unit does.
#[derive(Clone, Debug)]
pub enum Scenario {
    /// A session that follows the script.
    Script(Vec<Step>),
    /// The adapter refuses the launch (for example an unqualified version).
    RefusedLaunch(String),
    /// The launch reaches no first turn. `screen` is what the session showed.
    NoFirstTurn { screen: String },
    /// The session shows a dialog that the adapter does not know.
    UnknownDialog(Dialog),
}

struct Fake {
    script: Vec<Step>,
    next_step: usize,
    state: SessionState,
    turn_ended: bool,
    usage: Usage,
    queue: Vec<SessionEvent>,
    delivered: Vec<String>,
    alive: bool,
}

#[derive(Default)]
struct State {
    scenarios: HashMap<String, Scenario>,
    sessions: HashMap<String, Fake>,
    known_dialogs: BTreeMap<String, String>,
    plugins: Vec<ResolvedPlugin>,
    launches: usize,
    interrupts: usize,
    push_seq: u64,
}

/// The fake adapter.
#[derive(Default)]
pub struct FakeAdapter {
    state: Mutex<State>,
}

fn lock(m: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FakeAdapter {
    pub fn new() -> FakeAdapter {
        FakeAdapter::default()
    }

    /// Set the scenario for a unit. A unit with none gets an empty script.
    pub fn script(&self, unit_id: &str, scenario: Scenario) {
        lock(&self.state)
            .scenarios
            .insert(unit_id.to_string(), scenario);
    }

    /// Make the fake answer a dialog kind.
    pub fn know_dialog(&self, kind: &str, answer: &str) {
        lock(&self.state)
            .known_dialogs
            .insert(kind.to_string(), answer.to_string());
    }

    /// Make a plugin findable.
    pub fn add_plugin(&self, name: &str, version: &str, path: &str) {
        lock(&self.state).plugins.push(ResolvedPlugin {
            name: name.to_string(),
            version: version.to_string(),
            path: path.to_string(),
        });
    }

    /// Apply the next `n` steps of a session's script.
    pub fn advance(&self, session: &SessionRef, n: usize) {
        let mut st = lock(&self.state);
        let mut seq = st.push_seq;
        if let Some(f) = st.sessions.get_mut(&session.0) {
            for _ in 0..n {
                let Some(step) = f.script.get(f.next_step).cloned() else {
                    break;
                };
                f.next_step += 1;
                apply(f, step, &session.0, &mut seq);
            }
        }
        st.push_seq = seq;
    }

    /// Apply every step that is left.
    pub fn finish(&self, session: &SessionRef) {
        self.advance(session, usize::MAX);
    }

    /// The messages delivered to a session.
    pub fn delivered(&self, session: &SessionRef) -> Vec<String> {
        lock(&self.state)
            .sessions
            .get(&session.0)
            .map(|f| f.delivered.clone())
            .unwrap_or_default()
    }

    /// How many sessions started. A failed launch does not count.
    pub fn launch_count(&self) -> usize {
        lock(&self.state).launches
    }

    pub fn interrupt_count(&self) -> usize {
        lock(&self.state).interrupts
    }
}

fn apply(f: &mut Fake, step: Step, session: &str, seq: &mut u64) {
    match step {
        Step::TurnEnd => {
            f.turn_ended = true;
            f.state = SessionState::Idle;
            f.queue.push(SessionEvent::Activity);
        }
        Step::Usage(u) => {
            f.usage.input_tokens += u.input_tokens;
            f.usage.output_tokens += u.output_tokens;
            f.usage.cache_read_tokens += u.cache_read_tokens;
            f.usage.cache_write_tokens += u.cache_write_tokens;
            f.usage.cost_micros += u.cost_micros;
        }
        Step::Push {
            branch,
            sha,
            outcome,
        } => {
            *seq += 1;
            f.queue.push(SessionEvent::Push {
                id: format!("{session}-push-{seq}"),
                repo: "fake/repo".to_string(),
                branch,
                sha,
                outcome,
            });
        }
        Step::Activity => f.queue.push(SessionEvent::Activity),
        Step::Stalled(tail) => f.queue.push(SessionEvent::Stalled {
            screen_tail: redact(&tail),
        }),
        Step::DefaultBranchMoved {
            branch,
            before,
            after,
        } => f.queue.push(SessionEvent::DefaultBranchMoved {
            remote: "origin".to_string(),
            branch,
            before,
            after,
        }),
    }
}

fn no_session(session: &SessionRef) -> AgentError {
    AgentError::Failed(format!("session {} is unknown", session.0))
}

impl AgentPort for FakeAdapter {
    fn launch(&self, plan: &LaunchPlan) -> Result<SessionRef, AgentError> {
        let mut st = lock(&self.state);
        let scenario = st
            .scenarios
            .get(&plan.unit_id)
            .cloned()
            .unwrap_or(Scenario::Script(Vec::new()));
        let script = match scenario {
            Scenario::RefusedLaunch(why) => return Err(AgentError::Refused(why)),
            Scenario::NoFirstTurn { screen } => {
                // The same shape as a real adapter: the screen is data, redacted and one line.
                // The engine adds `launch failed: ` and cuts a reason at REASON_MAX_CHARS, and a
                // cut takes the end of the screen. So keep the end, and fit the whole reason.
                let head = "no first turn in the time given; screen: ";
                let flat = redact(&screen)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let room = REASON_MAX_CHARS
                    .saturating_sub("launch failed: ".len() + head.len())
                    .min(450);
                let skip = flat.chars().count().saturating_sub(room);
                let tail: String = flat.chars().skip(skip).collect();
                return Err(AgentError::Failed(format!("{head}{tail}")));
            }
            Scenario::UnknownDialog(d) => {
                let text: String = one_line_reason(&d.text).chars().take(300).collect();
                return Err(AgentError::Failed(format!(
                    "unknown dialog ({}) at launch: {text}",
                    d.kind
                )));
            }
            Scenario::Script(steps) => steps,
        };
        st.launches += 1;
        let id = format!("fake-{}", st.launches);
        st.sessions.insert(
            id.clone(),
            Fake {
                script,
                next_step: 0,
                state: SessionState::Working,
                turn_ended: false,
                usage: Usage::default(),
                queue: Vec::new(),
                delivered: Vec::new(),
                alive: true,
            },
        );
        Ok(SessionRef(id))
    }

    fn status(&self, session: &SessionRef) -> Result<SessionStatus, AgentError> {
        let st = lock(&self.state);
        let f = st
            .sessions
            .get(&session.0)
            .ok_or_else(|| no_session(session))?;
        Ok(SessionStatus {
            state: if f.alive {
                f.state
            } else {
                SessionState::Ended
            },
            turn_ended: f.turn_ended,
            first_turn_started: true,
        })
    }

    fn deliver(&self, session: &SessionRef, message: &str) -> Result<(), AgentError> {
        let mut st = lock(&self.state);
        let f = st
            .sessions
            .get_mut(&session.0)
            .ok_or_else(|| no_session(session))?;
        if f.state != SessionState::Idle {
            return Err(AgentError::Failed(
                "the session is not idle; a message reaches an idle session only".to_string(),
            ));
        }
        f.delivered.push(message.to_string());
        f.state = SessionState::Working;
        f.turn_ended = false;
        Ok(())
    }

    fn interrupt(&self, session: &SessionRef) -> Result<(), AgentError> {
        let mut st = lock(&self.state);
        st.interrupts += 1;
        let f = st
            .sessions
            .get_mut(&session.0)
            .ok_or_else(|| no_session(session))?;
        f.state = SessionState::Idle;
        f.turn_ended = true;
        Ok(())
    }

    fn usage(&self, session: &SessionRef) -> Result<Usage, AgentError> {
        let st = lock(&self.state);
        st.sessions
            .get(&session.0)
            .map(|f| f.usage)
            .ok_or_else(|| no_session(session))
    }

    fn answer_dialog(
        &self,
        session: &SessionRef,
        dialog: &Dialog,
    ) -> Result<DialogAnswer, AgentError> {
        let st = lock(&self.state);
        if !st.sessions.contains_key(&session.0) {
            return Err(no_session(session));
        }
        Ok(st
            .known_dialogs
            .get(&dialog.kind)
            .map_or(DialogAnswer::Unknown, |a| DialogAnswer::Answered(a.clone())))
    }

    fn apply_rules(&self, plan: &mut LaunchPlan, rules: &RuleSet) -> Result<(), AgentError> {
        if !rules.prompts_off {
            return Err(AgentError::Refused(
                "a seat runs with permission prompts off".to_string(),
            ));
        }
        plan.settings
            .insert("rule_set".to_string(), rules.name.clone());
        Ok(())
    }

    fn remove_ask_tool(&self, plan: &mut LaunchPlan) -> Result<(), AgentError> {
        plan.settings
            .insert("ask_tool".to_string(), "off".to_string());
        Ok(())
    }

    fn hand_off(
        &self,
        session: &SessionRef,
        successor: &LaunchPlan,
    ) -> Result<SessionRef, AgentError> {
        let next = self.launch(successor)?;
        if let Some(f) = lock(&self.state).sessions.get_mut(&session.0) {
            f.alive = false;
        }
        Ok(next)
    }

    fn is_alive(&self, session: &SessionRef) -> Result<bool, AgentError> {
        let st = lock(&self.state);
        st.sessions
            .get(&session.0)
            .map(|f| f.alive)
            .ok_or_else(|| no_session(session))
    }

    fn resolve_plugin(&self, name: &str, min_version: &str) -> Result<ResolvedPlugin, AgentError> {
        let st = lock(&self.state);
        let found: Vec<&ResolvedPlugin> = st.plugins.iter().filter(|p| p.name == name).collect();
        let at_least = |v: &str| {
            let parse =
                |s: &str| -> Vec<u64> { s.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
            parse(v) >= parse(min_version)
        };
        match found.iter().find(|p| at_least(&p.version)) {
            Some(p) => Ok((*p).clone()),
            None if found.is_empty() => {
                Err(AgentError::Refused(format!("plugin {name} was not found")))
            }
            None => Err(AgentError::Refused(format!(
                "plugin {name} is below the minimum {min_version}"
            ))),
        }
    }

    fn drain_events(&self, session: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        let mut st = lock(&self.state);
        let f = st
            .sessions
            .get_mut(&session.0)
            .ok_or_else(|| no_session(session))?;
        Ok(std::mem::take(&mut f.queue))
    }
}
