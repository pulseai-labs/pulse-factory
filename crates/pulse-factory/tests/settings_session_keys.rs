//! AC-10 of `r2.s2.w3`: the four `session.*` keys (ADR-0016's amendment of 2026-11-16) get, set and
//! unset at each scope, with their ranges and built-ins. A value out of range is refused with the
//! rule. A seat or a chat caller is refused. The resume flag is `record` for the four keys and
//! `current` for the three `chat.*` keys. The controller runs in the test process on a `FakeChat`.
//! Nothing here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;

use std::process::Command;
use std::sync::Arc;

use common::chat::*;
use common::ctl::Live;
use common::{BIN, code, stderr};
use pf_engine::port::store::Store;
use pulse_factory::controller::settings::{AtResume, SESSION_KEYS, at_resume_of};
use serde_json::Value;

const REPLAY: &str = "session.replay_bytes";
const HOLD: &str = "session.exit_hold_minutes";
const WAIT: &str = "session.attach_wait_ms";
const HOOK: &str = "session.hook_wait_seconds";

fn live() -> (Live, Arc<Rec>) {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    for name in ["alpha", "beta"] {
        add_project(&live, &repo(&live, name));
    }
    (live, rec)
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

fn unset(live: &Live, key: &str, scope: &[&str]) -> Value {
    let mut args = vec!["settings", "unset", key];
    args.extend_from_slice(scope);
    cli_json(live, &args)
}

fn from_kind(r: &Value) -> &str {
    r["from"]["kind"].as_str().unwrap()
}

#[test]
fn the_four_keys_read_their_built_ins_and_the_resume_flag_is_in_the_registry() {
    let (live, _) = live();
    let all = cli_json(&live, &["settings", "get"]);
    let keys: Vec<&str> = all["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "chat.deny_commands",
            "chat.self_turn_cap",
            "chat.propose_per_min",
            REPLAY,
            HOLD,
            WAIT,
            HOOK,
            "notify.app.on",
            "notify.orchestrator.on",
            "notify.app.max_words",
            "notify.orchestrator.max_words",
            "notify.resend_seconds",
        ]
    );
    let built_ins = [
        (REPLAY, 4_194_304),
        (HOLD, 1_440),
        (WAIT, 3_000),
        (HOOK, 15),
    ];
    for (key, value) in built_ins {
        let r = record(&live, key, &[]);
        assert_eq!(r["value"], value, "{r}");
        assert_eq!(r["built_in"], value, "{r}");
        assert_eq!(from_kind(&r), "built_in", "{r}");
        assert_eq!(r["merge"], "override", "{r}");
        assert_eq!(r["applies"], "next_launch", "{r}");
    }
    // The registry says what a resume does with each key.
    for (key, ..) in SESSION_KEYS {
        assert_eq!(at_resume_of(key), Some(AtResume::Record), "{key}");
        assert_eq!(at_resume_of(key).unwrap().as_str(), "record");
    }
    for key in [
        "chat.deny_commands",
        "chat.self_turn_cap",
        "chat.propose_per_min",
    ] {
        assert_eq!(at_resume_of(key), Some(AtResume::Current), "{key}");
        assert_eq!(at_resume_of(key).unwrap().as_str(), "current");
    }
    assert_eq!(at_resume_of("session.nonesuch"), None);
}

