//! Helpers for the chat tests of `r1.s3.w3`: a recording wrapper over `FakeChat`, a strict fake, a
//! live controller with a chat runtime, and readers of the store and the ledgers. No test here
//! starts the real `claude`, spends a token, or reaches a network beyond `127.0.0.1`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use pf_adapter_fake::FakeChat;
use pf_engine::port::chat::{
    BackgroundTask, ChatAnswer, ChatError, ChatId, ChatOpen, ChatOpened, ChatOption, ChatPort,
    ContextUsage, InputKind, MidTurn,
};
use pf_engine::port::store::{ChatItemRow, Store};
use pulse_factory::controller::Config;
use serde_json::Value;

use super::ctl::Live;
use super::{BIN, code, git, stderr, stdout};

/// The runtime ID of the built-in Orchestrator.
pub const RUNTIME: &str = "claude-code";

/// A `FakeChat` that records what the controller asked of it: each `open` spec, each `set_option`,
/// and the chats it opened. It follows the port exactly, as the fake does.
pub struct Rec {
    pub fake: FakeChat,
    pub opens: Mutex<Vec<ChatOpen>>,
    pub sets: Mutex<Vec<(String, String, String)>>,
    pub chats: Mutex<Vec<ChatId>>,
    /// The process id each chat reports, for the caller check.
    pub pids: Mutex<BTreeMap<ChatId, u32>>,
    /// How many times the controller called `interrupt` (any chat).
    interrupts: AtomicUsize,
    /// A stop that waits: the next `stop` sends on `.0` when it starts, then blocks until `.1`
    /// receives (or its sender drops). `hold_next_stop` arms it.
    stop_hook: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

/// The two ends of a held stop, for the test.
pub struct StopHold {
    /// Receives once the stop has started and holds.
    pub entered: mpsc::Receiver<()>,
    /// Send (or drop) to let the stop finish.
    pub release: mpsc::Sender<()>,
}

impl Rec {
    pub fn new(fake: FakeChat) -> Arc<Rec> {
        Arc::new(Rec {
            fake,
            opens: Mutex::new(Vec::new()),
            sets: Mutex::new(Vec::new()),
            chats: Mutex::new(Vec::new()),
            pids: Mutex::new(BTreeMap::new()),
            interrupts: AtomicUsize::new(0),
            stop_hook: Mutex::new(None),
        })
    }

    /// Make the next `stop` of any chat wait until the test releases it.
    pub fn hold_next_stop(&self) -> StopHold {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        *self.stop_hook.lock().unwrap() = Some((entered_tx, release_rx));
        StopHold { entered, release }
    }

    pub fn open_count(&self) -> usize {
        self.fake.open_count()
    }

    pub fn stop_count(&self) -> usize {
        self.fake.stop_count()
    }

    /// How many `interrupt` calls the controller made.
    pub fn interrupt_count(&self) -> usize {
        self.interrupts.load(Ordering::SeqCst)
    }

    /// The texts the chat that opened `n`-th took, oldest first.
    pub fn sent(&self, n: usize) -> Vec<String> {
        let id = self.chats.lock().unwrap()[n].clone();
        self.fake.sent(&id)
    }

    pub fn last_open(&self) -> ChatOpen {
        self.opens.lock().unwrap().last().cloned().expect("an open")
    }
}

impl ChatPort for Rec {
    fn options(&self) -> Vec<ChatOption> {
        self.fake.options()
    }
    fn mid_turn(&self) -> MidTurn {
        self.fake.mid_turn()
    }
    fn open(&self, spec: &ChatOpen) -> Result<ChatOpened, ChatError> {
        self.opens.lock().unwrap().push(spec.clone());
        let opened = self.fake.open(spec)?;
        self.chats.lock().unwrap().push(opened.chat.clone());
        Ok(opened)
    }
    fn send(&self, chat: &ChatId, kind: InputKind, text: &str) -> Result<(), ChatError> {
        self.fake.send(chat, kind, text)
    }
    fn answer(&self, chat: &ChatId, request_id: &str, a: &ChatAnswer) -> Result<(), ChatError> {
        self.fake.answer(chat, request_id, a)
    }
    fn interrupt(&self, chat: &ChatId) -> Result<(), ChatError> {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        self.fake.interrupt(chat)
    }
    fn set_option(&self, chat: &ChatId, id: &str, value: &str) -> Result<(), ChatError> {
        self.sets
            .lock()
            .unwrap()
            .push((chat.0.clone(), id.to_string(), value.to_string()));
        self.fake.set_option(chat, id, value)
    }
    fn context_usage(&self, chat: &ChatId) -> Result<ContextUsage, ChatError> {
        self.fake.context_usage(chat)
    }
    fn background_tasks(&self, chat: &ChatId) -> Result<Vec<BackgroundTask>, ChatError> {
        self.fake.background_tasks(chat)
    }
    fn is_alive(&self, chat: &ChatId) -> Result<bool, ChatError> {
        self.fake.is_alive(chat)
    }
    fn stop(&self, chat: &ChatId) -> Result<(), ChatError> {
        let hook = self.stop_hook.lock().unwrap().take();
        if let Some((entered, release)) = hook {
            let _ = entered.send(());
            let _ = release.recv();
        }
        self.fake.stop(chat)
    }
    fn process_id(&self, chat: &ChatId) -> Option<u32> {
        self.pids.lock().unwrap().get(chat).copied()
    }
}

fn option(id: &str, values: &[&str], default: &str, free: bool, live: bool) -> ChatOption {
    ChatOption {
        id: id.into(),
        label: id.into(),
        values: values.iter().map(|v| v.to_string()).collect(),
        default: default.into(),
        free,
        live,
    }
}

/// The strict fake of `chat_verbs.rs`: the default fake's four options, except that `effort` is
/// not free (`low`, `medium`, `high`, `xhigh`). The seed passes the launch check, and an undeclared
/// effort is refused.
pub fn strict_fake() -> FakeChat {
    FakeChat::with_options(vec![
        option("model", &[], "fake-model", true, true),
        option(
            "effort",
            &["low", "medium", "high", "xhigh"],
            "high",
            false,
            true,
        ),
        option("permission_mode", &[], "bypass", true, true),
        option("artifacts", &["on", "off"], "on", false, false),
    ])
}

/// A live controller with `chat` under the runtime of the built-in Orchestrator.
pub fn start(chat: Arc<Rec>) -> Live {
    start_with(chat, |_| {})
}

pub fn start_with(chat: Arc<Rec>, tweak: impl FnOnce(&mut Config) + Send + 'static) -> Live {
    Live::start_config(BIN.into(), move |cfg| {
        cfg.chat_runtimes.insert(RUNTIME.to_string(), chat);
        cfg.server_name = "test-server".to_string();
        tweak(cfg);
    })
}

/// A git repo with one commit, under the sandbox.
pub fn repo(live: &Live, name: &str) -> PathBuf {
    let d = live.sb.root.path().join("work").join(name);
    std::fs::create_dir_all(&d).unwrap();
    let d = std::fs::canonicalize(&d).unwrap();
    git(&d, &["init", "-q", "-b", "main"]);
    git(
        &d,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "x",
        ],
    );
    d
}

