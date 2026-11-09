//! AC-6: `ClaudeChat` against `pf-claude-replay` and the recorded fixtures. The real adapter reads
//! the real recordings, so the events a case recorded are the events the chat gives, in order, and
//! each sequence passes `check_order`. Scenes no recording holds are built by hand and played the
//! same way.

mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use common::scene::{self, Line};
use common::*;
use pf_adapter_claude::decode::{Decoder, Out, decode_recording};
use pf_adapter_claude::version::{MID_TURN, SELF_WAKE, SelfWake};
use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::chat::{
    ChatAnswer, ChatError, ChatEvent, ChatPort, ContextUsage, InputKind, MidTurn, TurnCause,
    check_order,
};
use serde_json::{Value, json};

const SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";

// ------------------------------------------------------------------ the driver

/// What the chat gave and what the recording says it should give.
struct Driven {
    got: Vec<ChatEvent>,
    want: Vec<ChatEvent>,
    exited: Vec<ChatEvent>,
}

fn content_text(msg: &Value) -> String {
    match &msg["message"]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect(),
        other => panic!("a user message with content {other}"),
    }
}

fn turn_open(events: &[ChatEvent]) -> bool {
    let mut open = false;
    for e in events {
        match e {
            ChatEvent::TurnStarted { .. } => open = true,
            ChatEvent::TurnResult { .. } | ChatEvent::Exited { .. } => open = false,
            _ => {}
        }
    }
    open
}

fn pump(opened: &mut pf_engine::port::chat::ChatOpened, got: &mut Vec<ChatEvent>, n: usize) {
    if got.len() >= n {
        return;
    }
    let need = n - got.len();
    got.extend(collect_until(&mut opened.events, 15, |g| g.len() >= need));
}

