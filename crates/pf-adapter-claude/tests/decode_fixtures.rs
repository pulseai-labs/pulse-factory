//! The decoder over every recording: the events pass the grammar, and the recorded facts come out.
//! These are the facts ADR-0014 lists as verified for CLI 2.1.319. The adapter reads the same
//! recordings through the replay binary in `stream_replay.rs`; here the decoder runs alone.

mod common;

use common::*;
use pf_adapter_claude::decode::{Decoder, Out, decode_recording};
use pf_engine::port::agent::Usage;
use pf_engine::port::chat::{CAP_BYTES, ChatEvent, InputKind, TaskEnd, TurnCause, check_order};
use serde_json::{Value, json};

fn events(name: &str) -> Vec<ChatEvent> {
    let f = load_fixture(name);
    decode_recording("2.1.319", "", &f.body).0
}

fn names(events: &[ChatEvent]) -> Vec<&'static str> {
    events.iter().map(ChatEvent::name).collect()
}

#[test]
fn every_recording_passes_the_event_grammar() {
    for f in all_fixtures() {
        let (events, _) = decode_recording("2.1.319", "", &f.body);
        check_order(&events).unwrap_or_else(|e| panic!("{}: {e}", f.name));
    }
}

#[test]
fn in_every_recording_the_deltas_of_a_block_join_to_its_text() {
    // A recorded fact, not a rule of `check_order`: a steered or multi-block turn would make a rule
    // fragile. If a future recording breaks it, the contract's statement changes, not the grammar.
    for f in all_fixtures() {
        let (events, _) = decode_recording("2.1.319", "", &f.body);
        let mut joined = String::new();
        for e in &events {
            match e {
                ChatEvent::TextDelta { text } => joined.push_str(text),
                ChatEvent::Text { text } => {
                    assert_eq!(
                        &joined, text,
                        "{}: the deltas before a Text join to it",
                        f.name
                    );
                    joined.clear();
                }
                _ => {}
            }
        }
        assert!(
            joined.is_empty(),
            "{}: deltas left over with no Text",
            f.name
        );
    }
}

#[test]
fn a_plain_turn_gives_deltas_one_text_and_one_result_per_turn() {
    let ev = events("plain_turn");
    assert_eq!(
        names(&ev).iter().filter(|n| **n == "TurnStarted").count(),
        3,
        "three turns in one process"
    );
    assert_eq!(names(&ev).iter().filter(|n| **n == "Text").count(), 3);
    assert!(ev.iter().all(|e| !matches!(e, ChatEvent::Exited { .. })));
}

#[test]
fn per_turn_usage_is_the_difference_of_the_cumulative_totals() {
    let f = load_fixture("plain_turn");
    let totals: Vec<f64> = f
        .body
        .iter()
        .filter(|(to, m)| to == "host" && m["type"] == "result")
        .map(|(_, m)| m["total_cost_usd"].as_f64().unwrap())
        .collect();
    assert_eq!(totals.len(), 3);
    let micros: Vec<u64> = totals.iter().map(|t| (t * 1e6).round() as u64).collect();
    let costs: Vec<u64> = events("plain_turn")
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnResult { usage, .. } => Some(usage.cost_micros),
            _ => None,
        })
        .collect();
    assert_eq!(
        costs,
        [micros[0], micros[1] - micros[0], micros[2] - micros[1]]
    );
    assert_eq!(
        costs.iter().sum::<u64>(),
        micros[2],
        "the session total is the sum of its turns"
    );
}

/// The cumulative totals of a recording's last `result`: cost in micros and the four token sums.
fn last_totals(name: &str) -> [u64; 5] {
    let f = load_fixture(name);
    let msg = f
        .body
        .iter()
        .rev()
        .find(|(to, m)| to == "host" && m["type"] == "result")
        .map(|(_, m)| m.clone())
        .unwrap();
    let mut t = [
        (msg["total_cost_usd"].as_f64().unwrap() * 1e6).round() as u64,
        0,
        0,
        0,
        0,
    ];
    for m in msg["modelUsage"].as_object().unwrap().values() {
        t[1] += m["inputTokens"].as_u64().unwrap_or(0);
        t[2] += m["outputTokens"].as_u64().unwrap_or(0);
        t[3] += m["cacheReadInputTokens"].as_u64().unwrap_or(0);
        t[4] += m["cacheCreationInputTokens"].as_u64().unwrap_or(0);
    }
    t
}

