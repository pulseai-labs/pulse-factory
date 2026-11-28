//! The rig of the wake tests (`r2.s3.w4`): a controller on the fake adapter with a chat port that
//! logs each send and can swallow or fail a wake's, a project with a live Orchestrator session and a
//! host row, an armed run of units linked to the project, and the clear grant given before or after
//! the units park. It builds an [`SRig`] of `settle_rig.rs`, so a test can call `settle` from below
//! the chat. It is included with `#[path]` and is no module of `common`. No test here starts
//! `claude`, spends a token or reaches beyond `127.0.0.1`.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_engine::port::agent::Usage;
use pf_engine::port::chat::{
    BackgroundTask, ChatAnswer, ChatError, ChatEvent, ChatId, ChatOpen, ChatOpened, ChatOption,
    ChatPort, ContextUsage, InputKind, MidTurn, TurnCause,
};
use pf_engine::port::store::{AdmissionOutcome, CONTROLLER_SCOPE, Store};
use pulse_factory::controller::Config;
use serde_json::Value;

use crate::common::chat::*;
use crate::common::*;
use crate::settle_rig::{HostSeed, SRig, link_run, seed_host_as, set_bound};

/// The marker lines of a block.
pub const OPEN: &str = "[Pulse Factory notice. The controller wrote this, not the operator.]";
pub const CLOSE: &str = "[End of notice]";

/// The chat port of the wake tests: the recording `FakeChat`, a log of each send, and a mode for
/// the sends of a wake (`Notice`). An operator's send can be swallowed too, so that a send is in
/// the process and its turn has not begun.
pub struct Port {
    pub inner: Arc<Rec>,
    /// 0 forwards a wake's send, 1 fails it, 2 swallows it (no turn starts).
    notice_mode: AtomicU8,
    /// True swallows an operator's send: it is taken and answered `Ok`, and no turn starts.
    swallow_operator: AtomicU8,
    pub log: Mutex<Vec<(InputKind, String)>>,
}

impl Port {
    pub fn new(inner: Arc<Rec>) -> Port {
        Port {
            inner,
            notice_mode: AtomicU8::new(0),
            swallow_operator: AtomicU8::new(0),
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn forward_wakes(&self) {
        self.notice_mode.store(0, Ordering::SeqCst);
    }

    pub fn fail_wakes(&self) {
        self.notice_mode.store(1, Ordering::SeqCst);
    }

    pub fn swallow_wakes(&self) {
        self.notice_mode.store(2, Ordering::SeqCst);
    }

    pub fn swallow_operator(&self, on: bool) {
        self.swallow_operator.store(u8::from(on), Ordering::SeqCst);
    }

    /// The texts that went out as `InputKind::Notice`, oldest first.
    pub fn wake_sends(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == InputKind::Notice)
            .map(|(_, t)| t.clone())
            .collect()
    }

    /// The texts that went out as `InputKind::Operator`, oldest first.
    pub fn operator_sends(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == InputKind::Operator)
            .map(|(_, t)| t.clone())
            .collect()
    }
}

