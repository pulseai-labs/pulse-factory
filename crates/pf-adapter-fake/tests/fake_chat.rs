//! AC-3: `FakeChat` follows the chat port exactly: the same event order (`check_order` passes on
//! every sequence), the same usage shape, the same cap and the same refusals. Every behaviour is
//! driven by the message text.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use pf_adapter_fake::FakeChat;
use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::chat::{
    CAP_BYTES, ChatAnswer, ChatError, ChatEvent, ChatEvents, ChatId, ChatOpen, ChatOption,
    ChatPort, InputKind, MidTurn, TaskEnd, TurnCause, check_order,
};

fn open_spec() -> ChatOpen {
    ChatOpen {
        cwd: "/work/tree".into(),
        resume: None,
        instructions: "be brief".into(),
        model: "fake-model".into(),
        effort: "high".into(),
        permission_mode: "bypass".into(),
        env: BTreeMap::new(),
        options: BTreeMap::new(),
        deny_commands: Vec::new(),
    }
}

struct Chat {
    port: FakeChat,
    id: ChatId,
    rx: ChatEvents,
    log: Vec<ChatEvent>,
}

impl Chat {
    fn new() -> Chat {
        Chat::with(FakeChat::new())
    }

    fn with(port: FakeChat) -> Chat {
        let opened = port.open(&open_spec()).expect("open");
        Chat {
            port,
            id: opened.chat,
            rx: opened.events,
            log: Vec::new(),
        }
    }

    fn say(&mut self, text: &str) -> Vec<ChatEvent> {
        self.port
            .send(&self.id, InputKind::Operator, text)
            .expect("send");
        self.until_result()
    }

    /// Read until a `TurnResult` or `Exited`, within 10 s. Everything read is also logged.
    fn until_result(&mut self) -> Vec<ChatEvent> {
        self.until(|e| matches!(e, ChatEvent::TurnResult { .. } | ChatEvent::Exited { .. }))
    }

