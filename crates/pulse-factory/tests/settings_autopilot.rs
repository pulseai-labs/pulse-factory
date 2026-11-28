//! AC-7 of `r2.s3.w4`: the two wake keys, `autopilot.wake` and `autopilot.wakes_per_hour`
//! (ADR-0016's amendment of 2026-11-24, `docs/contracts/cli.md`, `settings`). The first is a list of
//! one word, `["off"]` or `["below"]` (built-in `["below"]`); the second is a whole number from 0 to
//! 60 (built-in 6; 0 is off). Both merge by `override`, are written at all four scopes and read at
//! the project and the agent `orchestrator`, read `next_turn` on the wire and `current` at a resume,
//! and a chat or a seat is refused on a write. `settings get` lists fifteen keys, the two last.
//!
//! The controller runs in the test process on a `FakeChat`. Nothing here starts `claude`, spends a
//! token or reaches beyond `127.0.0.1`.

mod common;

use std::sync::Arc;

use common::chat::*;
use common::ctl::Live;
use common::{BIN, code, stderr};
use pulse_factory::controller::settings::{AtResume, at_resume_of};
use serde_json::{Value, json};

const WAKE: &str = "autopilot.wake";
const HOUR: &str = "autopilot.wakes_per_hour";

fn live() -> (Live, Arc<Rec>, String) {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let project = add_project(&live, &repo(&live, "alpha"));
    (live, rec, project)
}

fn record(live: &Live, key: &str, scope: &[&str]) -> Value {
    let mut args = vec!["settings", "get", key];
    args.extend_from_slice(scope);
    let v = cli_json(live, &args);
    assert_eq!(v["settings"].as_array().unwrap().len(), 1, "{v}");
    v["settings"][0].clone()
}

fn set(live: &Live, key: &str, value: &str, scope: &[&str]) -> Value {
    let mut args = vec!["settings", "set", key, value];
    args.extend_from_slice(scope);
    cli_json(live, &args)
}

fn refused(live: &Live, key: &str, value: &str, scope: &[&str]) -> String {
    let mut args = vec!["settings", "set", key, value];
    args.extend_from_slice(scope);
    cli_refused(live, &args)
}

fn from_kind(r: &Value) -> &str {
    r["from"]["kind"].as_str().unwrap()
}

#[test]
fn the_built_ins_are_below_and_six_and_both_keys_read_next_turn_and_current_at_a_resume() {
    let (live, _, project) = live();
    let scope = ["--project", project.as_str(), "--agent", "orchestrator"];
    let wake = record(&live, WAKE, &scope);
    assert_eq!(wake["value"], json!(["below"]), "{wake}");
    assert_eq!(wake["built_in"], json!(["below"]), "{wake}");
    assert_eq!(from_kind(&wake), "built_in");
    let hour = record(&live, HOUR, &scope);
    assert_eq!(hour["value"], 6, "{hour}");
    assert_eq!(hour["built_in"], 6, "{hour}");
    assert_eq!(from_kind(&hour), "built_in");
    for r in [&wake, &hour] {
        assert_eq!(r["merge"], "override", "{r}");
        // `next_turn` stands for the next wake (a third wire value is not added).
        assert_eq!(r["applies"], "next_turn", "{r}");
    }
    assert_eq!(at_resume_of(WAKE), Some(AtResume::Current));
    assert_eq!(at_resume_of(HOUR), Some(AtResume::Current));
}