#[test]
fn a_resumed_recording_charges_its_first_turn_only_its_own_usage() {
    // `resume` resumed the session `resume_first` recorded, and its first `result` carries that
    // process's totals over (CLI 2.1.319). Its first turn is charged only what it added.
    let before = last_totals("resume_first");
    let after = last_totals("resume");
    assert!(
        before[0] > 0 && after[0] > before[0],
        "the recording carries the earlier total over: {before:?} {after:?}"
    );
    let first = events("resume")
        .into_iter()
        .find_map(|e| match e {
            ChatEvent::TurnResult { usage, .. } => Some(usage),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        [
            first.cost_micros,
            first.input_tokens,
            first.output_tokens,
            first.cache_read_tokens,
            first.cache_write_tokens,
        ],
        [0, 1, 2, 3, 4].map(|i| after[i] - before[i]),
        "micros and every token class: the turn's own usage"
    );
}

/// A `result` with running totals (dollars, input tokens) and, if given, the turn's own usage in
/// `model_usage_since_last_result`.
fn result_with_own(cost: f64, input: u64, own: Option<(f64, u64)>) -> Value {
    let mut r = json!({"type": "result", "subtype": "success", "is_error": false, "total_cost_usd": cost,
        "modelUsage": {"m": {"inputTokens": input, "outputTokens": 0, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}}});
    if let Some((c, i)) = own {
        r["model_usage_since_last_result"] = json!({"m": {"costUSD": c, "inputTokens": i,
            "outputTokens": 0, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}});
    }
    r
}

fn usage_of(out: Vec<Out>) -> Usage {
    match out.into_iter().find(|o| matches!(o, Out::Event(_))) {
        Some(Out::Event(ChatEvent::TurnResult { usage, .. })) => usage,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_first_result_sets_the_baseline_to_what_the_process_carried_over() {
    // A resumed process: its first `result` holds totals of 0.9 dollars and 300 input tokens, of
    // which its own turn added 0.1 and 100 (`model_usage_since_last_result`).
    let result = result_with_own;
    let mut d = in_turn_decoder();
    let first = usage_of(feed(&mut d, result(0.9, 300, Some((0.1, 100)))));
    assert_eq!((first.cost_micros, first.input_tokens), (100_000, 100));
    // The next turn counts from the whole carried total.
    d.note_send("u2", InputKind::Operator);
    feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
        "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u2"]}),
    );
    let second = usage_of(feed(&mut d, result(1.0, 350, Some((0.1, 50)))));
    assert_eq!((second.cost_micros, second.input_tokens), (100_000, 50));
    // With no `model_usage_since_last_result`, a first result counts from zero, as before.
    let mut d = in_turn_decoder();
    let whole = usage_of(feed(&mut d, result(0.9, 300, None)));
    assert_eq!((whole.cost_micros, whole.input_tokens), (900_000, 300));
}

#[test]
fn a_first_result_outside_a_turn_sets_the_baseline_and_its_spend_lands_in_the_next_turn() {
    // The process's first `result` comes with no turn open: it is logged, and it still sets the
    // baseline, so its own spend (0.1, 100) goes into the next turn's usage, as a later stray
    // result's does.
    let mut d = Decoder::new("2.1.319", "s", false);
    let out = feed(&mut d, result_with_own(0.9, 300, Some((0.1, 100))));
    assert!(
        out.iter()
            .any(|o| matches!(o, Out::Log(l) if l.contains("outside a turn")))
    );
    d.note_send("u1", InputKind::Operator);
    feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
        "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u1"]}),
    );
    let next = usage_of(feed(&mut d, result_with_own(1.0, 350, Some((0.1, 50)))));
    assert_eq!((next.cost_micros, next.input_tokens), (200_000, 150));
}

#[test]
fn the_large_output_recording_has_one_capped_tool_result_of_its_original_size() {
    let capped: Vec<_> = events("big_output")
        .into_iter()
        .filter_map(|e| match e {
            ChatEvent::ToolFinished { output, .. } if output.truncated => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].bytes, 18892);
    assert!(capped[0].text.len() <= CAP_BYTES);
}