    fn until(&mut self, stop: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
        let end = Instant::now() + Duration::from_secs(10);
        let mut got = Vec::new();
        while Instant::now() < end {
            match self.rx.try_recv() {
                Ok(e) => {
                    let hit = stop(&e);
                    self.log.push(e.clone());
                    got.push(e);
                    if hit {
                        return got;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        panic!("no stop event in 10 s; got {got:?}");
    }

    /// The events that arrive within `ms` more, whatever they are.
    fn drain_for(&mut self, ms: u64) -> Vec<ChatEvent> {
        let end = Instant::now() + Duration::from_millis(ms);
        let mut got = Vec::new();
        while Instant::now() < end {
            match self.rx.try_recv() {
                Ok(e) => {
                    self.log.push(e.clone());
                    got.push(e);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        got
    }

    fn checked(&self) {
        check_order(&self.log).unwrap_or_else(|e| panic!("{e}\n{:#?}", self.log));
    }
}

fn result_of(events: &[ChatEvent]) -> (TurnCause, Usage, bool, String, Option<u32>) {
    events
        .iter()
        .find_map(|e| match e {
            ChatEvent::TurnResult {
                cause,
                usage,
                is_error,
                stop,
                cache_ttl_seconds,
                ..
            } => Some((*cause, *usage, *is_error, stop.clone(), *cache_ttl_seconds)),
            _ => None,
        })
        .expect("a TurnResult")
}

fn texts(events: &[ChatEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn deltas(events: &[ChatEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------ the plain reply

#[test]
fn a_plain_message_gets_a_streamed_reply_and_an_exact_usage() {
    let mut c = Chat::new();
    let events = c.say("hello");
    assert_eq!(
        events.first(),
        Some(&ChatEvent::TurnStarted {
            cause: TurnCause::Operator
        })
    );
    let d = deltas(&events);
    assert_eq!(
        d,
        vec!["fake rep", "ly: hell", "o"],
        "deltas of at most 8 characters"
    );
    assert!(d.iter().all(|x| x.chars().count() <= 8));
    assert_eq!(texts(&events), vec!["fake reply: hello"]);
    let (cause, usage, is_error, stop, ttl) = result_of(&events);
    assert_eq!(cause, TurnCause::Operator);
    assert_eq!(
        usage,
        Usage {
            input_tokens: 5,
            output_tokens: 17,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_micros: 3 * 5 + 15 * 17,
        }
    );
    assert!(!is_error);
    assert_eq!(stop, "end_turn");
    assert_eq!(ttl, Some(300));
    c.checked();
    // Characters, not bytes: a multi-byte message counts its characters.
    let events = c.say("héé");
    assert_eq!(result_of(&events).1.input_tokens, 3);
    c.checked();
}

#[test]
fn a_notice_starts_a_turn_with_the_notice_cause() {
    let mut c = Chat::new();
    c.port
        .send(
            &c.id,
            InputKind::Notice,
            "A background task finished: t1 (completed): ok",
        )
        .unwrap();
    let events = c.until_result();
    assert_eq!(
        events.first(),
        Some(&ChatEvent::TurnStarted {
            cause: TurnCause::Notice
        })
    );
    assert_eq!(result_of(&events).0, TurnCause::Notice);
    c.checked();
}

#[test]
fn a_tool_call_runs_and_a_big_output_is_capped_with_its_original_size() {
    let mut c = Chat::new();
    let events = c.say("fake:tool");
    let started = events
        .iter()
        .position(|e| matches!(e, ChatEvent::ToolStarted { .. }))
        .unwrap();
    let finished = events
        .iter()
        .position(|e| matches!(e, ChatEvent::ToolFinished { .. }))
        .unwrap();
    assert!(started < finished);
    let ChatEvent::ToolFinished {
        output, is_error, ..
    } = &events[finished]
    else {
        unreachable!()
    };
    assert_eq!(
        (output.text.as_str(), output.truncated, *is_error),
        ("ok", false, false)
    );
    c.checked();

    let events = c.say("fake:big");
    let ChatEvent::ToolFinished { output, .. } = events
        .iter()
        .find(|e| matches!(e, ChatEvent::ToolFinished { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert!(output.truncated);
    assert_eq!(output.bytes, 20000);
    assert!(output.text.len() <= CAP_BYTES);
    assert!(output.text.ends_with("…[truncated: 20000 bytes in all]"));
    c.checked();
}

// ------------------------------------------------------------------ approvals and questions

#[test]
fn an_approval_waits_for_its_answer_and_allow_and_deny_differ() {
    for (answer, expect_tool, reply) in [
        (ChatAnswer::Allow, true, "approved"),
        (
            ChatAnswer::Deny {
                message: "no way".into(),
            },
            false,
            "denied: no way",
        ),
    ] {
        let mut c = Chat::new();
        c.port
            .send(&c.id, InputKind::Operator, "fake:approval")
            .unwrap();
        let before = c.until(|e| matches!(e, ChatEvent::ApprovalRequest { .. }));
        let ChatEvent::ApprovalRequest { request_id, .. } = before.last().unwrap().clone() else {
            unreachable!()
        };
        // The turn waits with no time limit: nothing comes while it waits.
        assert!(c.drain_for(300).is_empty(), "the turn must wait");
        c.port.answer(&c.id, &request_id, &answer).unwrap();
        let after = c.until_result();
        assert_eq!(
            after
                .iter()
                .any(|e| matches!(e, ChatEvent::ToolStarted { .. })),
            expect_tool,
            "{after:?}"
        );
        assert_eq!(texts(&after), vec![reply]);
        c.checked();
    }
}

#[test]
fn a_question_is_answered_with_answers_and_wrong_answers_are_refused() {
    let mut c = Chat::new();
    c.port
        .send(&c.id, InputKind::Operator, "fake:question")
        .unwrap();
    let before = c.until(|e| matches!(e, ChatEvent::Question { .. }));
    let ChatEvent::Question { request_id, items } = before.last().unwrap().clone() else {
        unreachable!()
    };
    assert_eq!(items.len(), 1);
    let labels: Vec<&str> = items[0].options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["A", "B"]);
    // Allow and Deny do not answer a question; an unknown request id is refused.
    assert!(matches!(
        c.port.answer(&c.id, &request_id, &ChatAnswer::Allow),
        Err(ChatError::Refused(_))
    ));
    assert!(matches!(
        c.port.answer(
            &c.id,
            &request_id,
            &ChatAnswer::Deny {
                message: "x".into()
            }
        ),
        Err(ChatError::Refused(_))
    ));
    assert!(matches!(
        c.port.answer(&c.id, "nope", &ChatAnswer::Allow),
        Err(ChatError::Refused(_))
    ));
    let mut answers = BTreeMap::new();
    answers.insert(items[0].text.clone(), "B".to_string());
    c.port
        .answer(&c.id, &request_id, &ChatAnswer::Answers(answers.clone()))
        .unwrap();
    let after = c.until_result();
    assert_eq!(texts(&after), vec!["answer: B"]);
    // A closed request cannot be answered again.
    assert!(matches!(
        c.port
            .answer(&c.id, &request_id, &ChatAnswer::Answers(answers)),
        Err(ChatError::Refused(_))
    ));
    c.checked();
}

#[test]
fn answers_to_an_approval_are_refused() {
    let mut c = Chat::new();
    c.port
        .send(&c.id, InputKind::Operator, "fake:approval")
        .unwrap();
    let before = c.until(|e| matches!(e, ChatEvent::ApprovalRequest { .. }));
    let ChatEvent::ApprovalRequest { request_id, .. } = before.last().unwrap().clone() else {
        unreachable!()
    };
    let mut answers = BTreeMap::new();
    answers.insert("q".to_string(), "a".to_string());
    assert!(matches!(
        c.port
            .answer(&c.id, &request_id, &ChatAnswer::Answers(answers)),
        Err(ChatError::Refused(_))
    ));
    // The refusal leaves the request open.
    c.port
        .answer(&c.id, &request_id, &ChatAnswer::Allow)
        .unwrap();
    c.until_result();
    c.checked();
}

// ------------------------------------------------------------------ background tasks

#[test]
fn a_task_starts_finishes_and_then_a_task_turn_follows() {
    let mut c = Chat::new();
    let events = c.say("fake:task 50");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::TaskStarted { .. }))
    );
    // The task is listed until it finishes.
    assert_eq!(c.port.background_tasks(&c.id).unwrap().len(), 1);
    let rest = c.until_result();
    assert!(
        rest.iter().any(|e| matches!(
            e,
            ChatEvent::TaskFinished {
                end: TaskEnd::Completed,
                ..
            }
        )),
        "{rest:?}"
    );
    assert_eq!(
        rest.first(),
        Some(&ChatEvent::TaskFinished {
            task_id: match &rest[0] {
                ChatEvent::TaskFinished { task_id, .. } => task_id.clone(),
                _ => unreachable!(),
            },
            end: TaskEnd::Completed,
            summary: "done".into(),
        })
    );
    assert!(rest.iter().any(|e| matches!(
        e,
        ChatEvent::TurnStarted {
            cause: TurnCause::Task
        }
    )));
    assert_eq!(texts(&rest), vec!["task finished"]);
    assert_eq!(result_of(&rest).0, TurnCause::Task);
    assert!(c.port.background_tasks(&c.id).unwrap().is_empty());
    c.checked();
}

#[test]
fn two_tasks_finishing_in_a_running_turn_give_two_task_turns_the_port_accepts() {
    let mut c = Chat::new();
    c.port
        .send(&c.id, InputKind::Operator, "fake:task 400")
        .expect("send");
    c.until_result();
    c.port
        .send(&c.id, InputKind::Operator, "fake:task 400")
        .expect("send");
    c.until_result();
    // Both tasks finish while this turn runs, so two Task turns queue behind it.
    c.port
        .send(&c.id, InputKind::Operator, "fake:slow 1500")
        .expect("send");
    let mut rest = c.until_result();
    for _ in 0..2 {
        rest.extend(c.until_result());
    }
    let task_turns = rest
        .iter()
        .filter(|e| {
            matches!(
                e,
                ChatEvent::TurnStarted {
                    cause: TurnCause::Task
                }
            )
        })
        .count();
    assert_eq!(task_turns, 2, "{rest:?}");
    c.checked();
}

// ------------------------------------------------------------------ interrupt, fail, exit

#[test]
fn interrupt_ends_a_slow_turn_with_an_interrupted_error_result() {
    let mut c = Chat::new();
    c.port
        .send(&c.id, InputKind::Operator, "fake:slow 2000")
        .unwrap();
    c.until(|e| matches!(e, ChatEvent::TextDelta { .. }));
    let t = Instant::now();
    c.port.interrupt(&c.id).unwrap();
    let rest = c.until_result();
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "the interrupt ended the turn early"
    );
    let (_, _, is_error, stop, _) = result_of(&rest);
    assert!(is_error);
    assert_eq!(stop, "interrupted");
    // An interrupt with no turn is a no-op, and the next send starts a turn.
    c.port.interrupt(&c.id).unwrap();
    assert!(c.port.is_alive(&c.id).unwrap());
    let next = c.say("again");
    assert_eq!(texts(&next), vec!["fake reply: again"]);
    c.checked();
}

#[test]
fn a_failed_turn_and_an_exit() {
    let mut c = Chat::new();
    let events = c.say("fake:fail");
    let (_, _, is_error, _, _) = result_of(&events);
    assert!(is_error);
    c.checked();
    let events = c.say("fake:exit");
    assert!(
        matches!(events.last(), Some(ChatEvent::Exited { code: Some(1), .. })),
        "{events:?}"
    );
    assert!(!c.port.is_alive(&c.id).unwrap());
    c.checked();
    // A send after the exit is refused.
    assert!(c.port.send(&c.id, InputKind::Operator, "x").is_err());
}

// ------------------------------------------------------------------ a send during a turn

#[test]
fn a_send_during_a_turn_steers_it_under_steer() {
    let port = FakeChat::new();
    assert_eq!(
        port.mid_turn(),
        MidTurn::Steer,
        "the mode the qualification selected"
    );
    let mut c = Chat::with(port);
    c.port
        .send(&c.id, InputKind::Operator, "fake:slow 600")
        .unwrap();
    c.until(|e| matches!(e, ChatEvent::TextDelta { .. }));
    c.port.send(&c.id, InputKind::Operator, "more").unwrap();
    let rest = c.until_result();
    assert!(
        texts(&rest).contains(&"steered: more".to_string()),
        "{rest:?}"
    );
    let all = &c.log;
    assert_eq!(
        all.iter()
            .filter(|e| matches!(e, ChatEvent::TurnStarted { .. }))
            .count(),
        1,
        "a steered send adds no TurnStarted"
    );
    let (_, usage, is_error, _, _) = result_of(&rest);
    assert!(!is_error);
    assert_eq!(
        usage.input_tokens,
        ("fake:slow 600".len() + "more".len()) as u64
    );
    assert_eq!(
        usage.output_tokens,
        ("fake reply: fake:slow 600".len() + "steered: more".len()) as u64
    );
    assert_eq!(
        c.drain_for(200)
            .iter()
            .filter(|e| matches!(e, ChatEvent::TurnResult { .. }))
            .count(),
        0,
        "one TurnResult covers both"
    );
    c.checked();
}

#[test]
fn a_send_during_a_turn_becomes_the_next_turn_under_next_turn() {
    let port = FakeChat::with_mid_turn(MidTurn::NextTurn);
    assert_eq!(port.mid_turn(), MidTurn::NextTurn);
    let mut c = Chat::with(port);
    c.port
        .send(&c.id, InputKind::Operator, "fake:slow 400")
        .unwrap();
    c.until(|e| matches!(e, ChatEvent::TextDelta { .. }));
    c.port.send(&c.id, InputKind::Operator, "later").unwrap();
    let first = c.until_result();
    assert_eq!(result_of(&first).3, "end_turn");
    let second = c.until_result();
    assert_eq!(
        second.first(),
        Some(&ChatEvent::TurnStarted {
            cause: TurnCause::Operator
        })
    );
    assert_eq!(texts(&second), vec!["fake reply: later"]);
    assert_eq!(
        c.log
            .iter()
            .filter(|e| matches!(e, ChatEvent::TurnResult { .. }))
            .count(),
        2
    );
    assert_eq!(
        result_of(&first).1.input_tokens,
        "fake:slow 400".len() as u64,
        "the first turn does not count it"
    );
    c.checked();
}

// ------------------------------------------------------------------ options, context, resume

#[test]
fn live_options_change_and_bad_changes_are_refused() {
    let c = Chat::new();
    let options = c.port.options();
    let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["model", "effort", "permission_mode", "artifacts"]);
    let by = |id: &str| options.iter().find(|o| o.id == id).unwrap().clone();
    assert_eq!(
        (
            by("model").default.as_str(),
            by("model").free,
            by("model").live
        ),
        ("fake-model", true, true)
    );
    assert_eq!(
        (
            by("effort").default.as_str(),
            by("effort").free,
            by("effort").live
        ),
        ("high", true, true)
    );
    assert_eq!(
        (
            by("permission_mode").default.as_str(),
            by("permission_mode").free,
            by("permission_mode").live
        ),
        ("bypass", true, true)
    );
    let a = by("artifacts");
    assert_eq!((a.default.as_str(), a.live, a.free), ("on", false, false));
    assert_eq!(a.values, vec!["on", "off"]);

    for id in ["model", "effort", "permission_mode"] {
        c.port
            .set_option(&c.id, id, "another")
            .unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    let too_long = "x".repeat(65);
    for bad in ["", too_long.as_str(), "has space", "tab\there", "nul\u{0}x"] {
        assert!(
            matches!(
                c.port.set_option(&c.id, "model", bad),
                Err(ChatError::Refused(_))
            ),
            "{bad:?}"
        );
    }
    assert!(
        c.port.set_option(&c.id, "model", &"y".repeat(64)).is_ok(),
        "64 characters pass"
    );
    assert!(matches!(
        c.port.set_option(&c.id, "nope", "x"),
        Err(ChatError::Refused(_))
    ));
    assert!(
        matches!(
            c.port.set_option(&c.id, "artifacts", "off"),
            Err(ChatError::Refused(_))
        ),
        "not live"
    );
    assert!(matches!(
        c.port.set_option(&ChatId("unknown".into()), "model", "x"),
        Err(ChatError::Refused(_))
    ));
}

#[test]
fn a_strict_option_list_refuses_values_outside_it() {
    let strict = vec![ChatOption {
        id: "model".into(),
        label: "Model".into(),
        values: vec!["m1".into(), "m2".into()],
        default: "m1".into(),
        free: false,
        live: true,
    }];
    let port = FakeChat::with_options(strict);
    assert_eq!(port.options().len(), 1);
    let mut spec = open_spec();
    spec.model = "m3".into();
    assert!(matches!(port.open(&spec), Err(ChatError::Refused(_))));
    spec.model = "m2".into();
    spec.effort = "high".into();
    // With no `effort` option declared, the open still names one; the strict list has none to check.
    let opened = port.open(&spec);
    assert!(opened.is_ok(), "{:?}", opened.err());
    let id = opened.unwrap().chat;
    assert!(port.set_option(&id, "model", "m2").is_ok());
    assert!(matches!(
        port.set_option(&id, "model", "m3"),
        Err(ChatError::Refused(_))
    ));
}

#[test]
fn context_usage_grows_with_each_turn() {
    let mut c = Chat::new();
    assert_eq!(c.port.context_usage(&c.id).unwrap().used_tokens, 0);
    let events = c.say("hello");
    let usage = result_of(&events).1;
    let first = c.port.context_usage(&c.id).unwrap();
    assert_eq!(first.used_tokens, usage.input_tokens + usage.output_tokens);
    assert_eq!(first.max_tokens, 200_000);
    c.say("hello again");
    let second = c.port.context_usage(&c.id).unwrap();
    assert!(second.used_tokens > first.used_tokens);
}

#[test]
fn open_resumes_its_own_reference_and_refuses_another() {
    let port = FakeChat::new();
    let a = port.open(&open_spec()).unwrap();
    assert_eq!(a.runtime_version, "fake");
    assert!(a.qualified);
    assert!(a.session.0.starts_with("fake-session-"), "{:?}", a.session);
    let b = port.open(&open_spec()).unwrap();
    assert_ne!(a.session, b.session);
    assert_ne!(a.chat, b.chat);
    port.stop(&a.chat).unwrap();
    let mut spec = open_spec();
    spec.resume = Some(a.session.clone());
    let resumed = port.open(&spec).unwrap();
    assert_eq!(resumed.session, a.session, "a resume keeps the session");
    spec.resume = Some(SessionRef("not-mine".into()));
    assert!(matches!(port.open(&spec), Err(ChatError::ResumeFailed(_))));
    assert_eq!(port.open_count(), 3, "a refused open is not counted");
}

#[test]
fn open_refuses_a_reserved_or_malformed_env_name_and_an_unknown_option() {
    let port = FakeChat::new();
    for name in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "CLAUDECODE",
        "CLAUDE_CODE_ARTIFACT",
        "PF_SEAT_TOKEN",
        "ORCA_AGENT_HOOK_TOKEN",
        "HERDR_SOCKET_PATH",
        "DISABLE_AUTOUPDATER",
        "lower_case",
        "HAS-DASH",
        "",
    ] {
        let mut spec = open_spec();
        spec.env.insert(name.to_string(), "v".into());
        let err = port.open(&spec).expect_err(name);
        assert!(matches!(err, ChatError::Refused(_)), "{name}: {err}");
    }
    let mut spec = open_spec();
    spec.env
        .insert("SEAT_MODS_ROLE".into(), "orchestrator".into());
    spec.env.insert("MY_VAR_2".into(), "v".into());
    assert!(port.open(&spec).is_ok(), "an allowed name passes");
    let mut spec = open_spec();
    spec.options.insert("unknown".into(), "x".into());
    assert!(matches!(port.open(&spec), Err(ChatError::Refused(_))));
    for key in ["model", "effort", "permission_mode"] {
        let mut spec = open_spec();
        spec.options.insert(key.into(), "x".into());
        assert!(
            matches!(port.open(&spec), Err(ChatError::Refused(_))),
            "{key} has its own field and is refused in the options map"
        );
    }
    let mut spec = open_spec();
    spec.options.insert("artifacts".into(), "maybe".into());
    assert!(matches!(port.open(&spec), Err(ChatError::Refused(_))));
    spec.options.insert("artifacts".into(), "off".into());
    assert!(port.open(&spec).is_ok());
}

// ------------------------------------------------------------------ the receiver, stop, liveness

#[test]
fn a_dropped_receiver_mid_turn_fails_no_later_call() {
    let port = FakeChat::new();
    let opened = port.open(&open_spec()).unwrap();
    let id = opened.chat;
    port.send(&id, InputKind::Operator, "fake:slow 200")
        .unwrap();
    drop(opened.events);
    std::thread::sleep(Duration::from_millis(400));
    assert!(port.send(&id, InputKind::Operator, "still works").is_ok());
    assert!(port.interrupt(&id).is_ok());
    assert!(port.background_tasks(&id).is_ok());
    assert!(port.context_usage(&id).is_ok());
    assert!(port.is_alive(&id).unwrap(), "only stop ends the process");
    assert!(port.set_option(&id, "model", "x").is_ok());
    assert!(port.stop(&id).is_ok());
    assert!(!port.is_alive(&id).unwrap());
}

#[test]
fn stop_twice_gives_one_exited_and_an_unknown_chat_is_not_alive() {
    let mut c = Chat::new();
    assert!(c.port.is_alive(&c.id).unwrap());
    c.say("hello");
    c.port.stop(&c.id).unwrap();
    c.port.stop(&c.id).unwrap();
    assert!(!c.port.is_alive(&c.id).unwrap());
    let rest = c.drain_for(200);
    let exits: Vec<&ChatEvent> = rest
        .iter()
        .filter(|e| matches!(e, ChatEvent::Exited { .. }))
        .collect();
    assert_eq!(exits.len(), 1, "{rest:?}");
    assert_eq!(
        exits[0],
        &ChatEvent::Exited {
            code: None,
            reason: "stopped".into()
        }
    );
    assert_eq!(c.port.stop_count(), 1, "the repeat ended no process");
    // Reads still answer after a stop.
    assert!(c.port.background_tasks(&c.id).unwrap().is_empty());
    assert!(c.port.context_usage(&c.id).is_ok());
    c.checked();

    let unknown = ChatId("never-opened".into());
    assert_eq!(c.port.is_alive(&unknown), Ok(false));
    assert_eq!(c.port.stop(&unknown), Ok(()));
    assert_eq!(c.port.process_id(&c.id), None, "the fake runs no process");
}

#[test]
fn the_counters_count_opens_stops_and_sent_messages() {
    let mut c = Chat::new();
    assert_eq!(c.port.open_count(), 1);
    assert_eq!(c.port.stop_count(), 0);
    c.say("one");
    c.port.send(&c.id, InputKind::Notice, "two").unwrap();
    c.until_result();
    assert_eq!(
        c.port.sent(&c.id),
        vec!["one".to_string(), "two".to_string()]
    );
    assert!(c.port.sent(&ChatId("x".into())).is_empty());
}

// ------------------------------------------------------------------ the deny list (ADR-0016)

/// The fifteen built-in deny patterns of `r1.s4.w4`.
const BUILT_IN_DENY: [&str; 15] = [
    "git push -f*",
    "git push * -f*",
    "git push --force*",
    "git push * --force*",
    "git push * +*",
    "git push --delete",
    "git push * --delete",
    "git push -d",
    "git push * -d",
    "git branch -D",
    "git branch * -D",
    "gh pr merge",
    "gh -R * pr merge",
    "gh --repo * pr merge",
    "gh --repo=* pr merge",
];

#[test]
fn open_refuses_a_bad_deny_entry_and_names_it() {
    let port = FakeChat::new();
    for bad in ["git push * :*", "git (push", " git", "git  push"] {
        let mut spec = open_spec();
        spec.deny_commands = vec!["npm publish".into(), bad.into()];
        match port.open(&spec) {
            Err(ChatError::Refused(why)) => assert!(why.contains(bad.trim()), "{why}"),
            other => panic!("{bad:?} must be refused, got {other:?}"),
        }
    }
    assert_eq!(port.open_count(), 0, "a refused open is not counted");
    let mut dup = open_spec();
    dup.deny_commands = vec!["npm publish".into(), "npm publish".into()];
    assert!(matches!(port.open(&dup), Err(ChatError::Refused(_))));
}

#[test]
fn open_takes_the_fifteen_built_ins_and_an_empty_list() {
    let port = FakeChat::new();
    let mut spec = open_spec();
    spec.deny_commands = BUILT_IN_DENY.iter().map(ToString::to_string).collect();
    port.open(&spec).expect("the built-ins open");
    spec.deny_commands = Vec::new();
    port.open(&spec).expect("an empty list opens");
    assert_eq!(port.open_count(), 2);
}
