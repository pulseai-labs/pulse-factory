//! AC-7: the control protocol subset, as the adapter writes it. `pf-claude-replay` plays a hand-made
//! scene, and a wrapper copies every line the adapter writes to a file first, so a test reads what
//! the CLI would have read.

mod common;

use std::time::Duration;

use common::scene::{self, Line};
use common::*;
use pf_engine::port::chat::{
    ChatAnswer, ChatError, ChatEvent, ChatPort, ContextUsage, InputKind, cap,
};
use serde_json::{Value, json};

fn scene_rig(
    name: &str,
    body: Vec<Line>,
    tweak: impl FnOnce(&mut pf_adapter_claude::ClaudeConfig),
) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let refs: Vec<(&str, Value)> = body.iter().map(|(t, m)| (*t, m.clone())).collect();
    let fixture = write_fixture(dir.path(), name, &refs);
    let mut r = rig_with(&fixture, false, true, tweak);
    r.extra = Some(dir); // the replay reads the fixture for the life of the rig
    r
}

fn cli_request(id: &str, subtype: &str, extra: Value) -> Line {
    let mut request = json!({"subtype": subtype});
    if let (Some(o), Some(e)) = (request.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    (
        "cli",
        json!({"type": "control_request", "request_id": id, "request": request}),
    )
}

/// The requests the adapter wrote, by subtype.
fn requests<'a>(lines: &'a [Value], subtype: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|l| l["type"] == "control_request" && l["request"]["subtype"] == subtype)
        .collect()
}

fn no_background_tasks_request(lines: &[Value]) {
    assert!(
        requests(lines, "background_tasks").is_empty(),
        "the adapter never sends the background_tasks control request (it moves tasks to the background)"
    );
}

fn response_to<'a>(lines: &'a [Value], id: &str) -> &'a Value {
    lines
        .iter()
        .find(|l| l["type"] == "control_response" && l["response"]["request_id"] == id)
        .unwrap_or_else(|| panic!("no response to {id} in {lines:#?}"))
}

// ------------------------------------------------------------------ initialize and send

#[test]
fn initialize_is_the_first_line_and_names_no_hooks_no_servers_and_no_per_task_stop() {
    let r = scene_rig("init_first", scene::open(), |_| {});
    let opened = r.open();
    let lines = r.wait_stdin(|l| !l.is_empty());
    assert_eq!(lines[0]["type"], "control_request");
    assert_eq!(
        lines[0]["request"],
        json!({"subtype": "initialize"}),
        "the request holds the subtype only"
    );
    assert!(!lines[0].to_string().contains("perTaskStopAffordance"));
    r.chat.stop(&opened.chat).unwrap();
}