#[test]
fn the_mid_turn_recording_is_one_turn_and_the_background_recording_has_a_task_turn() {
    let ev = events("mid_turn");
    assert_eq!(
        names(&ev).iter().filter(|n| **n == "TurnStarted").count(),
        1,
        "steered: one turn"
    );
    assert_eq!(names(&ev).iter().filter(|n| **n == "TurnResult").count(), 1);

    let ev = events("background_task");
    let causes: Vec<TurnCause> = ev
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnStarted { cause } => Some(*cause),
            _ => None,
        })
        .collect();
    assert_eq!(causes, [TurnCause::Operator, TurnCause::Task]);
    let finished = ev
        .iter()
        .position(|e| {
            matches!(
                e,
                ChatEvent::TaskFinished {
                    end: TaskEnd::Completed,
                    ..
                }
            )
        })
        .expect("the task finished");
    let task_turn = ev
        .iter()
        .rposition(|e| matches!(e, ChatEvent::TurnStarted { .. }))
        .unwrap();
    assert!(finished < task_turn, "the Task turn follows the finish");
}

#[test]
fn an_interrupted_turn_is_an_error_result_named_by_its_subtype() {
    let stops: Vec<(bool, String)> = events("interrupt")
        .into_iter()
        .filter_map(|e| match e {
            ChatEvent::TurnResult { is_error, stop, .. } => Some((is_error, stop)),
            _ => None,
        })
        .collect();
    assert!(
        stops.contains(&(true, "error_during_execution".to_string())),
        "the recorded interrupt: {stops:?}"
    );
}

#[test]
fn the_permission_recording_gives_approvals_with_a_reason_and_a_question() {
    let ev = events("permissions");
    let approvals = ev
        .iter()
        .filter(|e| matches!(e, ChatEvent::ApprovalRequest { .. }))
        .count();
    let questions = ev
        .iter()
        .filter(|e| matches!(e, ChatEvent::Question { .. }))
        .count();
    assert_eq!((approvals, questions), (2, 1));
    assert!(
        ev.iter().any(|e| matches!(
            e,
            ChatEvent::ApprovalRequest { reason: Some(r), .. } if !r.is_empty()
        )),
        "the request's description stands in for the missing decision reason"
    );
}

#[test]
fn a_recording_with_no_session_events_gives_none_and_a_refused_resume_gives_a_log() {
    assert!(events("models").is_empty(), "an initialize only");
    let f = load_fixture("resume_missing");
    let (ev, logs) = decode_recording("2.1.319", "", &f.body);
    assert!(ev.is_empty());
    assert!(
        logs.iter().any(|l| l.contains("outside a turn")),
        "{logs:?}"
    );
}

// ------------------------------------------------------------------ what no recording holds

fn feed(d: &mut Decoder, msg: Value) -> Vec<Out> {
    d.feed(&msg)
}

fn in_turn_decoder() -> Decoder {
    let mut d = Decoder::new("2.1.319", "s", false);
    d.note_send("u1", InputKind::Operator);
    let out = feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
            "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u1"]}),
    );
    assert!(matches!(out[0], Out::Event(ChatEvent::TurnStarted { .. })));
    d
}

#[test]
fn a_cancelled_request_closes_an_open_one_and_ignores_an_unknown_one() {
    let mut d = in_turn_decoder();
    let out = feed(
        &mut d,
        json!({"type": "control_request", "request_id": "r1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}}}),
    );
    assert!(
        matches!(&out[0], Out::Event(ChatEvent::ApprovalRequest { request_id, .. }) if request_id == "r1")
    );
    let out = feed(
        &mut d,
        json!({"type": "control_cancel_request", "request_id": "r1"}),
    );
    assert_eq!(
        out,
        [Out::Event(ChatEvent::RequestClosed {
            request_id: "r1".into()
        })]
    );
    assert!(
        feed(
            &mut d,
            json!({"type": "control_cancel_request", "request_id": "r1"})
        )
        .is_empty()
    );
    assert!(
        feed(
            &mut d,
            json!({"type": "control_cancel_request", "request_id": "zz"})
        )
        .is_empty()
    );
}

#[test]
fn inbound_requests_the_adapter_does_not_serve_get_an_error_response() {
    let mut d = in_turn_decoder();
    for subtype in [
        "hook_callback",
        "mcp_message",
        "elicitation",
        "request_user_dialog",
        "frobnicate",
    ] {
        let out = feed(
            &mut d,
            json!({"type": "control_request", "request_id": format!("id-{subtype}"), "request": {"subtype": subtype}}),
        );
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::Respond { request_id, error }
                if request_id == &format!("id-{subtype}") && error.contains(subtype))),
            "{subtype}: {out:?}"
        );
        assert!(
            out.iter().all(|o| !matches!(o, Out::Event(_))),
            "{subtype}: no event"
        );
    }
}

