//! AC-1 of `r1.s4.w1`: a chat process, and anything below it, is refused on the operator's verbs
//! (`arm`, `claim`, `answer`, `clear`, `ack`, `device_pair`, `device_revoke`); `halt` stays open.
//! A chat runs with `PULSE_FACTORY_CHAT=<session id>` in its environment, so a child it detaches from
//! its process tree is refused too, unless it strips the variable (a speed bump, not a boundary).
//!
//! The controller runs in the test process with a recording `FakeChat`. The chat process is a real
//! shell whose PID the test registers in `Rec.pids`. The CLI, or this test binary as a helper for
//! the verbs the CLI cannot send bare, runs below that shell.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::chat::*;
use common::ctl::Live;
use common::*;
use pf_engine::port::chat::ChatId;
use pf_engine::port::store::RequestStatus;
use pf_ipc::{Handler, Request};
use pulse_factory::controller::chats::parse_chat_env;
use serde_json::Value;

const MARKER: &str = "PULSE_FACTORY_CHAT";

/// How the chat shell starts the process that calls the controller.
#[derive(Clone, Copy, PartialEq)]
enum Style {
    /// A plain child.
    Direct,
    /// `( ( cmd ) & )`: reparented to the nearest subreaper.
    DoubleFork,
    /// `setsid -f cmd`.
    SetsidF,
    /// `( nohup cmd & )` from a subshell that exits.
    Nohup,
    /// `setsid cmd` with no fork: it keeps its parent.
    Setsid,
    /// `env -u PULSE_FACTORY_CHAT` and a double fork.
    StripDoubleFork,
    /// `env -u PULSE_FACTORY_CHAT setsid cmd` with no fork.
    StripSetsid,
}

impl Style {
    fn name(self) -> &'static str {
        match self {
            Style::Direct => "direct",
            Style::DoubleFork => "dfork",
            Style::SetsidF => "setsidf",
            Style::Nohup => "nohup",
            Style::Setsid => "setsid",
            Style::StripDoubleFork => "stripdfork",
            Style::StripSetsid => "stripsetsid",
        }
    }
}

/// The inner script: it records its own parent chain, waits until the first parent is gone, then
/// runs the command with its output in files. A script file keeps the quoting out of the shells.
const INNER: &str = r#"sleep 0.3
p=$$
chain=""
while [ "$p" -gt 1 ] 2>/dev/null; do
  chain="$chain $p"
  p=$(sed 's/^.*) //' "/proc/$p/stat" | cut -d' ' -f2)
done
echo "$chain" >"$DIR/chain"
"$CMD" "$@" >"$DIR/out" 2>"$DIR/err"
echo $? >"$DIR/rc"
"#;

const OUTER: &str = r#"while [ ! -e "$GO" ]; do sleep 0.02; done
case "$STYLE" in
  direct) sh "$DIR/inner.sh" "$@" ;;
  dfork) ( ( sh "$DIR/inner.sh" "$@" ) & ) ;;
  setsidf) setsid -f sh "$DIR/inner.sh" "$@" ;;
  nohup) ( nohup sh "$DIR/inner.sh" "$@" >/dev/null 2>&1 & ) ;;
  setsid) setsid sh "$DIR/inner.sh" "$@" ;;
  stripdfork) ( ( env -u PULSE_FACTORY_CHAT sh "$DIR/inner.sh" "$@" ) & ) ;;
  stripsetsid) env -u PULSE_FACTORY_CHAT setsid sh "$DIR/inner.sh" "$@" ;;
esac
i=0
while [ ! -e "$DIR/rc" ] && [ "$i" -lt 750 ]; do sleep 0.02; i=$((i+1)); done
"#;

static RUNS: AtomicUsize = AtomicUsize::new(0);

struct Ran {
    rc: i32,
    out: String,
    err: String,
    /// The parent chain of the process that called, from itself up to (not including) PID 1.
    chain: Vec<u32>,
    shell: u32,
}

