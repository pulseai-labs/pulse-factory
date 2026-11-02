//! AC-16: reason text from outside never fails the one-line check and never leaks a secret.
//! `one_line_reason` flattens, cleans, cuts and redacts. `redact` removes each secret form. The
//! engine applies `one_line_reason` to every reason it receives.

mod common;

use common::*;
use pf_engine::launch::LaunchStep;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::{ForgeOutcome, PrRef};
use pf_engine::status::{REASON_NONE_GIVEN, REDACTED, check_reason, one_line_reason, redact};
use pf_engine::unit::Signal;
use pf_engine::{EndStatus, Status};

const GH: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";

// ------------------------------------------------------------------ one_line_reason

#[test]
fn a_multi_line_text_becomes_one_line() {
    assert_eq!(
        one_line_reason("first\nsecond\nthird"),
        "first / second / third"
    );
    assert_eq!(one_line_reason("first\r\nsecond"), "first / second");
    assert!(check_reason(&one_line_reason("a\nb\r\nc\rd")).is_ok());
}

#[test]
fn a_lone_carriage_return_is_one_break() {
    assert_eq!(one_line_reason("first\rsecond"), "first / second");
    // A pair is one break, not two.
    assert_eq!(one_line_reason("first\r\nsecond"), "first / second");
}

#[test]
fn control_characters_are_removed() {
    assert_eq!(one_line_reason("red\u{1b}[31m text\u{7}"), "red[31m text");
    assert_eq!(one_line_reason("a\u{0}b\u{8}c"), "abc");
    assert_eq!(one_line_reason("tab\there"), "tabhere");
}

#[test]
fn an_empty_or_blank_text_gets_the_fixed_text() {
    assert_eq!(REASON_NONE_GIVEN, "no reason text given");
    assert_eq!(one_line_reason(""), REASON_NONE_GIVEN);
    assert_eq!(one_line_reason("     "), REASON_NONE_GIVEN);
    assert_eq!(one_line_reason(" \n \r\n "), REASON_NONE_GIVEN);
    assert_eq!(one_line_reason("\u{1b}\u{7}"), REASON_NONE_GIVEN);
}

#[test]
fn a_long_text_is_cut_at_500_characters() {
    let long = "a".repeat(2000);
    assert_eq!(one_line_reason(&long).chars().count(), 500);
    let exact = "b".repeat(500);
    assert_eq!(one_line_reason(&exact), exact);
}

#[test]
fn the_cut_is_on_a_character_boundary_inside_multi_byte_text() {
    // Each of these is 2 to 4 bytes long, so a byte cut at 500 would split one.
    for unit in ["é", "日", "🦀"] {
        for extra in 0..4 {
            let text = format!("{}{}", "x".repeat(extra), unit.repeat(600));
            let out = one_line_reason(&text);
            assert_eq!(out.chars().count(), 500, "{unit} +{extra}");
            assert!(check_reason(&out).is_ok());
        }
    }
}

#[test]
fn the_cut_comes_after_the_secret_is_removed() {
    let text = format!("{} {GH}", "a".repeat(495));
    let out = one_line_reason(&text);
    assert!(!out.contains("ghp_"), "{out}");
    assert!(out.chars().count() <= 500);
}

#[test]
fn the_text_is_redacted_before_it_is_flattened() {
    let out = one_line_reason("line one\nAuthorization: Basic dXNlcjpwYXNz\nline three");
    assert_eq!(
        out,
        format!("line one / Authorization: {REDACTED} / line three")
    );
}

// ------------------------------------------------------------------ redact

fn assert_gone(input: &str, secret: &str) {
    let out = redact(input);
    assert!(!out.contains(secret), "{secret:?} survived in {out:?}");
    assert!(out.contains(REDACTED), "no marker in {out:?}");
}

#[test]
fn github_tokens_of_every_kind_are_removed() {
    for kind in ["ghp", "gho", "ghu", "ghs", "ghr"] {
        let token = format!("{kind}_0123456789abcdefghijklmnopqrstuvwxyz");
        assert_gone(&format!("remote said {token} is bad"), &token);
    }
    let pat = "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz0123456789";
    assert_gone(&format!("token {pat}."), pat);
}

#[test]
fn an_sk_key_is_removed_as_a_whole_word() {
    let key = "sk-abcdefghijklmnop0123";
    assert_gone(&format!("key {key} was rejected"), key);
    assert_gone(&format!("({key})"), key);
    let proj = "sk-proj-AbCdEfGhIjKlMnOp_0123456789";
    assert_gone(&format!("key={proj}"), proj);
}