/// Play one recorded case through the chat: do what the recorder did, in its order, and wait for
/// the events of each step before the next one.
fn drive(name: &str) -> Driven {
    let f = load_fixture(name);
    let path = fixtures_dir().join(format!("{name}.jsonl"));
    let rig = rig(&path);
    let mut s = spec(rig.work.path());
    if name == "resume" {
        put_transcript(&rig.root.path().join("claude-config"), SESSION_ID);
        s.resume = Some(SessionRef(SESSION_ID.into()));
    }
    let mut opened = rig.chat.open(&s).unwrap_or_else(|e| panic!("{name}: {e}"));
    let chat = &rig.chat;
    let id = opened.chat.clone();
    let mut got: Vec<ChatEvent> = Vec::new();
    let mut answered: BTreeSet<String> = BTreeSet::new();
    let mut stalled = None;

    let cli_lines: Vec<usize> = f
        .body
        .iter()
        .enumerate()
        .filter(|(_, (to, _))| to == "cli")
        .map(|(i, _)| i)
        .collect();
    for &i in &cli_lines[1..] {
        let prefix = decode_recording("2.1.319", "", &f.body[..i]).0;
        pump(&mut opened, &mut got, prefix.len());
        let msg = &f.body[i].1;
        match msg["type"].as_str().unwrap() {
            "user" => chat
                .send(&id, InputKind::Operator, &content_text(msg))
                .unwrap(),
            "control_response" => {
                let (request, is_question) = got
                    .iter()
                    .rev()
                    .find_map(|e| match e {
                        ChatEvent::ApprovalRequest { request_id, .. }
                            if !answered.contains(request_id) =>
                        {
                            Some((request_id.clone(), false))
                        }
                        ChatEvent::Question { request_id, .. }
                            if !answered.contains(request_id) =>
                        {
                            Some((request_id.clone(), true))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("{name}: an answer with no open request"));
                let body = &msg["response"]["response"];
                let answer = if body["behavior"] == "deny" {
                    ChatAnswer::Deny {
                        message: body["message"].as_str().unwrap_or("").into(),
                    }
                } else if let Some(answers) = body["updatedInput"]["answers"].as_object() {
                    ChatAnswer::Answers(
                        answers
                            .iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                            .collect(),
                    )
                } else if is_question {
                    // The recorder allowed a question with no answers; the chat takes only
                    // `Answers` for a question, so this is the same step with an empty set.
                    ChatAnswer::Answers(Default::default())
                } else {
                    ChatAnswer::Allow
                };
                chat.answer(&id, &request, &answer).unwrap();
                answered.insert(request);
            }
            "control_request" => {
                let r = &msg["request"];
                match r["subtype"].as_str().unwrap() {
                    // A repeat: the replay does not wait for it.
                    "initialize" => {}
                    "set_model" => chat
                        .set_option(&id, "model", r["model"].as_str().unwrap())
                        .unwrap(),
                    // The recorder sent `effortLevel` or `enableArtifact`. The replay matches the
                    // subtype only, and the chat sends this subtype for an effort change.
                    "apply_flag_settings" => chat.set_option(&id, "effort", "low").unwrap(),
                    "set_permission_mode" => chat
                        .set_option(&id, "permission_mode", r["mode"].as_str().unwrap())
                        .unwrap(),
                    "get_context_usage" => {
                        let usage = chat.context_usage(&id).unwrap();
                        assert_eq!(
                            usage,
                            ContextUsage {
                                used_tokens: 28005,
                                max_tokens: 1_000_000
                            },
                            "{name}: the usage the recording answered"
                        );
                    }
                    "interrupt" => {
                        if turn_open(&prefix) {
                            chat.interrupt(&id).unwrap();
                        } else {
                            // The chat sends no interrupt with no turn open (a no-op), so the
                            // replay never plays what follows it.
                            stalled = Some(i);
                            break;
                        }
                    }
                    other => {
                        panic!("{name}: the recorder sent {other}, which the driver does not know")
                    }
                }
            }
            other => panic!("{name}: a cli line of type {other}"),
        }
    }
    let want = match stalled {
        Some(i) => decode_recording("2.1.319", "", &f.body[..i]).0,
        None => decode_recording("2.1.319", "", &f.body).0,
    };
    pump(&mut opened, &mut got, want.len());
    std::thread::sleep(Duration::from_millis(50));
    while let Ok(e) = opened.events.try_recv() {
        got.push(e);
    }
    let exited = stop_and_drain(chat, &mut opened);
    Driven { got, want, exited }
}

// ------------------------------------------------------------------ every recording

#[test]
fn each_fixture_gives_the_events_its_case_recorded_and_passes_check_order() {
    for f in all_fixtures() {
        if f.name == "resume_missing" {
            continue; // an open that fails: its own test
        }
        let d = drive(&f.name);
        assert_eq!(d.got, d.want, "{}: the events the recording gives", f.name);
        assert!(
            matches!(d.exited.as_slice(), [ChatEvent::Exited { .. }]),
            "{}: one Exited after stop: {:?}",
            f.name,
            d.exited
        );
        let mut all = d.got.clone();
        all.extend(d.exited.clone());
        check_order(&all).unwrap_or_else(|e| panic!("{}: check_order: {e}", f.name));
    }
}

/// Cumulative totals of a recording's last result, in micros and tokens.
fn last_totals(name: &str) -> Usage {
    let f = load_fixture(name);
    let msg = f
        .body
        .iter()
        .rev()
        .find(|(to, m)| to == "host" && m["type"] == "result")
        .map(|(_, m)| m.clone())
        .unwrap();
    let mut u = Usage {
        cost_micros: (msg["total_cost_usd"].as_f64().unwrap() * 1e6).round() as u64,
        ..Usage::default()
    };
    for m in msg["modelUsage"].as_object().unwrap().values() {
        u.input_tokens += m["inputTokens"].as_u64().unwrap_or(0);
        u.output_tokens += m["outputTokens"].as_u64().unwrap_or(0);
        u.cache_read_tokens += m["cacheReadInputTokens"].as_u64().unwrap_or(0);
        u.cache_write_tokens += m["cacheCreationInputTokens"].as_u64().unwrap_or(0);
    }
    u
}

fn usages(events: &[ChatEvent]) -> Vec<Usage> {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnResult { usage, .. } => Some(*usage),
            _ => None,
        })
        .collect()
}

fn sum(us: &[Usage]) -> Usage {
    us.iter().fold(Usage::default(), |a, u| Usage {
        input_tokens: a.input_tokens + u.input_tokens,
        output_tokens: a.output_tokens + u.output_tokens,
        cache_read_tokens: a.cache_read_tokens + u.cache_read_tokens,
        cache_write_tokens: a.cache_write_tokens + u.cache_write_tokens,
        cost_micros: a.cost_micros + u.cost_micros,
    })
}