#[test]
fn a_cumulative_total_that_goes_down_counts_zero_logs_and_becomes_the_baseline() {
    let result = |cost: f64, input: u64| {
        json!({"type": "result", "subtype": "success", "is_error": false, "total_cost_usd": cost,
            "modelUsage": {"m": {"inputTokens": input, "outputTokens": 0, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}}})
    };
    let mut d = in_turn_decoder();
    feed(&mut d, result(0.5, 100));
    d.note_send("u2", InputKind::Operator);
    feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
        "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u2"]}),
    );
    let out = feed(&mut d, result(0.2, 40));
    assert!(
        out.iter()
            .any(|o| matches!(o, Out::Log(l) if l.contains("went down")))
    );
    let Some(Out::Event(ChatEvent::TurnResult { usage, .. })) =
        out.iter().find(|o| matches!(o, Out::Event(_))).cloned()
    else {
        panic!("{out:?}")
    };
    assert_eq!((usage.cost_micros, usage.input_tokens), (0, 0));
    // The next turn counts from the lower baseline.
    d.note_send("u3", InputKind::Operator);
    feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
        "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u3"]}),
    );
    let out = feed(&mut d, result(0.3, 90));
    let Some(Out::Event(ChatEvent::TurnResult { usage, .. })) =
        out.iter().find(|o| matches!(o, Out::Event(_))).cloned()
    else {
        panic!("{out:?}")
    };
    assert_eq!((usage.cost_micros, usage.input_tokens), (100_000, 50));
}

#[test]
fn an_unknown_message_kind_gives_no_event_and_no_failure() {
    let mut d = in_turn_decoder();
    for msg in [
        json!({"type": "rate_limit_event"}),
        json!({"type": "something_new", "x": 1}),
        json!({"type": "system", "subtype": "never_heard_of_it"}),
        json!({}),
    ] {
        assert!(feed(&mut d, msg).is_empty());
    }
}

/// `r1.s4.w4`, case 19: every `Bash` call of the recording that ran one of the four commands got an
/// error result, and the recording starts with the two self-started turns that d21 replays.
///
/// What the recording does not show (ADR-0014's amendment, case 19): the results of
/// `git branch -D pf-probe` and `git push -f origin main` carry the text of the recording host's own
/// `PreToolUse` hook, not the text of a deny rule, so they do not show the rule; the model made no
/// `Bash` call for `git push origin main --force` or for the compound command. The one result that
/// carries the rule's own text is `gh -R … pr merge 1`. A second run needs a new grant.
#[test]
fn the_deny_rules_recording_gives_every_bash_call_of_the_four_commands_an_error_and_starts_with_two_task_turns()
 {
    let f = load_fixture("deny_rules");
    assert_eq!(f.header["pf_fixture"]["case"], "deny_rules");
    assert_eq!(f.header["pf_fixture"]["cli_version"], "2.1.319");
    let (events, _) = decode_recording("2.1.319", "", &f.body);
    check_order(&events).unwrap();
    let causes: Vec<TurnCause> = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnStarted { cause } => Some(*cause),
            _ => None,
        })
        .collect();
    assert!(causes.len() >= 3, "{causes:?}");
    assert_eq!(
        causes[..3],
        [TurnCause::Operator, TurnCause::Task, TurnCause::Task],
        "the first three turns"
    );
    let host: Vec<&Value> = f
        .body
        .iter()
        .filter(|(to, _)| to == "host")
        .map(|(_, m)| m)
        .collect();
    let blocks: Vec<&Value> = host
        .iter()
        .flat_map(|m| m["message"]["content"].as_array().into_iter().flatten())
        .collect();
    let mut ran = 0;
    for command in [
        "git branch -D pf-probe",
        "git push -f origin main",
        "git push origin main --force",
        "gh -R pulseai-labs/pf-probe pr merge 1",
    ] {
        for b in &blocks {
            if !(b["type"] == "tool_use"
                && b["name"] == "Bash"
                && b["input"]["command"].as_str() == Some(command))
            {
                continue;
            }
            ran += 1;
            let result = blocks
                .iter()
                .find(|r| r["type"] == "tool_result" && r["tool_use_id"] == b["id"])
                .unwrap_or_else(|| panic!("no tool_result for `{command}`"));
            assert_eq!(
                result["is_error"], true,
                "`{command}` was not refused: {result}"
            );
        }
    }
    assert!(ran >= 3, "the recording holds {ran} of the four Bash calls");
}