#[test]
fn a_word_that_only_ends_in_sk_is_left_alone() {
    for text in [
        "task-1234567890123456789",
        "see task-1234567890123456789 for details",
        "disk-abcdefghijklmnopqrstuvwxyz",
        "sk-short",
        "sk-123456789012345",
    ] {
        assert_eq!(redact(text), text);
    }
    // 16 key characters is the lowest count that matches.
    assert_gone("sk-1234567890123456", "sk-1234567890123456");
}

#[test]
fn a_bearer_credential_is_removed() {
    assert_gone(
        "sent Bearer abc.DEF-123_xyz~+/= and failed",
        "abc.DEF-123_xyz",
    );
    assert_gone(
        "header bearer lowercasetokenvalue1234",
        "lowercasetokenvalue1234",
    );
    // Not a whole word: left alone.
    assert_eq!(redact("Bearers carry loads"), "Bearers carry loads");
}

#[test]
fn an_authorization_value_is_removed_to_the_end_of_the_line() {
    let out = redact("before\nAuthorization: Basic dXNlcjpwYXNz more words\nafter");
    assert_eq!(out, format!("before\nAuthorization: {REDACTED}\nafter"));
    let out = redact("authorization: token abc def");
    assert_eq!(out, format!("authorization: {REDACTED}"));
}

#[test]
fn token_key_and_secret_assignments_are_removed() {
    assert_gone("GITHUB_TOKEN=abc123xyz done", "abc123xyz");
    assert_gone("export API_KEY=hunter2hunter2 && run", "hunter2hunter2");
    assert_gone("DB_SECRET=s3cr3t-value", "s3cr3t-value");
    assert_gone("MY_TOKEN='quoted-secret' next", "quoted-secret");
    // The name stays, so a reader sees which variable it was.
    assert!(redact("GITHUB_TOKEN=abc123xyz").starts_with("GITHUB_TOKEN="));
    // An assignment of another name is left alone.
    assert_eq!(redact("PATH=/usr/bin"), "PATH=/usr/bin");
    assert_eq!(redact("TOKEN_COUNT=3"), "TOKEN_COUNT=3");
}

#[test]
fn an_api_key_flag_is_removed_in_both_forms() {
    assert_gone("run --api-key=abcdef123456 now", "abcdef123456");
    assert_gone("run --api-key abcdef123456 now", "abcdef123456");
    assert!(redact("run --api-key=abcdef123456 now").contains("--api-key="));
    assert!(redact("run --api-key abcdef123456 now").contains("--api-key "));
    assert!(redact("run --api-key abcdef123456 now").ends_with("now"));
}

#[test]
fn the_secret_in_a_url_is_removed() {
    let out = redact("clone https://alice:hunter2@example.com/org/repo.git failed");
    assert!(!out.contains("hunter2"), "{out}");
    assert!(out.contains("https://alice:"), "{out}");
    assert!(out.contains("@example.com/org/repo.git"), "{out}");
    // A URL with no password, or an @ in the path, is left alone.
    for text in [
        "https://example.com/a@b",
        "https://alice@example.com/repo",
        "see https://example.com:8080/path",
    ] {
        assert_eq!(redact(text), text);
    }
}

#[test]
fn plain_text_passes_through_redact() {
    for text in [
        "",
        "checks red",
        "merge conflict on src/lib.rs",
        "unit u1 parked: seat ended with no done signal",
        "héllo wörld 日本語",
        "ghp_short",
    ] {
        assert_eq!(redact(text), text);
    }
}

// ------------------------------------------------------------------ the engine applies it

fn multi_line() -> String {
    format!("first line\nsecond line has {GH}\r\nthird line\rlast")
}

/// The ledger holds the reason on one line and without the token.
fn assert_ledger_clean(env: &Env) {
    let ledger = env.ledger_text();
    assert!(!ledger.contains(GH), "{ledger}");
    assert!(!ledger.contains("ghp_"), "{ledger}");
    assert!(
        ledger.contains("first line / "),
        "the line breaks became ' / ': {ledger}"
    );
    for line in ledger.lines().filter(|l| !l.is_empty()) {
        assert!(line.starts_with("- #"), "a ledger line was split: {line:?}");
    }
}

fn run() -> Env {
    let e = env(workflow(&["build"], true));
    e.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1), unit("u2", "g1", 2)],
        })
        .unwrap();
    e
}