#[test]
fn each_key_sets_gets_and_unsets_at_each_scope_and_the_most_specific_wins() {
    let (live, _) = live();
    let ctx = ["--project", "P-1", "--agent", "orchestrator"];
    for (key, lo, hi, default) in SESSION_KEYS {
        let (server, agent, project, both) = (lo + 1, lo + 2, hi - 1, hi);
        set(&live, key, &server.to_string(), &[]);
        set(&live, key, &agent.to_string(), &["--agent", "orchestrator"]);
        set(&live, key, &project.to_string(), &["--project", "P-1"]);
        let answer = set(&live, key, &both.to_string(), &ctx);
        assert_eq!(answer["value"], both, "{key}");
        assert_eq!(answer["scope"]["kind"], "project_agent", "{key}");

        let r = record(&live, key, &ctx);
        assert_eq!(
            (r["value"].as_u64(), from_kind(&r)),
            (Some(both), "project_agent")
        );
        let layers: Vec<u64> = r["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["value"].as_u64().unwrap())
            .collect();
        assert_eq!(layers, [both, project, agent, server], "{key}: {r}");

        unset(&live, key, &ctx);
        let r = record(&live, key, &ctx);
        assert_eq!(
            (r["value"].as_u64(), from_kind(&r)),
            (Some(project), "project")
        );
        unset(&live, key, &["--project", "P-1"]);
        let r = record(&live, key, &ctx);
        assert_eq!((r["value"].as_u64(), from_kind(&r)), (Some(agent), "agent"));
        unset(&live, key, &["--agent", "orchestrator"]);
        let r = record(&live, key, &ctx);
        assert_eq!(
            (r["value"].as_u64(), from_kind(&r)),
            (Some(server), "server")
        );
        unset(&live, key, &[]);
        let r = record(&live, key, &ctx);
        assert_eq!(
            (r["value"].as_u64(), from_kind(&r)),
            (Some(default), "built_in")
        );
    }
    assert!(live.controller.store().setting_rows().unwrap().is_empty());
}

#[test]
fn the_ends_of_each_range_are_accepted() {
    let (live, _) = live();
    for (key, lo, hi, _) in SESSION_KEYS {
        for edge in [lo, hi] {
            let answer = set(&live, key, &edge.to_string(), &[]);
            assert_eq!(answer["value"], edge, "{key}");
        }
        unset(&live, key, &[]);
    }
}

#[test]
fn a_value_out_of_range_is_refused_with_the_rule() {
    let (live, _) = live();
    for (key, lo, hi, _) in SESSION_KEYS {
        let rule = format!("{key} is a whole number from {lo} to {hi}");
        for bad in [
            (lo - 1).to_string(),
            (hi + 1).to_string(),
            "0".to_string(),
            "\"ten\"".to_string(),
            "1.5".to_string(),
            "-3".to_string(),
        ] {
            if bad == "0" && lo == 0 {
                continue;
            }
            let why = cli_refused(&live, &["settings", "set", key, &bad]);
            assert!(why.contains(&rule), "{key} {bad}: {why}");
        }
    }
    assert!(live.controller.store().setting_rows().unwrap().is_empty());
    // An unknown key names the keys of the release.
    let why = cli_refused(&live, &["settings", "set", "session.nonesuch", "5"]);
    assert!(why.contains("unknown setting session.nonesuch"), "{why}");
    assert!(why.contains(REPLAY) && why.contains(HOOK), "{why}");
}

// ------------------------------------------------------------------ a seat or a chat is refused

/// Who is registered as the process of the chat, or of a seat, while a shell runs.
enum As {
    Chat,
    Seat,
}

/// One CLI call below a shell that the controller takes to be `who`: its exit code and stderr.
fn below(live: &Live, rec: &Rec, who: As, args: &str) -> (i32, String) {
    let dir = live.sb.root.path().join("below");
    std::fs::create_dir_all(&dir).unwrap();
    let go = dir.join("go");
    let _ = std::fs::remove_file(&go);
    let body = format!("\"$BIN\" {args} 2>\"$DIR/cli.err\"\necho $? >\"$DIR/rc\"\n");
    let body_file = dir.join("body.sh");
    std::fs::write(&body_file, body).unwrap();
    let script = r#"while [ ! -e "$GO" ]; do sleep 0.02; done
. "$BODYFILE"
"#;
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .env_clear()
        .env("PATH", live.sb.path_env())
        .env("HOME", live.sb.root.path())
        .env("PF_STATE_DIR", &live.sb.state)
        .env("PF_SHARE_DIR", &live.sb.share)
        .env("GO", &go)
        .env("DIR", &dir)
        .env("BIN", BIN)
        .env("BODYFILE", &body_file)
        .spawn()
        .unwrap();
    match who {
        As::Chat => {
            let chat = rec.chats.lock().unwrap()[0].clone();
            rec.pids.lock().unwrap().insert(chat, child.id());
        }
        As::Seat => {
            let refs = live.session_refs();
            assert!(!refs.is_empty(), "a tracked seat needs a session row");
            live.ports
                .pids
                .lock()
                .unwrap()
                .insert(refs[0].0.clone(), child.id());
        }
    }
    std::fs::write(&go, "").unwrap();
    child.wait().unwrap();
    rec.pids.lock().unwrap().clear();
    live.ports.pids.lock().unwrap().clear();
    let rc = std::fs::read_to_string(dir.join("rc")).unwrap();
    let err = std::fs::read_to_string(dir.join("cli.err")).unwrap_or_default();
    (rc.trim().parse().unwrap(), err)
}

#[test]
fn a_chat_is_refused_on_a_session_key_and_nothing_is_stored() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, session) = started(&live, "alpha");
    assert_eq!(session, "S-1");
    let (rc, err) = below(
        &live,
        &rec,
        As::Chat,
        &format!("settings set {REPLAY} 1048576 --command-id chat-cmd-1"),
    );
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("chat process"), "{err}");
    assert!(live.controller.store().setting_rows().unwrap().is_empty());
    let (rc, err) = below(
        &live,
        &rec,
        As::Chat,
        &format!("settings unset {HOOK} --command-id chat-cmd-2"),
    );
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("chat process"), "{err}");
}

#[test]
fn a_seat_is_refused_on_a_session_key_and_nothing_is_stored() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, session) = started(&live, "alpha");
    assert_eq!(session, "S-1");
    // A seat needs a session row the controller can map a process to.
    live.arm_with_units(&["u1"]);
    let (rc, err) = below(
        &live,
        &rec,
        As::Seat,
        &format!("settings set {WAIT} 5000 --command-id seat-cmd-1"),
    );
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("seat process"), "{err}");
    assert!(live.controller.store().setting_rows().unwrap().is_empty());
}

#[test]
fn the_cli_refuses_a_session_key_it_cannot_read_and_exits_with_the_rule() {
    let (live, _) = live();
    let out = live.cli(&["settings", "set", HOOK, "0"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("1 to 120"), "{}", stderr(&out));
}