/// The start recording (`r1.s4.w2`, case 20; hand-authored until the operator's run replaces it):
/// one turn that runs `pulse-factory propose s1 …` through `Bash` and never `pulse-factory arm`. The
/// rule that judges a call is the one case 20 applies to the stub's argv records.
#[test]
fn the_start_proposal_recording_holds_a_bash_proposal_of_s1_and_no_arm() {
    let f = load_fixture("start_proposal");
    assert_eq!(f.header["pf_fixture"]["case"], "start_proposal");
    assert_eq!(f.header["pf_fixture"]["cli_version"], "2.1.319");
    let (events, _) = decode_recording("2.1.319", "", &f.body);
    check_order(&events).unwrap();
    let blocks: Vec<&Value> = f
        .body
        .iter()
        .filter(|(to, _)| to == "host")
        .flat_map(|(_, m)| m["message"]["content"].as_array().into_iter().flatten())
        .collect();
    let (mut proposals, mut arms) = (0, 0);
    for b in &blocks {
        if b["type"] == "tool_use" && b["name"] == "Bash" {
            let (p, a) = propose_rule::judge_bash(b["input"]["command"].as_str().unwrap(), "s1");
            proposals += p;
            arms += a;
        }
    }
    assert!(proposals >= 1, "no Bash call proposes s1: {blocks:?}");
    assert_eq!(arms, 0, "a Bash call arms");
    // The words of the recorded call, so a change of the recording shows here.
    let command = blocks
        .iter()
        .find(|b| b["type"] == "tool_use" && b["name"] == "Bash")
        .and_then(|b| b["input"]["command"].as_str())
        .unwrap();
    assert!(
        command.contains("pulse-factory propose"),
        "the recorded command: {command}"
    );
    // One operator turn and one result.
    let n = names(&events);
    assert_eq!(n.iter().filter(|x| **x == "TurnResult").count(), 1, "{n:?}");
}

#[test]
fn the_matching_rule_reads_flags_values_quotes_prefixes_and_operators() {
    use propose_rule::*;
    let words = |s: &str| simple_commands(s).remove(0);
    // A flag's value is not a positional, in either spelling and in any order.
    assert!(is_proposal_of(
        &words("pulse-factory propose s1 --workflow w --ledger /l"),
        "s1"
    ));
    assert!(is_proposal_of(
        &words("pulse-factory propose --workflow w --ledger=/l s1 --json"),
        "s1"
    ));
    assert!(!is_proposal_of(
        &words("pulse-factory propose s2 --workflow s1"),
        "s1"
    ));
    assert!(!is_proposal_of(
        &words("pulse-factory propose s2 --param s1"),
        "s1"
    ));
    assert!(is_proposal_of(
        &words("pulse-factory propose s2 --param=x=1 s1"),
        "s1"
    ));
    // A boolean flag takes no value: `--json s1` leaves `s1` a positional.
    assert!(is_proposal_of(
        &words("pulse-factory propose --json s1"),
        "s1"
    ));
    // The program's basename, quotes, and `env` / `VAR=value` prefixes.
    assert!(is_proposal_of(
        &words("env PF=1 FOO='a b' /usr/local/bin/pulse-factory propose \"s1\""),
        "s1"
    ));
    assert!(!is_proposal_of(
        &words("echo pulse-factory propose s1"),
        "s1"
    ));
    assert!(!is_proposal_of(&words("pulse-factory-x propose s1"), "s1"));
    // `arm` is judged by the first argument only.
    assert!(is_arm(&words("pulse-factory arm s1 --yes")));
    assert!(!is_arm(&words("pulse-factory propose s1 --param arm=1")));
    // Operators split a command; quoted operators do not.
    let all = simple_commands("cd x && pulse-factory propose s1 | tee o; echo 'a; b' || true\nls");
    assert_eq!(all.len(), 6, "{all:?}");
    assert_eq!(all[3], ["echo", "a; b"]);
    assert_eq!(
        judge_bash("cd x && pulse-factory propose s1 --json | jq .", "s1"),
        (1, 0)
    );
    assert_eq!(
        judge_bash("pulse-factory propose s1; pulse-factory arm s1", "s1"),
        (1, 1)
    );
    // A stub's records: NUL-terminated words, one call a line.
    let calls = stub_calls("pulse-factory\0propose\0s1\0--json\0\npulse-factory\0arm\0s1\0\n");
    assert_eq!(calls.len(), 2);
    assert!(is_proposal_of(&calls[0], "s1"));
    assert!(is_arm(&calls[1]));
}