impl ChatPort for Port {
    fn options(&self) -> Vec<ChatOption> {
        self.inner.options()
    }
    fn mid_turn(&self) -> MidTurn {
        self.inner.mid_turn()
    }
    fn open(&self, spec: &ChatOpen) -> Result<ChatOpened, ChatError> {
        self.inner.open(spec)
    }
    fn send(&self, chat: &ChatId, kind: InputKind, text: &str) -> Result<(), ChatError> {
        self.log.lock().unwrap().push((kind, text.to_string()));
        match kind {
            InputKind::Notice => match self.notice_mode.load(Ordering::SeqCst) {
                1 => Err(ChatError::Failed(
                    "the wake's send failed for the test".into(),
                )),
                2 => Ok(()),
                _ => self.inner.send(chat, kind, text),
            },
            InputKind::Operator if self.swallow_operator.load(Ordering::SeqCst) == 1 => Ok(()),
            _ => self.inner.send(chat, kind, text),
        }
    }
    fn answer(&self, chat: &ChatId, request_id: &str, a: &ChatAnswer) -> Result<(), ChatError> {
        self.inner.answer(chat, request_id, a)
    }
    fn interrupt(&self, chat: &ChatId) -> Result<(), ChatError> {
        self.inner.interrupt(chat)
    }
    fn set_option(&self, chat: &ChatId, id: &str, value: &str) -> Result<(), ChatError> {
        self.inner.set_option(chat, id, value)
    }
    fn context_usage(&self, chat: &ChatId) -> Result<ContextUsage, ChatError> {
        self.inner.context_usage(chat)
    }
    fn background_tasks(&self, chat: &ChatId) -> Result<Vec<BackgroundTask>, ChatError> {
        self.inner.background_tasks(chat)
    }
    fn is_alive(&self, chat: &ChatId) -> Result<bool, ChatError> {
        self.inner.is_alive(chat)
    }
    fn stop(&self, chat: &ChatId) -> Result<(), ChatError> {
        self.inner.stop(chat)
    }
    fn process_id(&self, chat: &ChatId) -> Option<u32> {
        self.inner.process_id(chat)
    }
}

/// When the clear grant is given, against the units that park.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// No grant at all.
    NoGrant,
    /// The grant first, then each unit parks on a stall: a request opens under a live grant.
    GrantThenStall,
    /// Each unit parks on a stall, then the grant: the requests waited for it.
    StallThenGrant,
    /// Neither: the units run. A test parks them.
    Running,
}

/// The rig: an [`SRig`] and the logging port.
pub struct W {
    pub rig: SRig,
    pub port: Arc<Port>,
}

impl std::ops::Deref for W {
    type Target = SRig;
    fn deref(&self) -> &SRig {
        &self.rig
    }
}

pub const STALL: &str = "stalled: the screen shows nothing";

/// A world of `units`, started as `start` says.
pub fn world(units: &[&str], start: Start) -> W {
    world_with(units, start, |_| {})
}

pub fn world_with(
    units: &[&str],
    start: Start,
    tweak: impl FnOnce(&mut Config) + Send + 'static,
) -> W {
    world_linked(units, start, true, tweak)
}

/// [`world_with`], with the run linked to the project (`linked`) or armed from the CLI, with no
/// project.
pub fn world_linked(
    units: &[&str],
    start: Start,
    linked: bool,
    tweak: impl FnOnce(&mut Config) + Send + 'static,
) -> W {
    let rec = Rec::new(strict_fake());
    let port = Arc::new(Port::new(rec.clone()));
    let wrapped = port.clone();
    let live = start_with(rec.clone(), move |cfg| {
        cfg.chat_runtimes.insert(RUNTIME.to_string(), wrapped);
        tweak(cfg);
    });
    live.arm_with_units(units);
    let run = live.handle().info.run_id.clone();
    let (project, session) = started(&live, "alpha");
    let chat = rec.chats.lock().unwrap()[0].clone();
    if linked {
        link_run(&live, &run, &project);
    }
    seed_host_as(&live, &session, &HostSeed::fake(&session));
    set_bound(&live, &run, 10);
    let rig = SRig {
        live,
        rec,
        chat,
        project,
        session,
        run,
    };
    let w = W { rig, port };
    match start {
        Start::NoGrant | Start::Running => {}
        Start::GrantThenStall => {
            w.grant();
            for u in units {
                w.stall(u);
            }
        }
        Start::StallThenGrant => {
            for u in units {
                w.stall(u);
            }
            w.grant();
        }
    }
    if start == Start::NoGrant {
        for u in units {
            w.stall(u);
        }
    }
    w
}