fn launched(e: &Env, unit: &str) -> String {
    match e.engine.start_stage(unit).unwrap() {
        LaunchStep::Launched { session_id, .. } => session_id,
        other => panic!("not launched: {other:?}"),
    }
}

#[test]
fn park_accepts_a_multi_line_text_with_a_token() {
    let e = run();
    launched(&e, "u1");
    e.engine.park("u1", &multi_line()).unwrap();
    let (status, reason) = e.status_of("u1");
    assert_eq!(status, Status::Parked);
    let reason = reason.unwrap();
    assert!(
        !reason.contains(['\n', '\r']) && !reason.contains(GH),
        "{reason}"
    );
    assert_ledger_clean(&e);
}

#[test]
fn end_accepts_a_multi_line_text_with_a_token() {
    let e = run();
    let session = launched(&e, "u1");
    e.engine
        .end(&session, EndStatus::Failed, &multi_line())
        .unwrap();
    let (status, reason) = e.status_of(&session);
    assert_eq!(status, Status::Failed);
    let reason = reason.unwrap();
    assert!(
        !reason.contains(['\n', '\r']) && !reason.contains(GH),
        "{reason}"
    );
    assert_ledger_clean(&e);
}

#[test]
fn end_run_accepts_a_multi_line_text_with_a_token() {
    let e = run();
    e.engine.end_run(&multi_line()).unwrap();
    assert_ledger_clean(&e);
}

#[test]
fn halt_accepts_a_multi_line_text_with_a_token() {
    let e = run();
    e.engine.halt(&multi_line()).unwrap();
    assert_ledger_clean(&e);
}

#[test]
fn halt_for_accepts_a_multi_line_text_with_a_token() {
    let e = run();
    e.engine.halt_for("queue", &multi_line()).unwrap();
    assert_ledger_clean(&e);
}

#[test]
fn an_empty_reason_is_accepted_and_gets_the_fixed_text() {
    let e = run();
    launched(&e, "u1");
    e.engine.park("u1", "  \n ").unwrap();
    assert_eq!(e.status_of("u1").1.as_deref(), Some(REASON_NONE_GIVEN));
    e.engine.halt("").unwrap();
    assert!(e.ledger_text().contains(REASON_NONE_GIVEN));
}

#[test]
fn a_merge_failure_with_a_multi_line_text_and_a_token_parks_the_unit() {
    let e = run();
    let mut script = PrScript::green("work/u1", "sha-u1");
    script.merge = ForgeOutcome::Refused(multi_line());
    e.forge.script(1, script);
    launched(&e, "u1");
    e.engine
        .signal(
            "u1",
            "build",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(PrRef {
                repo: "synthetic/repo".to_string(),
                number: 1,
            }),
        )
        .unwrap();
    let step = e.engine.merge_next().unwrap();
    let MergeStep::Parked { unit, reason } = step else {
        panic!("expected a park, got {step:?}");
    };
    assert_eq!(unit, "u1");
    assert!(
        !reason.contains(['\n', '\r']) && !reason.contains(GH),
        "{reason}"
    );
    let (status, stored) = e.status_of("u1");
    assert_eq!(status, Status::Parked);
    let stored = stored.unwrap();
    assert!(
        !stored.contains(['\n', '\r']) && !stored.contains(GH),
        "{stored}"
    );
    assert_ledger_clean(&e);
}

#[test]
fn halt_for_from_two_sources_and_halt_after_halt_for_are_admitted() {
    let e = run();
    let a = e.engine.halt_for("queue", "bad record a.json").unwrap();
    let b = e
        .engine
        .halt_for("seed", "seed lane added no units")
        .unwrap();
    let c = e.engine.halt("operator stop").unwrap();
    for (name, admission) in [("queue", a), ("seed", b), ("halt", c)] {
        assert!(!admission.replayed, "{name}");
    }
    // The same source with the same text is a replay, not a conflict.
    let again = e.engine.halt_for("queue", "bad record a.json").unwrap();
    assert!(again.replayed);
}

#[test]
fn a_second_pass_over_redacted_text_changes_nothing() {
    let text = format!(
        "a {GH} b\nAuthorization: Basic dXNlcjpwYXNz\nGITHUB_TOKEN=abc --api-key xyz https://u:pw@h/p Bearer qrs"
    );
    let once = redact(&text);
    assert_eq!(redact(&once), once);
    let reason = one_line_reason(&text);
    assert_eq!(redact(&reason), reason);
}
