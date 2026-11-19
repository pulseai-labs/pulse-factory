//! AC-10 of `r2.s2.w5` (audit 11): a host that dropped output while the controller was down. With
//! `session.replay_bytes` at its smallest (65536), the replay held by a gate before the recording's
//! big tool output, the controller killed with `SIGKILL`, the gate released, and the controller
//! started again after the replay wrote past the buffer: one `output_lost` notice names the dropped
//! stretch, no item is stored twice, the turn's `turn_result` is stored once with the recording's
//! cost, and the chat stays live.
//!
//! Build `pf-host` and `pf-claude-replay` first (`cargo build -p pf-host -p pf-adapter-claude
//! --bins`). Nothing starts a model, spends a token or reaches a network.

mod common;

use common::chat_restart::*;

#[test]
fn a_host_that_dropped_output_gives_one_output_lost_notice_and_the_turn_is_still_counted_once() {
    // One recorded turn writes less than the smallest buffer, so the test writes the big tool
    // output three times (see `tripled_big_output`).
    let tmp = tempfile::tempdir().unwrap();
    let big = tripled_big_output(tmp.path());
    let fixture_path = big.as_str();
    let mut rig = ChatRig::with(
        fixture_path,
        None,
        &[("user", "g1")],
        &[],
        &[("session.replay_bytes", "65536")],
    );
    rig.send("make some output");
    // The replay holds before the big tool output.
    rig.wait_progress(0, host_lines_before(fixture_path, "user"));
    rig.wait_items("the tool call's start", |i| !i.is_empty());
    let pid = rig.pid(0);
    rig.r.kill9();
    // The gate is released while the controller is down: the replay writes the big line and the
    // rest of the turn, well past the 64 KiB buffer.
    rig.release("g1");
    rig.wait_progress(0, host_lines_before(fixture_path, "result") + 1);
    rig.r.start_controller();

    assert!(alive(pid), "the chat's process is the same");
    assert_eq!(rig.launches(), 1);
    assert!(rig.row().ended_at.is_none(), "the chat stays live");
    rig.wait_items("the turn's result", |i| {
        i.iter().any(|x| x.kind == "turn_result")
    });
    std::thread::sleep(std::time::Duration::from_millis(400));

    let lost = rig.notices("output_lost");
    assert_eq!(lost.len(), 1, "one notice: {:?}", rig.kinds());
    let text = lost[0].payload["text"].as_str().unwrap();
    assert!(text.starts_with("output lost: "), "{text}");
    let cursors: Vec<u64> = text
        .split("(cursor ")
        .nth(1)
        .and_then(|t| t.split(')').next())
        .map(|t| {
            t.split(" to ")
                .filter_map(|n| n.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(cursors.len(), 2, "the notice names the stretch: {text}");
    assert!(cursors[0] < cursors[1], "{text}");
    assert!(
        cursors[1] - cursors[0] > 0,
        "the dropped stretch is not empty: {text}"
    );

    // No item twice: the recorded turn stores its tool call and its reply once.
    let kinds = rig.kinds();
    assert_eq!(
        kinds.iter().filter(|k| *k == "turn_result").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "operator_message").count(),
        1,
        "{kinds:?}"
    );
    let calls: Vec<String> = rig
        .items()
        .into_iter()
        .filter(|i| i.kind == "tool_call")
        .map(|i| {
            i.payload["call_id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let mut unique = calls.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        calls.len(),
        unique.len(),
        "a tool call is stored twice: {calls:?}"
    );
    let texts: Vec<String> = rig
        .items()
        .into_iter()
        .filter(|i| i.kind == "assistant_text")
        .map(|i| i.payload["text"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut once = texts.clone();
    once.sort();
    once.dedup();
    assert_eq!(
        texts.len(),
        once.len(),
        "a reply is stored twice: {texts:?}"
    );

    // The cost is the recording's first turn, once.
    let want = result_micros("big_output")[0];
    assert_eq!(rig.row().cost_micros as u64, want);
    let result = rig
        .items()
        .into_iter()
        .find(|i| i.kind == "turn_result")
        .unwrap();
    assert_eq!(result.payload["usage"]["cost_micros"].as_u64(), Some(want));
    assert_eq!(rig.chat_lines("turn").len(), 1);
}