impl Ran {
    /// The controller's answer, when the process was the helper.
    fn response(&self) -> Value {
        let line = self
            .out
            .lines()
            .find_map(|l| l.split_once("HELPER_RESPONSE:").map(|(_, rest)| rest))
            .unwrap_or_else(|| panic!("no response from the helper: {}\n{}", self.out, self.err));
        serde_json::from_str(line).unwrap()
    }
}

struct Rig {
    live: Live,
    rec: Arc<Rec>,
    chat: ChatId,
    request: String,
    rev: u32,
    run: String,
}

/// A controller with an armed run, a parked unit `u1` and its open request, and a live session `S-1`.
fn rig() -> Rig {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), |cfg| {
        // A server-side setting that names the marker does not change it.
        cfg.chat_agent_env.insert(
            "ORCHESTRATOR".to_string(),
            [(MARKER.to_string(), "forged".to_string())].into(),
        );
    });
    live.arm_with_units(&["u1"]);
    live.park_unit("u1", "the reviewer found a defect");
    let (_, session) = started(&live, "alpha");
    assert_eq!(session, "S-1");
    let open = live
        .handle()
        .engine
        .view()
        .unwrap()
        .requests
        .iter()
        .find(|r| r.status == RequestStatus::Open && r.unit_id.as_deref() == Some("u1"))
        .expect("an open request for u1")
        .clone();
    let chat = rec.chats.lock().unwrap()[0].clone();
    let run = live.handle().info.run_id.clone();
    Rig {
        live,
        rec,
        chat,
        request: open.id,
        rev: open.revision,
        run,
    }
}

impl Rig {
    fn claims(&self) -> usize {
        self.live.handle().engine.view().unwrap().claims.len()
    }

    fn open_requests(&self) -> usize {
        self.live
            .handle()
            .engine
            .view()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.status == RequestStatus::Open)
            .count()
    }

    fn devices(&self) -> usize {
        let out = cli_json(&self.live, &["device", "list"]);
        out["devices"].as_array().map_or(0, Vec::len)
    }

    /// Run `cmd args` below a chat shell. `registered` puts the shell's PID in `Rec.pids` as the
    /// chat's process; `marker` puts `PULSE_FACTORY_CHAT=S-1` in the shell's environment, as the
    /// adapter does for a chat.
    fn below(
        &self,
        style: Style,
        registered: bool,
        marker: bool,
        cmd: &str,
        args: &[&str],
        extra: &[(&str, String)],
    ) -> Ran {
        let n = RUNS.fetch_add(1, Ordering::SeqCst);
        let dir: PathBuf = self.live.sb.root.path().join(format!("below-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inner.sh"), INNER).unwrap();
        let go = dir.join("go");
        let mut shell = Command::new("/bin/sh");
        shell
            .arg("-c")
            .arg(OUTER)
            .arg("sh")
            .args(args)
            .env_clear()
            .env("PATH", self.live.sb.path_env())
            .env("HOME", self.live.sb.root.path())
            .env("PF_STATE_DIR", &self.live.sb.state)
            .env("PF_SHARE_DIR", &self.live.sb.share)
            .env("GO", &go)
            .env("DIR", &dir)
            .env("CMD", cmd)
            .env("STYLE", style.name());
        if marker {
            shell.env(MARKER, "S-1");
        }
        for (k, v) in extra {
            shell.env(k, v);
        }
        let mut child = shell.spawn().unwrap();
        if registered {
            self.rec
                .pids
                .lock()
                .unwrap()
                .insert(self.chat.clone(), child.id());
        }
        std::fs::write(&go, "").unwrap();
        child.wait().unwrap();
        self.rec.pids.lock().unwrap().clear();
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .unwrap_or_else(|e| panic!("{name} of run {n} ({}): {e}", style.name()))
        };
        Ran {
            rc: read("rc").trim().parse().unwrap(),
            out: read("out"),
            err: read("err"),
            chain: read("chain")
                .split_whitespace()
                .map(|p| p.parse().unwrap())
                .collect(),
            shell: child.id(),
        }
    }

    /// The CLI below a chat shell.
    fn cli_below(&self, style: Style, registered: bool, marker: bool, args: &[&str]) -> Ran {
        self.below(style, registered, marker, BIN, args, &[])
    }

    /// A raw verb call below a chat shell, by this test binary as the helper.
    fn call_below(
        &self,
        style: Style,
        registered: bool,
        marker: bool,
        verb: &str,
        args: &Value,
    ) -> Ran {
        let exe = std::env::current_exe().unwrap();
        self.below(
            style,
            registered,
            marker,
            exe.to_str().unwrap(),
            &[
                "--exact",
                "helper_calls_the_controller",
                "--nocapture",
                "--test-threads=1",
            ],
            &[
                (
                    "PF_TEST_HELPER_STATE",
                    self.live.sb.state.display().to_string(),
                ),
                ("PF_TEST_HELPER_VERB", verb.to_string()),
                ("PF_TEST_HELPER_ARGS", args.to_string()),
            ],
        )
    }
}