#[test]
fn three_turns_give_three_per_turn_usages_that_sum_to_the_last_cumulative_total() {
    let d = drive("plain_turn");
    let us = usages(&d.got);
    assert_eq!(us.len(), 3);
    assert_eq!(
        sum(&us),
        last_totals("plain_turn"),
        "micros and every token class"
    );
    assert!(
        us[1].cost_micros < last_totals("plain_turn").cost_micros,
        "a turn's own usage, never a running total"
    );
    // The cache time to live of the recording: one hour.
    assert!(d.got.iter().any(|e| matches!(
        e,
        ChatEvent::TurnResult {
            cache_ttl_seconds: Some(3600),
            ..
        }
    )));
}

#[test]
fn a_resumed_process_starts_its_baseline_again_from_zero() {
    let first = usages(&drive("resume_first").got);
    let second = usages(&drive("resume").got);
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0],
        last_totals("resume"),
        "the second process counts from zero"
    );
    assert!(second[0].cost_micros > 0);
}

#[test]
fn a_cumulative_total_that_goes_down_counts_zero_and_becomes_the_new_baseline() {
    let mut body = scene::open();
    body.push(scene::user("u1", "a"));
    body.extend(scene::turn("u1", "a", 0.5, 100, 50));
    body.push(scene::user("u2", "b"));
    body.extend(scene::turn("u2", "b", 0.2, 40, 10)); // every total went down
    body.push(scene::user("u3", "c"));
    body.extend(scene::turn("u3", "c", 0.3, 90, 30));
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, Value)> = body.iter().map(|(t, m)| (*t, m.clone())).collect();
    let fixture = write_fixture(dir.path(), "going_down", &refs);
    let r = rig(&fixture);
    let mut opened = r.open();
    let mut got = Vec::new();
    for (i, text) in ["a", "b", "c"].iter().enumerate() {
        r.chat
            .send(&opened.chat, InputKind::Operator, text)
            .unwrap();
        got.extend(collect_until(&mut opened.events, 10, |g| {
            count_turn_results(g) == 1
        }));
        assert_eq!(count_turn_results(&got), i + 1);
    }
    let us = usages(&got);
    let u = |input, output, cost| Usage {
        input_tokens: input,
        output_tokens: output,
        cost_micros: cost,
        ..Usage::default()
    };
    assert_eq!(us, [u(100, 50, 500_000), u(0, 0, 0), u(50, 20, 100_000)]);
    // The log line: the decoder says so; the adapter prints what it says.
    let mut dec = Decoder::new("2.1.319", scene::SESSION, false);
    let mut logs = Vec::new();
    for (to, msg) in &body {
        if *to == "host" {
            for o in dec.feed(msg) {
                if let Out::Log(l) = o {
                    logs.push(l);
                }
            }
        }
    }
    assert!(logs.iter().any(|l| l.contains("went down")), "{logs:?}");
    r.chat.stop(&opened.chat).unwrap();
}

// ------------------------------------------------------------------ delivery, tools, tasks