/// `project add`, answering the new project's ID.
pub fn add_project(live: &Live, root: &Path) -> String {
    let out = live.cli(&["project", "add", &root.display().to_string(), "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    v["project"]["id"].as_str().unwrap().to_string()
}

/// Run the CLI and parse its `--json` answer. It fails the test when the verb refuses.
pub fn cli_json(live: &Live, args: &[&str]) -> Value {
    let mut all: Vec<&str> = args.to_vec();
    all.push("--json");
    let out = live.cli(&all);
    assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
    serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", stdout(&out)))
}

/// Run the CLI and answer its stderr when the verb refuses (exit 1).
pub fn cli_refused(live: &Live, args: &[&str]) -> String {
    let out = live.cli(args);
    assert_eq!(code(&out), 1, "{args:?} should refuse: {}", stdout(&out));
    stderr(&out)
}

/// A project with a repo and a live Orchestrator session: its project ID and `S-n`.
pub fn started(live: &Live, name: &str) -> (String, String) {
    let root = repo(live, name);
    let project = add_project(live, &root);
    let v = cli_json(live, &["session", "start", "--project", &project]);
    (project, v["session"]["id"].as_str().unwrap().to_string())
}

pub fn items(live: &Live, session: &str) -> Vec<ChatItemRow> {
    live.controller
        .store()
        .chat_items_after(session, 0, 10_000)
        .unwrap()
}

pub fn kinds(live: &Live, session: &str) -> Vec<String> {
    items(live, session).into_iter().map(|i| i.kind).collect()
}

pub fn count_kind(live: &Live, session: &str, kind: &str) -> usize {
    items(live, session)
        .iter()
        .filter(|i| i.kind == kind)
        .count()
}

/// Wait, with a bound, until `ok` holds. It fails with `what` and the log it saw.
pub fn wait_for(live: &Live, session: &str, what: &str, ok: impl Fn(&[ChatItemRow]) -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let seen = items(live, session);
        if ok(&seen) {
            return;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for {what}; the log holds: {:?}",
            seen.iter()
                .map(|i| format!("{}:{}", i.seq, i.kind))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn wait_turns(live: &Live, session: &str, n: usize) {
    wait_for(live, session, &format!("{n} turn results"), |items| {
        items.iter().filter(|i| i.kind == "turn_result").count() >= n
    });
}

/// The `seq` of the first item of a kind.
pub fn seq_of(live: &Live, session: &str, kind: &str) -> u64 {
    items(live, session)
        .iter()
        .find(|i| i.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} item in {session}"))
        .seq
}

pub fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

pub fn controller_ledger(live: &Live) -> Vec<String> {
    lines(&live.sb.state.join("controller.ledger"))
}

pub fn chat_ledger(live: &Live) -> Vec<String> {
    lines(&live.sb.state.join("chat.ledger"))
}

/// The lines of the controller ledger that name a command kind.
pub fn ledger_kind(live: &Live, kind: &str) -> Vec<String> {
    controller_ledger(live)
        .into_iter()
        .filter(|l| l.contains(&format!(" {kind} ")))
        .collect()
}

/// A stand-in for `/proc` in which the calling process (this test) has the given environment and
/// parent. The parent's own entry says its parent is the first process.
pub fn fake_proc(dir: &Path, environ: &[u8], parent: u32) {
    let me = dir.join(std::process::id().to_string());
    std::fs::create_dir_all(&me).unwrap();
    std::fs::write(me.join("environ"), environ).unwrap();
    std::fs::write(me.join("stat"), format!("1 (test) S {parent} 1 1 0 0\n")).unwrap();
    if parent > 1 {
        let up = dir.join(parent.to_string());
        std::fs::create_dir_all(&up).unwrap();
        std::fs::write(up.join("environ"), b"HOME=/x\0").unwrap();
        std::fs::write(up.join("stat"), "1 (chat) S 1 1 1 0 0\n").unwrap();
    }
}

/// Wait, with a bound, until the stored state of a session is `state`.
pub fn wait_state(live: &Live, session: &str, state: pf_engine::port::store::ChatSessionState) {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let row = live
            .controller
            .store()
            .chat_session(session)
            .unwrap()
            .unwrap();
        if row.state == state {
            return;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for state {state:?} of {session}; it is {:?}",
            row.state
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