/// Not a test: the process that `call_below` starts. Run on its own it does nothing.
#[test]
fn helper_calls_the_controller() {
    let Ok(state) = std::env::var("PF_TEST_HELPER_STATE") else {
        return;
    };
    let verb = std::env::var("PF_TEST_HELPER_VERB").unwrap();
    let args: Value = serde_json::from_str(&std::env::var("PF_TEST_HELPER_ARGS").unwrap()).unwrap();
    let resp = pf_ipc::call(Path::new(&state), &verb, args).unwrap();
    println!("HELPER_RESPONSE:{}", serde_json::to_string(&resp).unwrap());
}

fn refused_by_ancestry(ran: &Ran, verb: &str) {
    assert_eq!(ran.rc, 1, "{verb}: {}", ran.err);
    assert!(
        ran.err.contains(&format!("{verb} refused"))
            && ran.err.contains("descends from, chat process"),
        "{verb}: {}",
        ran.err
    );
}

// ------------------------------------------------------------------ the process tree

#[test]
fn the_operators_verbs_are_refused_below_a_chat_process() {
    let rig = rig();
    let rev = rig.rev.to_string();
    // The control, first: a shell that is not a chat claims. It leaves the token the answer needs.
    let control = rig.cli_below(
        Style::Direct,
        false,
        false,
        &["claim", "--holder", "ann@host"],
    );
    assert_eq!(control.rc, 0, "{}", control.err);
    assert_eq!(rig.claims(), 1);
    let devices_before = rig.devices();
    let runs_before = rig.live.controller.handles().len();

    let cli_verbs: Vec<(&str, Vec<&str>)> = vec![
        ("claim", vec!["claim", "--holder", "chat"]),
        (
            "answer",
            vec!["answer", &rig.request, "--rev", &rev, "--text", "go on"],
        ),
        ("clear", vec!["clear", &rig.run, "u1"]),
        ("ack", vec!["ack", &rig.request, "--rev", &rev]),
        ("device_pair", vec!["device", "pair"]),
        ("device_revoke", vec!["device", "revoke", "D-1"]),
    ];
    for (verb, args) in &cli_verbs {
        let ran = rig.cli_below(Style::Direct, true, false, args);
        refused_by_ancestry(&ran, verb);
    }
    let plan = serde_json::to_value(rig.live.plan()).unwrap();
    let ran = rig.call_below(Style::Direct, true, false, "arm", &plan);
    let resp = ran.response();
    assert_eq!(resp["ok"], false, "{resp}");
    let why = resp["error"].as_str().unwrap();
    assert!(
        why.contains("arm refused") && why.contains("descends from, chat process"),
        "{why}"
    );

    assert_eq!(rig.claims(), 1, "no claim was added");
    assert_eq!(rig.open_requests(), 1, "nothing was answered");
    assert_eq!(rig.live.unit_status("u1"), "parked", "nothing was cleared");
    assert_eq!(
        rig.devices(),
        devices_before,
        "no device was added or revoked"
    );
    assert_eq!(
        rig.live.controller.handles().len(),
        runs_before,
        "no run was armed"
    );

    // The same calls from a shell that is not a chat pass their caller check.
    let ran = rig.cli_below(
        Style::Direct,
        false,
        false,
        &["ack", &rig.request, "--rev", &rev],
    );
    assert!(
        !ran.err.contains("descends from") && !ran.err.contains(MARKER),
        "the control was refused by the caller check: {}",
        ran.err
    );
    // The API is off in this rig, so a pair that passes the check ends there.
    let ran = rig.cli_below(Style::Direct, false, false, &["device", "pair"]);
    assert!(
        !ran.err.contains("descends from") && !ran.err.contains(MARKER),
        "{}",
        ran.err
    );
    let ran = rig.call_below(Style::Direct, false, false, "arm", &plan);
    let why = ran.response()["error"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !why.contains("descends from") && !why.contains("cannot tell"),
        "{why}"
    );
    let ran = rig.cli_below(
        Style::Direct,
        false,
        false,
        &["answer", &rig.request, "--rev", &rev, "--text", "go on"],
    );
    assert_eq!(ran.rc, 0, "{}", ran.err);
    assert_eq!(rig.live.unit_status("u1"), "running");
}

