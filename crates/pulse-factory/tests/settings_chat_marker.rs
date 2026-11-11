//! AC-9 of `r1.s4.w2`: the caller key of a refused `settings` write reads `PULSE_FACTORY_CHAT` (the
//! barrier's ruling, W13). A caller that holds the marker but runs under no live chat process (a child
//! the chat detached by a double fork) is audited as `chat:<S-n>` for a value of the form `S-<digits>`,
//! as `chat:invalid` for any other value, and as `seat-token` when it also holds `PF_SEAT_TOKEN`. The
//! controller never writes a raw invalid value to a ledger, a store row or a refusal text.
//!
//! The controller runs in the test process on a `FakeChat`. Each caller is a real shell whose PID the
//! test registers as the chat's, and the CLI runs below it in a child that the shell detached. Nothing
//! here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;

use std::time::Duration;

use common::chat::*;
use common::start::*;
use common::*;
use pf_engine::port::store::{CONTROLLER_SCOPE, Store};

const CAP: &str = "chat.self_turn_cap";
const DENY: &str = "chat.deny_commands";
const PROPOSE: &str = "chat.propose_per_min";

/// `settings set <key> <value>` from a detached child that holds `marker` (and `extra`).
fn detached_set(s: &Start, marker: &str, key: &str, value: &str, extra: &[(&str, String)]) -> Ran {
    let args = ["settings", "set", key, value, "--command-id", "marker-cmd"];
    s.below(0, Style::DoubleFork, true, Some(marker), BIN, &args, extra)
}

fn refused_line(s: &Start, caller: &str, bucket: &str) -> Vec<String> {
    s.ledger_with("setting_set refused")
        .into_iter()
        .filter(|l| l.contains(&format!("caller={caller} bucket={bucket} ")))
        .collect()
}

/// The whole of every place a raw value could land.
fn everything_written(s: &Start) -> String {
    let mut all = String::new();
    for name in ["controller.ledger", "chat.ledger", "api.ledger"] {
        all.push_str(&std::fs::read_to_string(s.live.sb.state.join(name)).unwrap_or_default());
    }
    for a in s
        .live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
    {
        all.push_str(&a.content);
        all.push('\n');
        all.push_str(&a.reason.unwrap_or_default());
        all.push('\n');
    }
    all
}

// One test function: a test that changes the environment of children keeps its cases together.
#[test]
fn the_caller_key_reads_the_chat_marker_and_never_writes_a_raw_invalid_value() {
    let s = Start::with(StartOptions {
        tweak: Box::new(|cfg| cfg.refusal_window = Duration::from_secs(4)),
        ..StartOptions::default()
    });

    // A detached child with `PULSE_FACTORY_CHAT=S-1`: `chat:S-1`.
    let ran = detached_set(&s, "S-1", CAP, "50", &[]);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(
        !ran.chain.contains(&ran.shell),
        "the child's chain {:?} holds the chat shell {}",
        ran.chain,
        ran.shell
    );
    let lines = refused_line(&s, "chat:S-1", CAP);
    assert_eq!(lines.len(), 1, "{:#?}", controller_ledger(&s.live));
    assert!(lines[0].contains(" refused=1 "), "{}", lines[0]);

    // A marker with trailing text, and a marker of 40 characters: `chat:invalid`.
    let spaced = "S-1 bucket=x";
    let long = format!("S-{}", "1".repeat(38));
    assert_eq!(long.len(), 40);
    let ran = detached_set(&s, spaced, DENY, "[]", &[]);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(
        ran.err.contains("chat:invalid"),
        "the refusal names the form: {}",
        ran.err
    );
    assert!(
        !ran.err.contains(spaced),
        "the refusal holds the raw value: {}",
        ran.err
    );
    assert_eq!(refused_line(&s, "chat:invalid", DENY).len(), 1);
    let ran = detached_set(&s, &long, PROPOSE, "3", &[]);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(ran.err.contains("chat:invalid"), "{}", ran.err);
    assert!(!ran.err.contains(&long), "{}", ran.err);
    assert_eq!(refused_line(&s, "chat:invalid", PROPOSE).len(), 1);

    // `PF_SEAT_TOKEN` with the marker: `seat-token`.
    let ran = detached_set(
        &s,
        "S-1",
        CAP,
        "51",
        &[("PF_SEAT_TOKEN", "seat-token-value".to_string())],
    );
    assert_eq!(ran.rc, 1, "{}", ran.err);
    // The first refusal of this caller and bucket writes its line at once.
    assert_eq!(refused_line(&s, "seat-token", CAP).len(), 1);

    // No raw invalid value, and no seat token, was written anywhere.
    let all = everything_written(&s);
    assert!(!all.contains(spaced), "the spaced marker was written");
    assert!(!all.contains(&long), "the long marker was written");
    assert!(
        !all.contains("seat-token-value"),
        "the seat token was written"
    );
    // No refused call changed a setting.
    assert!(
        s.live.controller.store().setting_rows().unwrap().is_empty(),
        "a refused write changed a row"
    );
}
