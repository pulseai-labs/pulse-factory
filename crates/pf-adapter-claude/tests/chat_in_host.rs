//! `ClaudeChat` in a session host (r2.s2.w5; ADR-0014's amendment of 2026-11-16, G3, G4, G8): the
//! chat runs the replay in a pipes host with the allowlisted environment, the launch record holds
//! what a resume needs and no secret, each delivery carries its cursor and a checkpoint at a quiet
//! point, a second adapter rebinds the same host and reads the turn again, the stop ends the child
//! and the host, and an exit that was acknowledged ends the host at once.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::scene::{self, Line};
use common::*;
use pf_adapter_claude::decode::Decoder;
use pf_adapter_claude::pinned;
use pf_adapter_claude::sha256::hash_file;
use pf_adapter_claude::{ClaudeChat, ClaudeConfig};
use pf_engine::port::agent::SessionRef;
use pf_engine::port::chat::{
    ChatDelivery, ChatError, ChatEvent, ChatEvents, ChatPort, ChatRebind, ChatResumePoint,
    InputKind, PendingKind, PendingRequest, RecordedBinary, TurnCause, check_order,
};
use pf_engine::port::host::SessionHosts;
use serde_json::{Value, json};

/// A claude stand-in that dumps its environment, writes its pid, and runs the replay on `fixture`
/// with `extra` flags. Every line the adapter writes goes to `<dir>/stdin` first.
fn wrapper(dir: &Path, fixture: &Path, extra: &str) -> PathBuf {
    let body = format!(
        "#!/bin/sh\nenv > '{d}/env'\necho $$ > '{d}/pid'\ntee -a '{d}/stdin' | '{r}' --fixture '{f}' {extra} \"$@\"\n",
        d = dir.display(),
        r = REPLAY,
        f = fixture.display(),
    );
    let bin = dir.join("claude-host");
    std::fs::write(&bin, body).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

struct Setup {
    _root: tempfile::TempDir,
    dir: PathBuf,
    work: tempfile::TempDir,
    bin: PathBuf,
    cfg: ClaudeConfig,
}

fn setup(fixture: &Path, extra: &str, inherited: BTreeMap<String, String>) -> Setup {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("records");
    std::fs::create_dir_all(&dir).unwrap();
    let bin = wrapper(&dir, fixture, extra);
    let mut cfg = ClaudeConfig::new(root.path().join("share"));
    cfg.claude_bin_override = Some(bin.clone());
    cfg.config_dir = root.path().join("claude-config");
    cfg.init_seconds = 15;
    cfg.control_seconds = 5;
    cfg.inherited_env = Some(inherited);
    Setup {
        _root: root,
        dir,
        work: tempfile::tempdir().unwrap(),
        bin,
        cfg,
    }
}

fn deliveries(
    events: &mut ChatEvents,
    secs: u64,
    done: impl Fn(&[ChatDelivery]) -> bool,
) -> Vec<ChatDelivery> {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut got = Vec::new();
    loop {
        while let Ok(d) = events.try_recv() {
            got.push(d);
        }
        if done(&got) {
            return got;
        }
        assert!(
            Instant::now() < end,
            "the deliveries did not come in {secs} s: {got:#?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn events_of(d: &[ChatDelivery]) -> Vec<ChatEvent> {
    d.iter().map(|x| x.event.clone()).collect()
}

fn has_result(d: &[ChatDelivery]) -> bool {
    d.iter()
        .any(|x| matches!(x.event, ChatEvent::TurnResult { .. }))
}

fn plain_turn() -> PathBuf {
    fixtures_dir().join("plain_turn.jsonl")
}

fn lines(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("stdin"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn open_starts_the_replay_in_a_pipes_host_with_the_allowlisted_environment_and_no_other_variable() {
    let inherited: BTreeMap<String, String> = [
        ("HOME", "/h"),
        ("PATH", "/usr/bin:/bin"),
        ("ANTHROPIC_API_KEY", "leak"),
        ("PF_SEAT_TOKEN", "leak"),
        ("SOME_TOKEN", "leak"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let s = setup(&plain_turn(), "", inherited);
    let c = hosted_with(s.cfg.clone());
    let mut spec = spec(s.work.path());
    spec.env.insert("AGENT_VAR".into(), "1".into());
    spec.secret_env
        .insert("MY_SECRET".into(), "s3cr3t-value".into());
    let opened = c.open(&spec).expect("open");
    let host = opened.host.clone().expect("a chat in a host names it");
    assert_eq!(host.incarnation, 1);
    let env: BTreeMap<String, String> = std::fs::read_to_string(s.dir.join("env"))
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut names: Vec<&str> = env
        .keys()
        .map(String::as_str)
        .filter(|k| !["PWD", "SHLVL", "_", "OLDPWD"].contains(k))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "AGENT_VAR",
            "CLAUDE_CODE_ARTIFACT",
            "DISABLE_AUTOUPDATER",
            "HOME",
            "MY_SECRET",
            "PATH"
        ],
        "the allowlist, the agent's map, the secret and the two adapter names"
    );
    assert_eq!(
        env["MY_SECRET"], "s3cr3t-value",
        "the child gets the real value"
    );
    // The host runs the child on pipes, and its pid is the host's hello pid.
    let record = c.hosts.launched_record(&host.unit).unwrap().unwrap();
    assert_eq!(record["transport"]["kind"], "pipes");
    assert_eq!(c.process_id(&opened.chat), Some(read_pid(&s.dir)));
    c.stop(&opened.chat).unwrap();
}

fn read_pid(dir: &Path) -> u32 {
    std::fs::read_to_string(dir.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn the_record_body_holds_the_binary_the_settings_the_binding_and_the_form_and_no_secret() {
    let s = setup(&plain_turn(), "", BTreeMap::new());
    let c = hosted_with(s.cfg.clone());
    let mut spec = spec(s.work.path());
    spec.secret_env
        .insert("MY_SECRET".into(), "s3cr3t-value".into());
    spec.deny_commands = vec!["git push -f*".into()];
    spec.deny_from = "project".into();
    spec.options.insert("artifacts".into(), "off".into());
    let opened = c.open(&spec).expect("open");
    let unit = opened.host.clone().unwrap().unit;
    let record = c.hosts.launched_record(&unit).unwrap().unwrap();
    let text = record.to_string();
    assert!(
        !text.contains("s3cr3t-value"),
        "no secret value anywhere: {text}"
    );
    assert_eq!(record["runtime"]["id"], "claude-code");
    assert_eq!(
        record["runtime"]["binary_path"],
        s.bin.display().to_string()
    );
    assert_eq!(
        record["runtime"]["binary_sha256"],
        hash_file(&s.bin).unwrap(),
        "the override binary is hashed too"
    );
    assert_eq!(record["runtime"]["version"], opened.runtime_version);
    assert_eq!(record["runtime"]["qualified"], false);
    let st = &record["settings"];
    assert_eq!(st["model"], "claude-opus-5-5");
    assert_eq!(st["effort"], "high");
    assert_eq!(st["permission_mode"], "bypassPermissions");
    assert_eq!(st["options"], json!({"artifacts": "off"}));
    assert_eq!(st["instructions"], "be brief");
    assert_eq!(
        st["chat.deny_commands"],
        json!({"value": ["git push -f*"], "scope": "project"})
    );
    assert_eq!(
        record["run_binding"],
        json!({"chat_session": "S-1", "project_id": "P-1", "agent_id": "orchestrator", "worktree_id": "W-1"})
    );
    assert_eq!(
        record["resume"],
        json!({"flag": "--resume", "id_from": "session_row"})
    );
    // The argv is this launch's: the binary, then the command line with the session id it used.
    let want: Vec<String> = std::iter::once(s.bin.display().to_string())
        .chain(pf_adapter_claude::launch::argv(&spec, &opened.session.0))
        .collect();
    assert_eq!(record["argv"], json!(want));
    assert!(record["argv"].to_string().contains("--session-id"));
    // The secret has its name and a `sha256:` stand-in, no value.
    assert_eq!(record["secret_names"], json!(["MY_SECRET"]));
    assert!(
        record["env"]["MY_SECRET"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    c.stop(&opened.chat).unwrap();
}

#[test]
fn deliveries_carry_the_cursor_and_a_checkpoint_only_at_a_quiet_point() {
    let s = setup(&plain_turn(), "", BTreeMap::new());
    let c = hosted_with(s.cfg.clone());
    let mut opened = c.open(&spec(s.work.path())).expect("open");
    c.send(&opened.chat, InputKind::Operator, "one").unwrap();
    let got = deliveries(&mut opened.events, 15, has_result);
    assert!(got.len() >= 4, "{got:#?}");
    let mut last = 0;
    for d in &got {
        let at = d.at.expect("a hosted chat gives each delivery its cursor");
        assert!(
            at > last,
            "the cursor grows with each event: {at} after {last}"
        );
        last = at;
    }
    for d in &got {
        let quiet = matches!(d.event, ChatEvent::TurnResult { .. });
        assert_eq!(
            d.checkpoint.is_some(),
            quiet,
            "a checkpoint only where nothing is in flight: {:?}",
            d.event
        );
    }
    let result = got.last().unwrap();
    let cp = result.checkpoint.as_deref().unwrap();
    assert!(Decoder::restore("2.1.319", false, cp).is_some(), "{cp}");
    let v: Value = serde_json::from_str(cp).unwrap();
    assert!(v["totals"]["cost_micros"].as_u64().unwrap() > 0, "{cp}");
    assert_eq!(v["seen_result"], true);
    check_order(&events_of(&got)).unwrap();
    c.stop(&opened.chat).unwrap();
}

/// A scene: a reply, then an inbound control request the adapter refuses, then the end of turn.
fn hook_scene(dir: &Path) -> PathBuf {
    let mut body: Vec<Line> = scene::open();
    body.push(scene::user("u1", "go"));
    body.push(scene::sys_init(&["u1"]));
    body.push(scene::delta("hi"));
    body.push((
        "host",
        json!({"type": "control_request", "request_id": "h1", "request": {"subtype": "hook_callback"}}),
    ));
    body.push(scene::text("hi"));
    body.push(scene::result(0.5, 10, 20, &["u1"]));
    write_fixture(dir, "hook_scene", &body)
}

#[test]
fn a_second_adapter_rebinds_the_same_unit_and_reads_the_turn_again_with_no_second_response() {
    let root = tempfile::tempdir().unwrap();
    let fixture = hook_scene(root.path());
    let gate = root.path().join("gate-result");
    let s = setup(
        &fixture,
        &format!("--gate result:{}", gate.display()),
        BTreeMap::new(),
    );
    let first = hosted_with(s.cfg.clone());
    let mut opened = first.open(&spec(s.work.path())).expect("open");
    let unit = opened.host.clone().unwrap().unit;
    first.send(&opened.chat, InputKind::Operator, "go").unwrap();
    // The turn runs to the gate: TurnStarted, the delta, the text. The hook was refused once.
    let before = deliveries(&mut opened.events, 15, |d| {
        d.iter().any(|x| matches!(x.event, ChatEvent::Text { .. }))
    });
    let refusals = |dir: &Path| {
        lines(dir)
            .iter()
            .filter(|l| l["response"]["request_id"] == "h1")
            .count()
    };
    assert_eq!(
        refusals(&s.dir),
        1,
        "the first adapter refused the hook once"
    );
    let committed = before.last().unwrap().at.unwrap();
    // The first adapter is gone (the controller died): it drops its connection, stops nothing.
    first.detach(&opened.chat).unwrap();
    assert!(first.hosts.is_live(&unit), "the host and its child live on");
    let pid = read_pid(&s.dir);
    assert!(!is_gone(pid));

    // A second adapter over the same hosts rebinds from the start of the output.
    let second = ClaudeChat::new(s.cfg.clone());
    second.set_hosts(first.hosts.clone());
    let point = ChatResumePoint {
        runtime_session: opened.session.clone(),
        acked: 0,
        committed,
        checkpoint: None,
        pending: Vec::new(),
    };
    let ChatRebind::Survived(mut again) = second.rebind(&unit, &point) else {
        panic!("the host survived");
    };
    assert_eq!(again.host.as_ref().unwrap().unit, unit);
    let reread = deliveries(&mut again.events, 15, |d| d.len() >= before.len());
    assert_eq!(
        events_of(&reread[..before.len()]),
        events_of(&before),
        "the same events"
    );
    assert_eq!(
        reread[..before.len()]
            .iter()
            .map(|d| d.at)
            .collect::<Vec<_>>(),
        before.iter().map(|d| d.at).collect::<Vec<_>>(),
        "at the same cursors"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        refusals(&s.dir),
        1,
        "a request re-read at or below committed gets no second response"
    );
    // The child is the same process, and the turn goes on after the gate opens.
    assert_eq!(second.process_id(&again.chat), Some(pid));
    std::fs::write(&gate, b"").unwrap();
    let mut all = reread;
    all.extend(deliveries(&mut again.events, 15, has_result));
    check_order(&events_of(&all)).unwrap();
    assert!(
        matches!(
            all.last().unwrap().event,
            ChatEvent::TurnResult {
                cause: TurnCause::Operator,
                ..
            }
        ),
        "{:?}",
        all.last()
    );
    second.stop(&again.chat).unwrap();
}

/// The d31 walk of 2026-11-20: the controller restarted while a reply streamed. A turn's start
/// and its deltas store no item, so the store held no position for the host: nothing acknowledged,
/// nothing committed and no checkpoint. The second adapter reads the `init` again as a new line,
/// with a decoder that never saw the send. The message started the turn, so it stays an operator
/// turn.
#[test]
fn a_rebind_with_nothing_committed_keeps_the_cause_of_the_turn_a_message_started() {
    let root = tempfile::tempdir().unwrap();
    let mut body: Vec<Line> = scene::open();
    body.push(scene::user("u1", "go"));
    body.push(scene::sys_init(&["u1"]));
    body.push(scene::delta("hi"));
    body.push(scene::text("hi"));
    body.push(scene::result(0.5, 10, 20, &["u1"]));
    let fixture = write_fixture(root.path(), "streamed_reply", &body);
    let gate = root.path().join("gate-assistant");
    let s = setup(
        &fixture,
        &format!("--gate assistant:{}", gate.display()),
        BTreeMap::new(),
    );
    let first = hosted_with(s.cfg.clone());
    let mut opened = first.open(&spec(s.work.path())).expect("open");
    let unit = opened.host.clone().unwrap().unit;
    first.send(&opened.chat, InputKind::Operator, "go").unwrap();
    let before = deliveries(&mut opened.events, 15, |d| {
        d.iter()
            .any(|x| matches!(x.event, ChatEvent::TextDelta { .. }))
    });
    assert_eq!(
        before[0].event,
        ChatEvent::TurnStarted {
            cause: TurnCause::Operator
        }
    );
    assert!(
        before.iter().all(|d| d.checkpoint.is_none()),
        "no quiet point yet, so the controller has no checkpoint: {before:#?}"
    );
    first.detach(&opened.chat).unwrap();

    let second = ClaudeChat::new(s.cfg.clone());
    second.set_hosts(first.hosts.clone());
    let point = ChatResumePoint {
        runtime_session: opened.session.clone(),
        acked: 0,
        committed: 0,
        checkpoint: None,
        pending: Vec::new(),
    };
    let ChatRebind::Survived(mut again) = second.rebind(&unit, &point) else {
        panic!("the host survived");
    };
    let mut all = deliveries(&mut again.events, 15, |d| d.len() >= before.len());
    assert_eq!(
        all[0].event,
        ChatEvent::TurnStarted {
            cause: TurnCause::Operator
        },
        "the turn read again is the operator's"
    );
    std::fs::write(&gate, b"").unwrap();
    all.extend(deliveries(&mut again.events, 15, has_result));
    check_order(&events_of(&all)).unwrap();
    assert!(
        matches!(
            all.last().unwrap().event,
            ChatEvent::TurnResult {
                cause: TurnCause::Operator,
                ..
            }
        ),
        "{:?}",
        all.last()
    );
    second.stop(&again.chat).unwrap();
}

#[test]
fn a_rebind_from_a_checkpoint_takes_the_totals_and_a_pending_request_can_be_answered() {
    // Turn one ends (a quiet point, so a checkpoint); turn two asks for approval and waits.
    let root = tempfile::tempdir().unwrap();
    let mut body: Vec<Line> = scene::open();
    body.push(scene::user("u1", "one"));
    body.extend(scene::turn("u1", "a", 0.25, 10, 20));
    body.push(scene::user("u2", "two"));
    body.push(scene::sys_init(&["u2"]));
    body.push(scene::can_use("a1", "Bash", json!({"command": "ls"})));
    body.push(scene::cli_answer("a1"));
    body.extend([scene::text("done"), scene::result(0.75, 30, 50, &["u2"])]);
    let fixture = write_fixture(root.path(), "approval", &body);
    let s = setup(&fixture, "", BTreeMap::new());
    let first = hosted_with(s.cfg.clone());
    let mut opened = first.open(&spec(s.work.path())).expect("open");
    let unit = opened.host.clone().unwrap().unit;
    first
        .send(&opened.chat, InputKind::Operator, "one")
        .unwrap();
    let one = deliveries(&mut opened.events, 15, has_result);
    let checkpoint = one.last().unwrap().checkpoint.clone().unwrap();
    let acked = one.last().unwrap().at.unwrap();
    first.ack(&opened.chat, acked).unwrap();
    first
        .send(&opened.chat, InputKind::Operator, "two")
        .unwrap();
    let two = deliveries(&mut opened.events, 15, |d| {
        d.iter()
            .any(|x| matches!(x.event, ChatEvent::ApprovalRequest { .. }))
    });
    let committed = two.last().unwrap().at.unwrap();
    first.detach(&opened.chat).unwrap();

    let second = ClaudeChat::new(s.cfg.clone());
    second.set_hosts(first.hosts.clone());
    let point = ChatResumePoint {
        runtime_session: opened.session.clone(),
        acked,
        committed,
        checkpoint: Some(checkpoint),
        pending: vec![PendingRequest {
            request_id: "a1".into(),
            kind: PendingKind::Approval,
            input: Some("{\"command\":\"ls\"}".into()),
        }],
    };
    let ChatRebind::Survived(mut again) = second.rebind(&unit, &point) else {
        panic!("the host survived");
    };
    // The read starts at `acked`: turn one is not read again.
    let reread = deliveries(&mut again.events, 15, |d| {
        d.iter()
            .any(|x| matches!(x.event, ChatEvent::ApprovalRequest { .. }))
    });
    assert!(
        !reread
            .iter()
            .any(|d| matches!(d.event, ChatEvent::Text { ref text } if text == "a")),
        "turn one is not read again: {reread:#?}"
    );
    assert_eq!(reread.last().unwrap().at, Some(committed));
    // The approval is open: an allow goes through, and the turn finishes.
    second
        .answer(&again.chat, "a1", &pf_engine::port::chat::ChatAnswer::Allow)
        .unwrap();
    let rest = deliveries(&mut again.events, 15, has_result);
    let result = rest.last().unwrap();
    let ChatEvent::TurnResult { usage, .. } = &result.event else {
        panic!("{result:?}");
    };
    assert_eq!(
        usage.cost_micros, 500_000,
        "the turn's own cost: the checkpoint's totals are the baseline (0.75 - 0.25 dollars)"
    );
    second.stop(&again.chat).unwrap();
}

#[test]
fn stop_ends_the_childs_group_and_the_host_and_is_safe_to_repeat() {
    let s = setup(&plain_turn(), "", BTreeMap::new());
    let c = hosted_with(s.cfg.clone());
    let mut opened = c.open(&spec(s.work.path())).expect("open");
    let unit = opened.host.clone().unwrap().unit;
    let pid = read_pid(&s.dir);
    assert!(c.hosts.is_live(&unit));
    c.stop(&opened.chat).unwrap();
    assert!(wait_gone(pid), "the child is gone");
    let end = Instant::now() + Duration::from_secs(10);
    while c.hosts.is_live(&unit) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!c.hosts.is_live(&unit), "the host ended with it");
    let got = deliveries(&mut opened.events, 5, |d| {
        d.iter()
            .any(|x| matches!(x.event, ChatEvent::Exited { .. }))
    });
    assert_eq!(
        got.iter()
            .filter(|d| matches!(d.event, ChatEvent::Exited { .. }))
            .count(),
        1
    );
    c.stop(&opened.chat).expect("a second stop is fine");
    assert!(
        std::iter::from_fn(|| opened.events.try_recv().ok())
            .next()
            .is_none(),
        "no second Exited"
    );
}

#[test]
fn an_exit_then_an_ack_of_its_cursor_sends_exit_ack_and_the_host_ends_at_once() {
    let m = managed(
        &format!("{} (Claude Code)", pinned::CLI_VERSION),
        &format!("{ANSWER_INITIALIZE}exit 0"),
    );
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut opened = c.open(&spec(work.path())).expect("open");
    let unit = opened.host.clone().unwrap().unit;
    let got = deliveries(&mut opened.events, 10, |d| {
        d.iter()
            .any(|x| matches!(x.event, ChatEvent::Exited { .. }))
    });
    let exited = got.last().unwrap();
    let at = exited.at.expect("the exit carries its cursor");
    assert!(
        c.hosts.is_live(&unit),
        "the host waits for the exit to be consumed"
    );
    c.ack(&opened.chat, at).unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while c.hosts.is_live(&unit) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !c.hosts.is_live(&unit),
        "the host ended at once after the ack"
    );
}

#[test]
fn a_recorded_binary_that_is_gone_or_changed_fails_the_resume_and_starts_nothing() {
    let m = good_stub();
    let c = chat(&m);
    put_transcript(
        &m.root.path().join("claude-config"),
        "11111111-2222-4333-8444-555555555555",
    );
    let work = tempfile::tempdir().unwrap();
    let mut spec = spec(work.path());
    spec.resume = Some(SessionRef("11111111-2222-4333-8444-555555555555".into()));
    let real = hash_file(&m.bin).unwrap();
    let cases = [
        ("gone", m.root.path().join("no-such-claude"), real.clone()),
        ("changed", m.bin.clone(), "ab".repeat(32)),
    ];
    for (name, path, sha) in cases {
        spec.recorded_binary = Some(RecordedBinary {
            path: path.display().to_string(),
            sha256: sha.clone(),
            version: pinned::CLI_VERSION.into(),
        });
        let err = c.open(&spec).expect_err(name);
        let ChatError::RuntimeNotReady(why) = &err else {
            panic!("{name}: a missing copy is RuntimeNotReady, not context lost: {err:?}");
        };
        assert!(why.contains("gone or changed"), "{name}: {why}");
        assert!(why.contains(&path.display().to_string()), "{name}: {why}");
        assert!(why.contains(&sha), "{name}: {why}");
        assert!(why.contains("put that copy back"), "{name}: {why}");
        assert_eq!(
            c.hosts.starts.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "{name}: no host started, so no other binary ran"
        );
    }
    // The right copy opens, and runs the recorded path.
    spec.recorded_binary = Some(RecordedBinary {
        path: m.bin.display().to_string(),
        sha256: real.clone(),
        version: pinned::CLI_VERSION.into(),
    });
    let opened = c.open(&spec).expect("the recorded copy is there");
    let unit = opened.host.clone().unwrap().unit;
    let record = c.hosts.launched_record(&unit).unwrap().unwrap();
    assert_eq!(
        record["runtime"]["binary_path"],
        m.bin.display().to_string()
    );
    assert_eq!(record["runtime"]["binary_sha256"], real);
    c.stop(&opened.chat).unwrap();
    let _ = ClaudeConfig::new("/x");
}