#[test]
fn halt_stays_open_to_a_chat_and_stops_it() {
    let rig = rig();
    let ran = rig.cli_below(
        Style::Direct,
        true,
        true,
        &["halt", "--reason", "stop now", "--json"],
    );
    assert_eq!(ran.rc, 0, "{}", ran.err);
    let v: Value = serde_json::from_str(&ran.out).unwrap();
    assert_eq!(v["halted"], true, "{v}");
    assert!(
        v["chats_stopped"].to_string().contains("S-1"),
        "chats_stopped names S-1: {v}"
    );
    let view = rig.live.handle().engine.view().unwrap();
    assert!(
        view.run.unwrap().halted_reason.is_some(),
        "the run is halted"
    );
}

#[test]
fn arm_through_the_plain_handler_is_refused_for_want_of_a_caller() {
    let rig = rig();
    let plan = serde_json::to_value(rig.live.plan()).unwrap();
    let resp = rig.live.controller.handle(Request::new("arm", plan));
    assert!(!resp.ok);
    let why = resp.error.unwrap();
    assert!(
        why.contains("arm refused") && why.contains("cannot tell which process called"),
        "{why}"
    );
    assert_eq!(rig.live.controller.handles().len(), 1, "no run was armed");
}

// ------------------------------------------------------------------ the marker

#[test]
fn a_session_opens_and_resumes_with_the_marker_set_last() {
    let rig = rig();
    let first = rig.rec.opens.lock().unwrap()[0].clone();
    assert_eq!(
        first.env.get(MARKER).map(String::as_str),
        Some("S-1"),
        "a setting that names the marker does not replace it"
    );
    // A resume: a message, a non-live option, and the next send reopens the idle process.
    cli_json(&rig.live, &["session", "send", "S-1", "first"]);
    wait_turns(&rig.live, "S-1", 1);
    cli_json(
        &rig.live,
        &["session", "set", "S-1", "--option", "artifacts=off"],
    );
    cli_json(&rig.live, &["session", "send", "S-1", "again"]);
    wait_turns(&rig.live, "S-1", 2);
    let reopen = rig.rec.last_open();
    assert!(reopen.resume.is_some(), "the second open is a resume");
    assert_eq!(reopen.env.get(MARKER).map(String::as_str), Some("S-1"));
}

#[test]
fn a_setting_that_names_the_marker_is_dropped_with_one_warning() {
    let vars: BTreeMap<String, String> = [(
        "PF_AGENT_ENV_ORCHESTRATOR".to_string(),
        "EXAMPLE_ROLE=orchestrator PULSE_FACTORY_CHAT=forged".to_string(),
    )]
    .into();
    let (settings, warnings) = parse_chat_env(&vars, Some("host"));
    let pairs = &settings.agent_env["ORCHESTRATOR"];
    assert_eq!(
        pairs.get("EXAMPLE_ROLE").map(String::as_str),
        Some("orchestrator")
    );
    assert!(!pairs.contains_key(MARKER), "{pairs:?}");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains(MARKER), "{warnings:?}");
}