#[test]
fn settings_get_lists_fifteen_keys_and_the_two_wake_keys_are_the_last() {
    let (live, _, _) = live();
    let all = cli_json(&live, &["settings", "get"]);
    let keys: Vec<&str> = all["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 15, "{keys:?}");
    assert_eq!(&keys[13..], [WAKE, HOUR]);
    assert_eq!(keys[12], "authority.clear_acts");
}

#[test]
fn a_wake_value_is_one_word_off_or_below_and_anything_else_is_refused_with_the_rule() {
    let (live, _, project) = live();
    for bad in [
        "[]",
        r#"["all"]"#,
        r#"["off","below"]"#,
        r#""below""#,
        "1",
        r#"[1]"#,
        r#"["Below"]"#,
    ] {
        let err = refused(&live, WAKE, bad, &["--project", &project]);
        assert!(
            err.contains(&format!(
                r#"autopilot.wake is a list of one word, ["off"] or ["below"], not {bad}"#
            )),
            "{bad}: {err}"
        );
    }
    for good in [r#"["off"]"#, r#"["below"]"#] {
        let v = set(&live, WAKE, good, &["--project", &project]);
        assert_eq!(v["value"], serde_json::from_str::<Value>(good).unwrap());
    }
}

#[test]
fn the_hourly_bound_is_zero_to_sixty_and_sixty_one_is_refused_with_the_rule() {
    let (live, _, project) = live();
    for edge in [0, 1, 60] {
        let v = set(&live, HOUR, &edge.to_string(), &["--project", &project]);
        assert_eq!(v["value"], edge, "{v}");
    }
    for bad in ["61", "-1", "6.5", r#""6""#, "[6]"] {
        let err = refused(&live, HOUR, bad, &["--project", &project]);
        assert!(
            err.contains("autopilot.wakes_per_hour is a whole number from 0 to 60"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn each_key_is_written_at_the_four_scopes_and_the_most_specific_row_wins_whole() {
    let (live, _, project) = live();
    let both = ["--project", project.as_str(), "--agent", "orchestrator"];
    // The server row reaches every context.
    set(&live, WAKE, r#"["off"]"#, &[]);
    set(&live, HOUR, "10", &[]);
    let r = record(&live, WAKE, &both);
    assert_eq!(
        (r["value"].clone(), from_kind(&r)),
        (json!(["off"]), "server")
    );
    let r = record(&live, HOUR, &both);
    assert_eq!((r["value"].clone(), from_kind(&r)), (json!(10), "server"));
    // The agent row is above the server's, the project row above the agent's, and the pair above
    // both.
    set(&live, HOUR, "20", &["--agent", "orchestrator"]);
    assert_eq!(record(&live, HOUR, &both)["value"], 20);
    assert_eq!(from_kind(&record(&live, HOUR, &both)), "agent");
    set(&live, HOUR, "30", &["--project", &project]);
    assert_eq!(record(&live, HOUR, &both)["value"], 30);
    assert_eq!(from_kind(&record(&live, HOUR, &both)), "project");
    set(&live, HOUR, "40", &both);
    let r = record(&live, HOUR, &both);
    assert_eq!(
        (r["value"].clone(), from_kind(&r)),
        (json!(40), "project_agent")
    );
    // `override`: the winner hides every other stored row, and `layers` lists all four.
    assert_eq!(r["layers"].as_array().unwrap().len(), 4, "{r}");
    assert_eq!(r["masked"].as_array().unwrap().len(), 3, "{r}");
    // The wake key at the project and the pair, and a context with no project reads the server row.
    set(&live, WAKE, r#"["below"]"#, &["--project", &project]);
    assert_eq!(record(&live, WAKE, &both)["value"], json!(["below"]));
    set(&live, WAKE, r#"["off"]"#, &both);
    assert_eq!(record(&live, WAKE, &both)["value"], json!(["off"]));
    assert_eq!(
        record(&live, WAKE, &["--agent", "orchestrator"])["value"],
        json!(["off"])
    );
    // An unset returns the chain to the next row.
    cli_json(
        &live,
        &[
            "settings",
            "unset",
            WAKE,
            "--project",
            &project,
            "--agent",
            "orchestrator",
        ],
    );
    assert_eq!(record(&live, WAKE, &both)["value"], json!(["below"]));
}

/// A caller that is a real shell the controller takes for the live chat, or a caller that holds a
/// seat token, as `settings_caller_refusal.rs` runs them.
#[test]
fn a_chat_and_a_seat_are_refused_on_both_writes_by_the_caller_check() {
    use std::process::Command;
    let (live, rec, project) = live();
    cli_json(&live, &["session", "start", "--project", &project]);
    let chat = rec.chats.lock().unwrap()[0].clone();
    let go = live.sb.root.path().join("go");
    for (who, as_chat) in [("chat", true), ("seat", false)] {
        for (key, value) in [(WAKE, r#"["off"]"#), (HOUR, "9")] {
            let _ = std::fs::remove_file(&go);
            let script = r#"while [ ! -e "$GO" ]; do sleep 0.02; done
"$BIN" settings set "$KEY" "$VALUE" --project "$PROJECT" 2>"$ERR"
echo $? >"$RC"
"#;
            let err_file = live.sb.root.path().join("err");
            let rc_file = live.sb.root.path().join("rc");
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c")
                .arg(script)
                .env_clear()
                .env("PATH", live.sb.path_env())
                .env("HOME", live.sb.root.path())
                .env("PF_STATE_DIR", &live.sb.state)
                .env("PF_SHARE_DIR", &live.sb.share)
                .env("GO", &go)
                .env("BIN", BIN)
                .env("KEY", key)
                .env("VALUE", value)
                .env("PROJECT", &project)
                .env("ERR", &err_file)
                .env("RC", &rc_file);
            if !as_chat {
                // A seat's own mark: the controller refuses any caller that holds the token.
                cmd.env("PF_SEAT_TOKEN", "tok-test-0123456789");
            }
            let mut child = cmd.spawn().unwrap();
            if as_chat {
                rec.pids.lock().unwrap().insert(chat.clone(), child.id());
            }
            std::fs::write(&go, "").unwrap();
            child.wait().unwrap();
            rec.pids.lock().unwrap().clear();
            let rc = std::fs::read_to_string(&rc_file).unwrap();
            assert_eq!(rc.trim(), "1", "{key} as a {who}");
            let err = std::fs::read_to_string(&err_file).unwrap_or_default();
            assert!(err.contains("refused"), "{key} as a {who}: {err}");
        }
    }
    // Nothing was stored: both keys still read their built-ins.
    let scope = ["--project", project.as_str(), "--agent", "orchestrator"];
    assert_eq!(from_kind(&record(&live, WAKE, &scope)), "built_in");
    assert_eq!(from_kind(&record(&live, HOUR, &scope)), "built_in");
    // The operator's own call, with no chat above it, is admitted.
    let out = live.cli(&["settings", "set", HOUR, "9", "--project", &project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}
