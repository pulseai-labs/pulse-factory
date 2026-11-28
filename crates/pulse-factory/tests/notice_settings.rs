//! AC-9 of `r2.s11.w3`: the five `notify.*` keys (ADR-0016's amendment of 2026-11-21) get, set and
//! unset at each scope, with their ranges and built-ins. A value out of range is refused with the
//! rule. A project value masks the server value. A chat caller is refused, and its refusals are
//! bounded as for the other keys. The controller runs in the test process on a `FakeChat`. Nothing
//! here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;

use std::process::Command;
use std::sync::Arc;

use common::chat::*;
use common::ctl::Live;
use common::{BIN, code, stderr};
use pf_engine::port::store::Store;
use pulse_factory::controller::settings::{AtResume, NOTIFY_KEYS, at_resume_of};
use serde_json::Value;

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
fn the_five_keys_read_their_built_ins_and_apply_at_the_next_notice() {
    let (live, _) = live();
    assert_eq!(NOTIFY_KEYS.len(), 5);
    let built_ins = [
        ("notify.app.on", 1, 0, 1),
        ("notify.orchestrator.on", 1, 0, 1),
        ("notify.app.max_words", 30, 5, 200),
        ("notify.orchestrator.max_words", 100, 10, 500),
        ("notify.resend_seconds", 60, 10, 3600),
    ];
    for (key, default, lo, hi) in built_ins {
        let r = record(&live, key, &[]);
        assert_eq!(r["value"], default, "{r}");
        assert_eq!(r["built_in"], default, "{r}");
        assert_eq!(from_kind(&r), "built_in", "{r}");
        assert_eq!(r["merge"], "override", "{r}");
        // `next_turn` stands for the next notice (a third wire value is `r2.s11.w5`'s).
        assert_eq!(r["applies"], "next_turn", "{r}");
        assert!(NOTIFY_KEYS.contains(&(key, lo, hi, default)), "{key}");
        assert_eq!(at_resume_of(key), Some(AtResume::Current), "{key}");
    }
    // The fourteen keys come in the order of the registry: the five `notify.*` keys come after the seven
    // before them, and `authority.clear_acts` and `authority.rerun_checks_per_head` are last.
    let all = cli_json(&live, &["settings", "get"]);
    let keys: Vec<&str> = all["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 14, "{keys:?}");
    assert_eq!(
        &keys[7..12],
        [
            "notify.app.on",
            "notify.orchestrator.on",
            "notify.app.max_words",
            "notify.orchestrator.max_words",
            "notify.resend_seconds",
        ]
    );
}

#[test]
fn each_key_takes_its_range_and_refuses_a_value_outside_it() {
    let (live, _) = live();
    for (key, lo, hi, _) in NOTIFY_KEYS {
        for edge in [lo, hi] {
            let answer = set(&live, key, &edge.to_string(), &[]);
            assert_eq!(answer["value"], edge, "{key}");
        }
        unset(&live, key, &[]);
        let rule = format!("{key} is a whole number from {lo} to {hi}");
        for bad in [hi + 1, hi + 100]
            .map(|n| n.to_string())
            .into_iter()
            .chain(
                ["\"ten\"", "1.5", "-3", "true"]
                    .into_iter()
                    .map(String::from),
            )
            .chain((lo > 0).then(|| (lo - 1).to_string()))
        {
            let why = cli_refused(&live, &["settings", "set", key, &bad]);
            assert!(why.contains(&rule), "{key} {bad}: {why}");
        }
    }
    assert!(live.controller.store().setting_rows().unwrap().is_empty());
    // An unknown key names the keys of the release, the new ones too.
    let why = cli_refused(&live, &["settings", "set", "notify.nonesuch", "5"]);
    assert!(why.contains("unknown setting notify.nonesuch"), "{why}");
    assert!(
        why.contains("notify.app.on") && why.contains("notify.resend_seconds"),
        "{why}"
    );
}

#[test]
fn a_project_value_masks_the_server_value_and_the_orchestrator_keys_resolve_at_the_agent() {
    let (live, _) = live();
    set(&live, "notify.app.max_words", "40", &[]);
    set(&live, "notify.app.max_words", "12", &["--project", "P-1"]);
    let r = record(&live, "notify.app.max_words", &["--project", "P-1"]);
    assert_eq!((r["value"].as_u64(), from_kind(&r)), (Some(12), "project"));
    let masked: Vec<u64> = r["masked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["value"].as_u64().unwrap())
        .collect();
    assert_eq!(masked, [40], "{r}");
    // Another project reads the server value.
    let r = record(&live, "notify.app.max_words", &["--project", "P-2"]);
    assert_eq!((r["value"].as_u64(), from_kind(&r)), (Some(40), "server"));

    // An orchestrator key also takes a value at the agent, and the project and agent win over both.
    let ctx = ["--project", "P-1", "--agent", "orchestrator"];
    set(
        &live,
        "notify.orchestrator.on",
        "0",
        &["--agent", "orchestrator"],
    );
    let r = record(&live, "notify.orchestrator.on", &ctx);
    assert_eq!((r["value"].as_u64(), from_kind(&r)), (Some(0), "agent"));
    set(&live, "notify.orchestrator.on", "1", &ctx);
    let r = record(&live, "notify.orchestrator.on", &ctx);
    assert_eq!(
        (r["value"].as_u64(), from_kind(&r)),
        (Some(1), "project_agent")
    );
    unset(&live, "notify.orchestrator.on", &ctx);
    unset(
        &live,
        "notify.orchestrator.on",
        &["--agent", "orchestrator"],
    );
    unset(&live, "notify.app.max_words", &["--project", "P-1"]);
    unset(&live, "notify.app.max_words", &[]);
    assert!(live.controller.store().setting_rows().unwrap().is_empty());
}

// ------------------------------------------------------------------ a chat is refused

/// Who is registered as the process of the chat, or of a seat, while a shell runs.
enum As {
    Chat,
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
fn a_chat_is_refused_on_a_notify_key_and_its_refusals_are_bounded_as_for_the_other_keys() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, session) = started(&live, "alpha");
    assert_eq!(session, "S-1");
    for (n, key) in ["notify.app.on", "notify.resend_seconds"]
        .iter()
        .enumerate()
    {
        let (rc, err) = below(
            &live,
            &rec,
            As::Chat,
            &format!("settings set {key} 20 --command-id chat-notify-{n}"),
        );
        assert_eq!(rc, 1, "{err}");
        assert!(err.contains("chat process"), "{err}");
    }
    let (rc, err) = below(
        &live,
        &rec,
        As::Chat,
        "settings unset notify.orchestrator.max_words --command-id chat-notify-9",
    );
    assert_eq!(rc, 1, "{err}");
    assert!(live.controller.store().setting_rows().unwrap().is_empty());
    // Each key is a bucket of its own, so the audit names the key, and an unknown key is `other`.
    let lines = controller_ledger(&live);
    for bucket in [
        "notify.app.on",
        "notify.resend_seconds",
        "notify.orchestrator.max_words",
    ] {
        let n = lines
            .iter()
            .filter(|l| l.contains(&format!("bucket={bucket} ")))
            .count();
        assert_eq!(n, 1, "{bucket}: {lines:#?}");
    }
    // The operator at the CLI is not refused.
    let out = live.cli(&["settings", "set", "notify.resend_seconds", "20"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn max_words_applies_to_the_next_notice_and_cuts_at_a_whole_word() {
    let (live, _) = live();
    live.arm_with_units(&["u1"]);
    live.park_unit("u1", "the reviewer found a defect");
    let first = live.controller.store().notice_deliveries(1).unwrap();
    let app = first
        .iter()
        .find(|d| d.channel == "app")
        .unwrap()
        .text
        .clone();
    assert!(app.split_whitespace().count() > 6, "{app}");
    assert!(
        !app.ends_with('…'),
        "the default cap of 30 words cuts nothing: {app}"
    );
    // A new cap does not touch a notice that is written: it rules the next one.
    set(&live, "notify.app.max_words", "6", &[]);
    let unchanged = live.controller.store().notice_deliveries(1).unwrap();
    assert_eq!(unchanged, first);
    let (id, _) = {
        let view = live.handle().engine.view().unwrap();
        let r = view.open_request_of("u1").unwrap();
        (r.id.clone(), r.revision)
    };
    let row = live.controller.store().request(&id).unwrap().unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = serde_json::json!("changed");
    live.handle().engine.revise_request(&id, 1, &body).unwrap();
    live.controller.tick_all();
    let second = live.controller.store().notice_deliveries(2).unwrap();
    let text = &second.iter().find(|d| d.channel == "app").unwrap().text;
    // Six whole words, then the mark of a cut: `Changed: ` counts for the cap.
    let words: Vec<&str> = text.trim_end_matches('…').split_whitespace().collect();
    assert_eq!(words.len(), 6, "{text}");
    assert!(
        text.ends_with('…') && text.starts_with("Changed: "),
        "{text}"
    );
    // The orchestrator channel keeps its own cap.
    let orch = &second
        .iter()
        .find(|d| d.channel == "orchestrator")
        .unwrap()
        .text;
    assert!(!orch.ends_with('…'), "{orch}");
}