#[test]
fn a_child_that_keeps_the_marker_is_refused_even_when_detached() {
    let rig = rig();
    let rev = rig.rev.to_string();
    let control = rig.cli_below(
        Style::Direct,
        false,
        false,
        &["claim", "--holder", "ann@host"],
    );
    assert_eq!(control.rc, 0, "{}", control.err);

    let claim = ["claim", "--holder", "chat"];
    let send = ["session", "send", "S-1", "hello"];
    let answer = ["answer", &rig.request, "--rev", &rev, "--text", "go on"];
    let cases: Vec<(Style, &[&str])> = vec![
        (Style::Direct, &claim),
        (Style::DoubleFork, &claim),
        (Style::SetsidF, &claim),
        (Style::Nohup, &claim),
        (Style::DoubleFork, &send),
        (Style::DoubleFork, &answer),
    ];
    for (style, args) in cases {
        let ran = rig.cli_below(style, true, true, args);
        assert_eq!(ran.rc, 1, "{} {args:?}: {}", style.name(), ran.err);
        assert!(
            ran.err.contains("refused") && ran.err.contains(MARKER),
            "{} {args:?}: {}",
            style.name(),
            ran.err
        );
        if style != Style::Direct {
            // The marker refused it, not the walk: no chat PID is on the parent chain.
            assert!(
                !ran.chain.contains(&ran.shell),
                "{}: the chat shell {} is on the chain {:?}, so the walk could have refused",
                style.name(),
                ran.shell,
                ran.chain
            );
        }
    }
    assert_eq!(rig.claims(), 1, "no claim was added");
    assert_eq!(rig.open_requests(), 1, "nothing was answered");
    assert!(
        rig.rec.sent(0).is_empty(),
        "no message reached the chat: {:?}",
        rig.rec.sent(0)
    );
}

#[test]
fn the_ancestry_refuses_a_setsid_child_that_keeps_its_parent_even_with_the_marker_stripped() {
    let rig = rig();
    let control = rig.cli_below(
        Style::Direct,
        false,
        false,
        &["claim", "--holder", "ann@host"],
    );
    assert_eq!(control.rc, 0, "{}", control.err);
    for style in [Style::StripSetsid, Style::Setsid] {
        let ran = rig.cli_below(
            style,
            true,
            style == Style::Setsid,
            &["claim", "--holder", "chat"],
        );
        if style == Style::StripSetsid {
            refused_by_ancestry(&ran, "claim");
            assert!(ran.chain.contains(&ran.shell), "{:?}", ran.chain);
        } else {
            assert_eq!(ran.rc, 1, "{}", ran.err);
        }
    }
    assert_eq!(rig.claims(), 1);
}

/// KNOWN LIMIT, recorded as it is and not as wanted behaviour (R15). A chat that strips
/// `PULSE_FACTORY_CHAT` (`env -u`) and detaches (a double fork) is granted. The marker and the
/// ancestry walk are a speed bump against a cooperating or accidental chat, of the same class as
/// Q4's deny list and the seat's `PF_SEAT_TOKEN`. Residue the Release 2 authority-rules spine owns:
/// `env -u`, `env -i`, `systemd-run --user` (documented, not run: a test cannot rely on a user
/// manager), `at` and `cron` jobs that do not inherit the environment, keystrokes into an operator
/// terminal, and a same-uid rewrite of `/proc/<pid>/environ`. `docs/contracts/requests.md`, "The
/// caller check", names the limit.
#[test]
fn known_limit_a_chat_that_strips_the_marker_and_detaches_is_granted() {
    let rig = rig();
    let ran = rig.cli_below(
        Style::StripDoubleFork,
        true,
        true,
        &["claim", "--holder", "stripped-chat"],
    );
    assert_eq!(ran.rc, 0, "the known limit changed: {}", ran.err);
    assert!(!ran.chain.contains(&ran.shell), "{:?}", ran.chain);
    assert_eq!(rig.claims(), 1, "the claim rows show the granted claim");
}