#[test]
fn no_answer_to_initialize_in_init_seconds_fails_the_open_and_kills_the_process() {
    let r = scene_rig("no_answer", vec![scene::init_request()], |c| {
        c.init_seconds = 1
    });
    let started = std::time::Instant::now();
    let err = r.chat.open(&spec(r.work.path())).expect_err("no answer");
    assert!(matches!(err, ChatError::Failed(_)), "{err}");
    assert!(err.to_string().contains("not answered"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(6));
    let pid: u32 = std::fs::read_to_string(r.dir.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(wait_gone(pid), "the process was killed");
}

#[test]
fn send_writes_one_stamped_user_message() {
    let mut body = scene::open();
    body.push(scene::user("u1", "hello"));
    body.extend(scene::turn("u1", "hi", 0.1, 3, 2));
    let r = scene_rig("send_one", body, |_| {});
    let mut opened = r.open();
    r.chat
        .send(&opened.chat, InputKind::Operator, "hello there")
        .unwrap();
    collect_until(&mut opened.events, 10, |g| count_turn_results(g) == 1);
    let lines = r.wait_stdin(|l| l.iter().any(|x| x["type"] == "user"));
    let users: Vec<&Value> = lines.iter().filter(|l| l["type"] == "user").collect();
    assert_eq!(users.len(), 1, "one user message");
    assert_eq!(
        users[0]["message"],
        json!({"role": "user", "content": "hello there"})
    );
    assert!(users[0]["parent_tool_use_id"].is_null());
    assert!(
        is_uuid_v4(users[0]["uuid"].as_str().unwrap()),
        "stamped with a version-4 UUID"
    );
    no_background_tasks_request(&lines);
    r.chat.stop(&opened.chat).unwrap();
}

// ------------------------------------------------------------------ answers

fn question_input() -> Value {
    json!({"questions": [{"question": "Pick?", "header": "H", "multiSelect": false,
        "options": [{"label": "A", "description": "a"}, {"label": "B", "description": "b"}]}]})
}

#[test]
fn answer_writes_each_of_the_three_shapes_with_the_right_request_id_and_the_whole_input() {
    // An approval whose input is above the 16 KB cap: the event holds the capped text, and the
    // answer is written from the original.
    let big = json!({"command": "x".repeat(20_000)});
    let mut body = scene::open();
    body.push(scene::user("u1", "go"));
    body.push(scene::sys_init(&["u1"]));
    body.push(scene::can_use("a1", "Bash", big.clone()));
    body.push(scene::cli_answer("a1"));
    body.push(scene::can_use("a2", "Bash", json!({"command": "rm -rf x"})));
    body.push(scene::cli_answer("a2"));
    body.push(scene::can_use("q1", "AskUserQuestion", question_input()));
    body.push(scene::cli_answer("q1"));
    body.push(scene::can_use("c1", "Bash", json!({"command": "ls"})));
    body.push((
        "host",
        json!({"type": "control_cancel_request", "request_id": "c1"}),
    ));
    body.push(scene::delta("done"));
    body.push(scene::text("done"));
    body.push(scene::result(0.1, 10, 5, &["u1"]));
    let r = scene_rig("answers", body, |_| {});
    let mut opened = r.open();
    let id = opened.chat.clone();
    r.chat.send(&id, InputKind::Operator, "go").unwrap();
    let mut got = Vec::new();

    // a1: Allow. Refusals first: an unknown id, and `Answers` to an approval.
    got.extend(collect_until(&mut opened.events, 10, |g| {
        g.iter().any(
            |e| matches!(e, ChatEvent::ApprovalRequest { request_id, .. } if request_id == "a1"),
        )
    }));
    let Some(ChatEvent::ApprovalRequest {
        input,
        tool,
        reason,
        ..
    }) = got.last().cloned()
    else {
        panic!("{got:?}")
    };
    assert_eq!(tool, "Bash");
    assert_eq!(
        reason.as_deref(),
        Some("a description"),
        "the request's description is the reason"
    );
    assert!(
        input.truncated && input.bytes > 16_384,
        "the event input is capped: {}",
        input.bytes
    );
    assert_eq!(
        input,
        cap(&serde_json::to_string(&big).unwrap()),
        "the same cap as everywhere"
    );
    let lines_before = r.wait_stdin(|l| l.len() >= 2).len(); // initialize and the user message
    assert!(
        matches!(
            r.chat.answer(&id, "nope", &ChatAnswer::Allow),
            Err(ChatError::Refused(_))
        ),
        "an unknown id"
    );
    assert!(
        matches!(
            r.chat
                .answer(&id, "a1", &ChatAnswer::Answers(Default::default())),
            Err(ChatError::Refused(_))
        ),
        "Answers to an approval"
    );
    assert_eq!(
        r.stdin_lines().len(),
        lines_before,
        "a refusal writes nothing"
    );
    r.chat.answer(&id, "a1", &ChatAnswer::Allow).unwrap();
    assert!(
        matches!(
            r.chat.answer(&id, "a1", &ChatAnswer::Allow),
            Err(ChatError::Refused(_))
        ),
        "an answered request is closed"
    );

    // a2: Deny.
    got.extend(collect_until(&mut opened.events, 10, |g| {
        g.iter().any(
            |e| matches!(e, ChatEvent::ApprovalRequest { request_id, .. } if request_id == "a2"),
        )
    }));
    r.chat
        .answer(
            &id,
            "a2",
            &ChatAnswer::Deny {
                message: "not this".into(),
            },
        )
        .unwrap();

    // q1: Answers merged into the original input; Allow to a question is refused.
    got.extend(collect_until(&mut opened.events, 10, |g| {
        g.iter()
            .any(|e| matches!(e, ChatEvent::Question { request_id, .. } if request_id == "q1"))
    }));
    assert!(
        matches!(
            r.chat.answer(&id, "q1", &ChatAnswer::Allow),
            Err(ChatError::Refused(_))
        ),
        "Allow to a question"
    );
    r.chat
        .answer(
            &id,
            "q1",
            &ChatAnswer::Answers([("Pick?".to_string(), "B".to_string())].into()),
        )
        .unwrap();

    // c1: the CLI cancels it.
    got.extend(collect_until(&mut opened.events, 10, |g| {
        count_turn_results(g) == 1
    }));
    assert!(got.contains(&ChatEvent::RequestClosed {
        request_id: "c1".into()
    }));
    assert!(
        matches!(
            r.chat.answer(&id, "c1", &ChatAnswer::Allow),
            Err(ChatError::Refused(_))
        ),
        "a closed request"
    );

    let lines = r.wait_stdin(|l| {
        ["a1", "a2", "q1"].iter().all(|i| {
            l.iter()
                .any(|x| x["type"] == "control_response" && x["response"]["request_id"] == *i)
        })
    });
    let body_of = |id: &str| response_to(&lines, id)["response"]["response"].clone();
    assert_eq!(response_to(&lines, "a1")["response"]["subtype"], "success");
    assert_eq!(
        body_of("a1"),
        json!({"behavior": "allow", "updatedInput": big}),
        "the original, uncapped input"
    );
    assert_eq!(
        body_of("a2"),
        json!({"behavior": "deny", "message": "not this"})
    );
    let mut merged = question_input();
    merged["answers"] = json!({"Pick?": "B"});
    assert_eq!(
        body_of("q1"),
        json!({"behavior": "allow", "updatedInput": merged}),
        "answers merged into the input"
    );
    no_background_tasks_request(&lines);
    r.chat.stop(&id).unwrap();
}

// ------------------------------------------------------------------ interrupt

fn error_result(uuid: &str) -> Line {
    (
        "host",
        json!({"type": "result", "subtype": "error_during_execution", "is_error": true,
            "stop_reason": "tool_use", "duration_ms": 3, "total_cost_usd": 0.01,
            "modelUsage": {"m": {"inputTokens": 5, "outputTokens": 1, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}},
            "usage": {}, "user_message_uuids": [uuid]}),
    )
}

#[test]
fn interrupt_writes_one_request_the_turn_ends_and_a_second_interrupt_with_no_turn_is_a_no_op() {
    let mut body = scene::open();
    body.push(scene::user("u1", "sleep"));
    body.push(scene::sys_init(&["u1"]));
    body.push(scene::delta("working"));
    body.push(cli_request("req-2", "interrupt", json!({})));
    body.push(scene::control_ok("req-2", json!({"still_queued": []})));
    body.push(error_result("u1"));
    let r = scene_rig("interrupt", body, |_| {});
    let mut opened = r.open();
    let id = opened.chat.clone();

    r.chat.interrupt(&id).expect("no turn yet: a no-op");
    assert!(
        requests(&r.stdin_lines(), "interrupt").is_empty(),
        "nothing written with no turn"
    );

    r.chat.send(&id, InputKind::Operator, "sleep").unwrap();
    let mut got = collect_until(&mut opened.events, 10, |g| {
        g.iter().any(|e| matches!(e, ChatEvent::TextDelta { .. }))
    });
    r.chat.interrupt(&id).expect("interrupt");
    got.extend(collect_until(&mut opened.events, 10, |g| {
        count_turn_results(g) == 1
    }));
    let Some(ChatEvent::TurnResult { is_error, stop, .. }) = got.last().cloned() else {
        panic!("{got:?}")
    };
    assert!(is_error, "the interrupted turn is an error result");
    assert_eq!(
        stop, "error_during_execution",
        "the subtype, not the misleading stop_reason tool_use"
    );
    assert!(r.chat.is_alive(&id).unwrap(), "the process lives on");

    r.chat
        .interrupt(&id)
        .expect("a second interrupt, no turn: a no-op");
    std::thread::sleep(Duration::from_millis(150));
    let lines = r.wait_stdin(|l| !requests(l, "interrupt").is_empty());
    assert_eq!(
        requests(&lines, "interrupt").len(),
        1,
        "one interrupt request in all"
    );
    no_background_tasks_request(&lines);
    r.chat.stop(&id).unwrap();
}

// ------------------------------------------------------------------ options and usage

#[test]
fn set_option_of_model_effort_and_mode_sends_the_three_control_requests() {
    let mut body = scene::open();
    body.extend([
        cli_request("req-2", "set_model", json!({})),
        scene::control_ok("req-2", json!({"model": "claude-sonnet-5-5"})),
        cli_request("req-3", "apply_flag_settings", json!({})),
        scene::control_ok("req-3", json!({})),
        cli_request("req-4", "set_permission_mode", json!({})),
        scene::control_ok("req-4", json!({"mode": "plan"})),
        cli_request("req-5", "get_context_usage", json!({})),
        scene::control_ok(
            "req-5",
            json!({"totalTokens": 28005, "maxTokens": 1_000_000}),
        ),
    ]);
    let r = scene_rig("options", body, |_| {});
    let opened = r.open();
    let id = &opened.chat;
    r.chat.set_option(id, "model", "claude-sonnet-5-5").unwrap();
    r.chat.set_option(id, "effort", "low").unwrap();
    r.chat.set_option(id, "permission_mode", "plan").unwrap();
    let usage = r.chat.context_usage(id).unwrap();
    assert_eq!(
        usage,
        ContextUsage {
            used_tokens: 28005,
            max_tokens: 1_000_000
        }
    );
    assert!(r.chat.background_tasks(id).unwrap().is_empty());
    // The wrapper's `tee` may write its file just after the replay has read the line.
    let lines = r.wait_stdin(|l| l.len() >= 5);
    let sent: Vec<&Value> = lines.iter().skip(1).map(|l| &l["request"]).collect();
    assert_eq!(
        sent,
        [
            &json!({"subtype": "set_model", "model": "claude-sonnet-5-5"}),
            &json!({"subtype": "apply_flag_settings", "settings": {"effortLevel": "low"}}),
            &json!({"subtype": "set_permission_mode", "mode": "plan"}),
            &json!({"subtype": "get_context_usage", "detail": "summary"}),
        ]
    );
    no_background_tasks_request(&lines);
    // A bad value or a non-live option is refused before any request is written.
    for (key, value) in [
        ("model", "bad value"),
        ("effort", "extreme"),
        ("permission_mode", "yolo"),
        ("artifacts", "off"),
        ("nope", "x"),
    ] {
        assert!(
            matches!(
                r.chat.set_option(id, key, value),
                Err(ChatError::Refused(_))
            ),
            "{key}={value}"
        );
    }
    assert_eq!(r.stdin_lines().len(), lines.len(), "refusals write nothing");
    r.chat.stop(id).unwrap();
}

#[test]
fn an_error_response_gives_failed_with_its_text() {
    let mut body = scene::open();
    body.extend([
        cli_request("req-2", "get_context_usage", json!({})),
        scene::control_error("req-2", "usage is not available now"),
    ]);
    let r = scene_rig("error_response", body, |_| {});
    let opened = r.open();
    let err = r
        .chat
        .context_usage(&opened.chat)
        .expect_err("an error response");
    assert!(matches!(err, ChatError::Failed(_)), "{err}");
    assert!(
        err.to_string().contains("usage is not available now"),
        "{err}"
    );
    r.chat.stop(&opened.chat).unwrap();
}

#[test]
fn no_response_in_control_seconds_gives_failed() {
    let mut body = scene::open();
    body.push(cli_request("req-2", "get_context_usage", json!({})));
    let r = scene_rig("no_response", body, |c| c.control_seconds = 1);
    let opened = r.open();
    let started = std::time::Instant::now();
    let err = r.chat.context_usage(&opened.chat).expect_err("no response");
    assert!(matches!(err, ChatError::Failed(_)), "{err}");
    assert!(err.to_string().contains("no response"), "{err}");
    assert!(started.elapsed() >= Duration::from_millis(900));
    r.chat.stop(&opened.chat).unwrap();
}

// ------------------------------------------------------------------ what the CLI asks of the host

#[test]
fn inbound_hook_callback_mcp_message_and_unknown_requests_get_an_error_response() {
    let mut body = scene::open();
    for (id, subtype) in [
        ("h1", "hook_callback"),
        ("h2", "mcp_message"),
        ("h3", "elicitation"),
        ("h4", "request_user_dialog"),
        ("h5", "frobnicate"),
    ] {
        body.push((
            "host",
            json!({"type": "control_request", "request_id": id, "request": {"subtype": subtype}}),
        ));
    }
    let r = scene_rig("inbound", body, |_| {});
    let opened = r.open();
    let lines = r.wait_stdin(|l| l.iter().filter(|x| x["type"] == "control_response").count() == 5);
    for (id, subtype) in [
        ("h1", "hook_callback"),
        ("h2", "mcp_message"),
        ("h3", "elicitation"),
        ("h4", "request_user_dialog"),
        ("h5", "frobnicate"),
    ] {
        let resp = &response_to(&lines, id)["response"];
        assert_eq!(resp["subtype"], "error", "{subtype}");
        assert!(
            resp["error"].as_str().unwrap().contains(subtype),
            "the error names the subtype: {resp}"
        );
    }
    // No event came of them.
    r.chat.stop(&opened.chat).unwrap();
}