/// Seconds-level pacing: take events, noting when each kind first came.
fn first_times(
    opened: &mut pf_engine::port::chat::ChatOpened,
    secs: u64,
    stop_at: impl Fn(&ChatEvent) -> bool,
) -> Vec<(ChatEvent, Instant)> {
    let mut seen = Vec::new();
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        assert!(
            Instant::now() < end,
            "the awaited event did not come in {secs} s: {seen:?}"
        );
        while let Ok(e) = opened.events.try_recv() {
            let stop = stop_at(&e);
            seen.push((e, Instant::now()));
            if stop {
                return seen;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn deltas_reach_the_receiver_well_before_the_replay_writes_the_result() {
    // A scene with real gaps: deltas at 100, 400 and 700 ms, the result at 1300 ms. A chat that
    // delivers live shows the first delta about 1.2 s before the result; one that batches shows none.
    let mut body = scene::open();
    body.push(scene::user("u1", "go"));
    let timed: Vec<(&str, u64, Value)> = body
        .iter()
        .map(|(t, m)| (*t, 0, m.clone()))
        .chain([
            ("host", 10, scene::sys_init(&["u1"]).1),
            ("host", 100, scene::delta("one ").1),
            ("host", 400, scene::delta("two ").1),
            ("host", 700, scene::delta("three").1),
            ("host", 710, scene::text("one two three").1),
            ("host", 1300, scene::result(0.1, 10, 5, &["u1"]).1),
        ])
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture_timed(dir.path(), "paced", &timed);
    let r = rig_with(&fixture, true, false, |_| {});
    let mut opened = r.open();
    r.chat
        .send(&opened.chat, InputKind::Operator, "go")
        .unwrap();
    let seen = first_times(&mut opened, 10, |e| {
        matches!(e, ChatEvent::TurnResult { .. })
    });
    let at = |pred: fn(&ChatEvent) -> bool| seen.iter().find(|(e, _)| pred(e)).unwrap().1;
    let first_delta = at(|e| matches!(e, ChatEvent::TextDelta { .. }));
    let result = at(|e| matches!(e, ChatEvent::TurnResult { .. }));
    assert!(
        result - first_delta >= Duration::from_millis(900),
        "the first delta came {:?} before the result; live delivery shows about 1.2 s",
        result - first_delta
    );
    r.chat.stop(&opened.chat).unwrap();
}

#[test]
fn at_the_recorded_pace_a_recorded_turn_keeps_its_gaps() {
    // `plain_turn` records 816 ms between the turn's `init` and its first text delta.
    let path = fixtures_dir().join("plain_turn.jsonl");
    let r = rig_with(&path, true, false, |_| {});
    let mut opened = r.open();
    r.chat
        .send(&opened.chat, InputKind::Operator, "go")
        .unwrap();
    let seen = first_times(&mut opened, 15, |e| {
        matches!(e, ChatEvent::TurnResult { .. })
    });
    let started = seen
        .iter()
        .find(|(e, _)| matches!(e, ChatEvent::TurnStarted { .. }))
        .unwrap()
        .1;
    let delta = seen
        .iter()
        .find(|(e, _)| matches!(e, ChatEvent::TextDelta { .. }))
        .unwrap()
        .1;
    assert!(
        delta - started >= Duration::from_millis(600),
        "TurnStarted to the first delta took {:?}; the recording says 816 ms",
        delta - started
    );
    r.chat.stop(&opened.chat).unwrap();
}

#[test]
fn the_large_output_fixture_gives_a_capped_tool_result_with_its_original_size() {
    let d = drive("big_output");
    let capped: Vec<_> = d
        .got
        .iter()
        .filter_map(|e| match e {
            ChatEvent::ToolFinished { output, .. } if output.truncated => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(capped.len(), 1, "one output is cut");
    assert_eq!(capped[0].bytes, 18892, "the original size");
    assert!(
        capped[0].text.len() <= pf_engine::port::chat::CAP_BYTES,
        "within 16 KB, mark included"
    );
    assert!(capped[0].text.ends_with("…[truncated: 18892 bytes in all]"));
    let whole: Vec<_> = d
        .got
        .iter()
        .filter_map(|e| match e {
            ChatEvent::ToolFinished { output, .. } if !output.truncated => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(
        whole.len(),
        1,
        "the 2 KB preview of the first call stays whole"
    );
}

#[test]
fn a_subagents_text_and_tools_give_no_event() {
    let sub = |mut line: Line| -> Line {
        line.1["parent_tool_use_id"] = json!("toolu_sub");
        line
    };
    let mut body = scene::open();
    body.push(scene::user("u1", "go"));
    body.push(scene::sys_init(&["u1"]));
    body.push(sub(scene::delta("from the subagent")));
    body.push(sub(scene::text("from the subagent")));
    body.push(sub((
        "host",
        json!({"type": "assistant", "parent_tool_use_id": "toolu_sub", "message": {"role": "assistant",
            "content": [{"type": "tool_use", "id": "toolu_inner", "name": "Bash", "input": {"command": "ls"}}]}}),
    )));
    body.push(sub((
        "host",
        json!({"type": "user", "parent_tool_use_id": "toolu_sub", "message": {"role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "toolu_inner", "content": "x"}]}}),
    )));
    body.push(scene::delta("main"));
    body.push(scene::text("main"));
    body.push(scene::result(0.1, 10, 5, &["u1"]));
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, Value)> = body.iter().map(|(t, m)| (*t, m.clone())).collect();
    let r = rig(&write_fixture(dir.path(), "subagent", &refs));
    let mut opened = r.open();
    r.chat
        .send(&opened.chat, InputKind::Operator, "go")
        .unwrap();
    let got = collect_until(&mut opened.events, 10, |g| count_turn_results(g) == 1);
    let names: Vec<&str> = got.iter().map(|e| e.name()).collect();
    assert_eq!(
        names,
        ["TurnStarted", "TextDelta", "Text", "TurnResult"],
        "{got:?}"
    );
    assert!(got.contains(&ChatEvent::Text {
        text: "main".into()
    }));
    r.chat.stop(&opened.chat).unwrap();
}

fn tasks_changed(ids: &[&str]) -> Line {
    let tasks: Vec<Value> = ids
        .iter()
        .map(|i| json!({"task_id": i, "task_type": "local_bash", "description": format!("task {i}")}))
        .collect();
    (
        "host",
        json!({"type": "system", "subtype": "background_tasks_changed", "tasks": tasks}),
    )
}

#[test]
fn background_tasks_replace_their_set_on_each_change_and_start_empty() {
    let mut body = scene::open();
    body.push(scene::user("u1", "go"));
    body.push(scene::sys_init(&["u1"]));
    body.push(tasks_changed(&["a", "b"]));
    body.push(tasks_changed(&["b"]));
    body.push(scene::result(0.1, 10, 5, &["u1"]));
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, Value)> = body.iter().map(|(t, m)| (*t, m.clone())).collect();
    let r = rig(&write_fixture(dir.path(), "tasks", &refs));
    let mut opened = r.open();
    assert_eq!(
        r.chat.background_tasks(&opened.chat).unwrap(),
        [],
        "empty before the first"
    );
    r.chat
        .send(&opened.chat, InputKind::Operator, "go")
        .unwrap();
    collect_until(&mut opened.events, 10, |g| count_turn_results(g) == 1);
    let tasks = r.chat.background_tasks(&opened.chat).unwrap();
    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["b"], "the second snapshot replaces the first");
    assert_eq!(tasks[0].kind, "local_bash");
    assert!(
        !tasks[0].ambient,
        "no ambient key in any recording, so false"
    );
    r.chat.stop(&opened.chat).unwrap();
}

#[test]
fn the_task_signal_tracks_a_recorded_background_task_and_its_finish_starts_a_task_turn() {
    // The recording keeps 4.4 s between the turn's result and the task's end; at the recorded pace
    // the set can be read between them.
    let path = fixtures_dir().join("background_task.jsonl");
    let r = rig_with(&path, true, true, |_| {});
    let mut opened = r.open();
    r.chat
        .send(&opened.chat, InputKind::Operator, "go")
        .unwrap();
    let mut got = collect_until(&mut opened.events, 20, |g| count_turn_results(g) == 1);
    std::thread::sleep(Duration::from_millis(100));
    let during = r.chat.background_tasks(&opened.chat).unwrap();
    assert_eq!(
        during.len(),
        1,
        "the task is listed while it runs: {during:?}"
    );
    assert_eq!(during[0].kind, "local_bash");
    got.extend(collect_until(&mut opened.events, 20, |g| {
        count_turn_results(g) == 1
    }));
    let end = Instant::now() + Duration::from_secs(5);
    while !r.chat.background_tasks(&opened.chat).unwrap().is_empty() {
        assert!(Instant::now() < end, "the set is still not empty");
        std::thread::sleep(Duration::from_millis(10));
    }
    let causes: Vec<TurnCause> = got
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnStarted { cause } => Some(*cause),
            _ => None,
        })
        .collect();
    assert_eq!(
        causes,
        [TurnCause::Operator, TurnCause::Task],
        "the CLI woke itself"
    );
    assert_eq!(SELF_WAKE, SelfWake::Cli);
    // Self-wake is the CLI's: the adapter wrote the initialize and the one user message, no notice.
    let types: Vec<String> = r
        .stdin_lines()
        .iter()
        .map(|l| l["type"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        types,
        ["control_request", "user"],
        "no notice message: {types:?}"
    );
    r.chat.stop(&opened.chat).unwrap();
}

#[test]
fn a_message_sent_mid_turn_steers_the_turn_and_the_fake_agrees_on_the_mode() {
    let d = drive("mid_turn");
    let turns = d
        .got
        .iter()
        .filter(|e| matches!(e, ChatEvent::TurnStarted { .. }))
        .count();
    assert_eq!(turns, 1, "no new TurnStarted for a steered send");
    assert_eq!(
        count_turn_results(&d.got),
        1,
        "one TurnResult covers both messages"
    );
    assert!(d.got.iter().any(|e| matches!(
        e,
        ChatEvent::TurnResult {
            cause: TurnCause::Operator,
            ..
        }
    )));
    assert_eq!(MID_TURN, MidTurn::Steer);
    let r = rig(&fixtures_dir().join("mid_turn.jsonl"));
    assert_eq!(r.chat.mid_turn(), MID_TURN);
    assert_eq!(
        pf_adapter_fake::FakeChat::new().mid_turn(),
        MID_TURN,
        "the fake and the adapter agree"
    );
}

// ------------------------------------------------------------------ session, resume, checks

#[test]
fn an_init_with_another_session_id_gives_session_moved_once() {
    let mut body = scene::open();
    body.push(scene::user("u1", "a"));
    body.extend(scene::turn("u1", "a", 0.1, 10, 5));
    body.push(scene::user("u2", "b"));
    let mut moved = scene::sys_init(&["u2"]);
    moved.1["session_id"] = json!("moved-session");
    body.extend([
        moved,
        scene::delta("b"),
        scene::text("b"),
        scene::result(0.2, 20, 10, &["u2"]),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, Value)> = body.iter().map(|(t, m)| (*t, m.clone())).collect();
    let r = rig(&write_fixture(dir.path(), "moved", &refs));
    let mut opened = r.open();
    let mut got = Vec::new();
    for text in ["a", "b"] {
        r.chat
            .send(&opened.chat, InputKind::Operator, text)
            .unwrap();
        got.extend(collect_until(&mut opened.events, 10, |g| {
            count_turn_results(g) == 1
        }));
    }
    let moves: Vec<&ChatEvent> = got
        .iter()
        .filter(|e| matches!(e, ChatEvent::SessionMoved { .. }))
        .collect();
    assert_eq!(
        moves,
        [&ChatEvent::SessionMoved {
            session: SessionRef("moved-session".into())
        }]
    );
    let at = got
        .iter()
        .position(|e| matches!(e, ChatEvent::SessionMoved { .. }))
        .unwrap();
    assert!(
        matches!(got[at - 1], ChatEvent::TurnStarted { .. }),
        "right after the second TurnStarted"
    );
    check_order(&got).expect("the grammar allows it");
    r.chat.stop(&opened.chat).unwrap();
}

#[test]
fn a_resume_of_a_session_with_no_transcript_is_resume_failed_and_the_error_is_named() {
    // Case 15: the CLI answers no `initialize`, writes one error `result`, and exits 1.
    let path = fixtures_dir().join("resume_missing.jsonl");
    let r = rig(&path);
    put_transcript(&r.root.path().join("claude-config"), SESSION_ID);
    let mut s = spec(r.work.path());
    s.resume = Some(SessionRef(SESSION_ID.into()));
    let started = Instant::now();
    let err = r.chat.open(&s).expect_err("no resume");
    assert!(matches!(err, ChatError::ResumeFailed(_)), "{err}");
    assert!(err.to_string().contains("No conversation found"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "it does not wait out init_seconds"
    );
}

#[test]
fn a_process_that_exits_before_initialize_answers_is_resume_failed_for_a_resume() {
    let m = managed(
        "2.1.319 (Claude Code)",
        "echo 'No conversation found with session ID: x' >&2\nexit 1",
    );
    let c = chat(&m);
    put_transcript(&m.root.path().join("claude-config"), SESSION_ID);
    let work = tempfile::tempdir().unwrap();
    let mut s = spec(work.path());
    s.resume = Some(SessionRef(SESSION_ID.into()));
    let err = c.open(&s).expect_err("exits at once");
    assert!(matches!(err, ChatError::ResumeFailed(_)), "{err}");
    assert!(err.to_string().contains("No conversation found"), "{err}");
    // The same exit in a new session is Failed, not ResumeFailed.
    let err = c.open(&spec(work.path())).expect_err("exits at once");
    assert!(matches!(err, ChatError::Failed(_)), "{err}");
}

/// Open the `init` fixture with one change to its `init` message, send, and take what comes.
fn init_with(edit: impl Fn(&mut Value)) -> Vec<ChatEvent> {
    let dir = tempfile::tempdir().unwrap();
    let path = edited_fixture(dir.path(), "init", |to, msg| {
        if to == "host" && msg["type"] == "system" && msg["subtype"] == "init" {
            edit(msg);
        }
    });
    let r = rig(&path);
    let mut opened = r.open();
    r.chat
        .send(&opened.chat, InputKind::Operator, "hello")
        .unwrap();
    let got = collect_until(&mut opened.events, 10, |g| {
        g.iter().any(|e| matches!(e, ChatEvent::Exited { .. }))
    });
    r.chat.stop(&opened.chat).unwrap();
    got
}

fn exit_reason(events: &[ChatEvent]) -> String {
    match events.last() {
        Some(ChatEvent::Exited { reason, .. }) => reason.clone(),
        other => panic!("no Exited at the end: {other:?}"),
    }
}

#[test]
fn an_init_that_fails_a_check_ends_in_exited_with_a_reason_that_names_the_check() {
    let key = init_with(|m| m["apiKeySource"] = json!("ANTHROPIC_API_KEY"));
    assert!(exit_reason(&key).contains("apiKeySource"), "{key:?}");
    assert_eq!(key.len(), 1, "no turn starts on a failed check: {key:?}");

    let timer = init_with(|m| m["tools"].as_array_mut().unwrap().push(json!("CronCreate")));
    assert!(exit_reason(&timer).contains("CronCreate"), "{timer:?}");
    assert_eq!(timer.len(), 1, "{timer:?}");

    let version = init_with(|m| m["claude_code_version"] = json!("2.1.318"));
    let reason = exit_reason(&version);
    assert!(
        reason.contains("2.1.318") && reason.contains("version"),
        "{reason}"
    );
    assert_eq!(version.len(), 1, "{version:?}");
}

#[test]
fn a_line_that_is_not_json_is_skipped() {
    let init = serde_json::to_string(&scene::sys_init(&[]).1).unwrap();
    let delta = serde_json::to_string(&scene::delta("after").1).unwrap();
    let result = serde_json::to_string(&scene::result(0.1, 1, 1, &[]).1).unwrap();
    let m = managed(
        "2.1.319 (Claude Code)",
        &format!(
            "{ANSWER_INITIALIZE}echo 'this line is not JSON {{'\nread -r user\nprintf '%s\\n' '{init}' '{delta}' '{result}'\ncat > /dev/null"
        ),
    );
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut opened = c.open(&spec(work.path())).expect("open");
    c.send(&opened.chat, InputKind::Operator, "go").unwrap();
    let got = collect_until(&mut opened.events, 10, |g| count_turn_results(g) == 1);
    assert!(
        got.contains(&ChatEvent::TextDelta {
            text: "after".into()
        }),
        "{got:?}"
    );
    c.stop(&opened.chat).unwrap();
}

#[test]
fn a_receiver_dropped_mid_turn_fails_no_later_call_and_stop_still_works() {
    let path = fixtures_dir().join("plain_turn.jsonl");
    let r = rig(&path);
    let opened = r.open();
    let chat_id = opened.chat.clone();
    r.chat.send(&chat_id, InputKind::Operator, "one").unwrap();
    drop(opened.events);
    std::thread::sleep(Duration::from_millis(200));
    r.chat
        .send(&chat_id, InputKind::Operator, "two")
        .expect("a send after the drop");
    assert!(r.chat.is_alive(&chat_id).unwrap(), "the process lives on");
    assert!(r.chat.background_tasks(&chat_id).is_ok());
    r.chat
        .send(&chat_id, InputKind::Operator, "three")
        .expect("another send");
    r.chat.stop(&chat_id).expect("stop works");
    assert!(!r.chat.is_alive(&chat_id).unwrap());
}