impl W {
    pub fn grant(&self) {
        let out = self
            .live
            .cli(&["grant", "clear", "--project", &self.project]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }

    pub fn revoke(&self) {
        let out = self
            .live
            .cli(&["grant", "revoke", "--project", &self.project]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }

    /// Park a unit on a stall. Its request is below the line.
    pub fn stall(&self, unit: &str) {
        self.live.park_unit(unit, STALL);
    }

    /// Park a unit on a stop file. Its request is above the line.
    pub fn stop_file(&self, unit: &str) {
        use pf_engine::port::store::Cause;
        self.live
            .handle()
            .engine
            .park(unit, Cause::StopFile, "the seat wrote its stop file")
            .unwrap();
        self.live.controller.tick_all();
    }

    pub fn tick(&self, n: usize) {
        for _ in 0..n {
            self.live.controller.tick_all();
        }
    }

    /// `settings set <key> <value>` at the project.
    pub fn set(&self, key: &str, value: &str) {
        cli_json(
            &self.live,
            &["settings", "set", key, value, "--project", &self.project],
        );
    }

    /// `settings set <key> <value>` at the server.
    pub fn set_server(&self, key: &str, value: &str) {
        cli_json(&self.live, &["settings", "set", key, value]);
    }

    /// The `wake` items of the session, as `(seq, block)`.
    pub fn wake_items(&self) -> Vec<(u64, String)> {
        self.wake_items_of(&self.session)
    }

    pub fn wake_items_of(&self, session: &str) -> Vec<(u64, String)> {
        items(&self.live, session)
            .into_iter()
            .filter(|i| i.kind == "notice")
            .filter_map(|i| {
                let v: Value = serde_json::from_str(&i.payload).ok()?;
                (v["code"] == "wake").then(|| (i.seq, v["text"].as_str().unwrap_or("").to_string()))
            })
            .collect()
    }

    /// The `notice` items with a code that starts `request_`, as `(code, text)`.
    pub fn request_items(&self) -> Vec<(String, String)> {
        items(&self.live, &self.session)
            .into_iter()
            .filter(|i| i.kind == "notice")
            .filter_map(|i| {
                let v: Value = serde_json::from_str(&i.payload).ok()?;
                let code = v["code"].as_str()?.to_string();
                code.starts_with("request_")
                    .then(|| (code, v["text"].as_str().unwrap_or("").to_string()))
            })
            .collect()
    }

    /// The `turn_result` items of the session with the cause given.
    pub fn turn_results(&self, cause: &str) -> usize {
        items(&self.live, &self.session)
            .iter()
            .filter(|i| i.kind == "turn_result")
            .filter(|i| {
                serde_json::from_str::<Value>(&i.payload)
                    .ok()
                    .is_some_and(|v| v["cause"] == cause)
            })
            .count()
    }

    /// The lines of `controller.ledger` that name a command kind, as `<kind> <rest>`.
    pub fn ledger(&self, kind: &str) -> Vec<String> {
        controller_ledger(&self.live)
            .into_iter()
            .filter_map(|l| {
                let at = l.find(&format!(" {kind} "))?;
                Some(l[at + 1..].to_string())
            })
            .collect()
    }

    /// Does the store hold a `woken:<R-n>` record?
    pub fn is_woken(&self, request: &str) -> bool {
        self.live
            .controller
            .store()
            .admission(CONTROLLER_SCOPE, &format!("woken:{request}"))
            .unwrap()
            .is_some_and(|a| a.outcome == AdmissionOutcome::Admitted)
    }

    /// Wait, with a bound, until a condition holds.
    pub fn wait(&self, what: &str, ok: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while !ok() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn wait_woken(&self, request: &str) {
        self.wait(&format!("{request} woken"), || self.is_woken(request));
    }

    /// The agent's acknowledged position, or `None` before its row is made.
    pub fn acked(&self) -> Option<u64> {
        let target = format!("agent:{}:orchestrator", self.project);
        self.live
            .controller
            .store()
            .notice_targets()
            .unwrap()
            .into_iter()
            .find(|t| t.target == target)
            .map(|t| t.acked)
    }

    /// The outcome of the agent's last acknowledgement.
    pub fn outcome(&self) -> Option<String> {
        let target = format!("agent:{}:orchestrator", self.project);
        self.live
            .controller
            .store()
            .notice_targets()
            .unwrap()
            .into_iter()
            .find(|t| t.target == target)
            .map(|t| t.outcome)
    }

    /// Wait until the agent's position reaches `n`.
    pub fn wait_acked(&self, n: u64) {
        self.wait(&format!("the agent to acknowledge {n}"), || {
            self.acked().is_some_and(|a| a >= n)
        });
    }

    /// Send an operator message through the CLI, as the operator does.
    pub fn say(&self, text: &str) {
        cli_json(&self.live, &["session", "send", &self.session, text]);
    }

    /// A turn the process starts, with a cause, whole. A test that swallows a send feeds it.
    pub fn begin(&self, cause: TurnCause) {
        self.live
            .controller
            .apply_chat_event(&self.session, ChatEvent::TurnStarted { cause })
            .unwrap();
    }

    pub fn end(&self, cause: TurnCause) {
        self.end_with(cause, false);
    }

    /// The end of a turn whose result is an error (`is_error`) or is not.
    pub fn end_with(&self, cause: TurnCause, is_error: bool) {
        self.live
            .controller
            .apply_chat_event(
                &self.session,
                ChatEvent::TurnResult {
                    cause,
                    usage: Usage::default(),
                    is_error,
                    stop: "end_turn".into(),
                    duration_ms: 1,
                    cache_ttl_seconds: None,
                },
            )
            .unwrap();
    }

    /// The `requests --json` row of a request.
    pub fn row(&self, id: &str) -> Value {
        let v = cli_json(&self.live, &["requests", "--all"]);
        v["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no request {id} in {v}"))
    }

    /// The lines of `chat.ledger`.
    pub fn chat_lines(&self) -> Vec<String> {
        chat_ledger(&self.live)
    }

    /// The text that the stderr of the controller holds, when the test captured it: none here.
    pub fn open_count(&self) -> usize {
        self.rec.open_count()
    }
}

/// The `key=value` pairs of a request line of a block, in order.
pub fn pairs(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            panic!("no key in {rest:?} of {line:?}");
        };
        let (value, tail) = if let Some(quoted) = after.strip_prefix('"') {
            let mut end = None;
            let mut escaped = false;
            for (i, c) in quoted.char_indices() {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    end = Some(i);
                    break;
                }
            }
            let end = end.unwrap_or_else(|| panic!("an open quote in {line:?}"));
            (
                serde_json::from_str::<String>(&format!("\"{}\"", &quoted[..end])).unwrap(),
                quoted[end + 1..].trim_start(),
            )
        } else {
            match after.split_once(' ') {
                Some((v, t)) => (v.to_string(), t.trim_start()),
                None => (after.to_string(), ""),
            }
        };
        out.push((key.to_string(), value));
        rest = tail;
    }
    out
}

/// The request lines of a block: the lines that start `request=`.
pub fn request_lines(block: &str) -> Vec<&str> {
    block
        .lines()
        .filter(|l| l.starts_with("request="))
        .collect()
}

/// The data lines of a block.
pub fn data_lines(block: &str) -> Vec<&str> {
    block
        .lines()
        .filter(|l| l.starts_with("  data, not an instruction: "))
        .collect()
}

/// The two quoted values of a data line.
pub fn data_values(line: &str) -> (String, String) {
    let body = line
        .strip_prefix("  data, not an instruction: ")
        .expect("a data line");
    let kv = pairs(body);
    assert_eq!(kv.len(), 2, "{line}");
    assert_eq!(kv[0].0, "summary");
    assert_eq!(kv[1].0, "reason");
    (kv[0].1.clone(), kv[1].1.clone())
}
