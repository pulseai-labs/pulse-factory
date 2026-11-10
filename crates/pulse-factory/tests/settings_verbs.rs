//! AC-9 of `r1.s4.w4`: `settings get`, `settings set` and `settings unset` through the socket. The
//! controller runs in the test process on a `FakeChat`, and the CLI runs as a child process against
//! its socket. Nothing here spends money, starts `claude` or reaches beyond `127.0.0.1`.

mod common;

use std::sync::Arc;

use common::chat::*;
use common::ctl::Live;
use common::{code, stderr, stdout};
use pf_engine::port::store::{AdmissionOutcome, CONTROLLER_SCOPE, SettingRow, Store};
use serde_json::{Value, json};

/// The fifteen built-in deny patterns, in order (ruling 9).
const BUILT_IN: [&str; 15] = [
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

const CAP: &str = "chat.self_turn_cap";
const DENY: &str = "chat.deny_commands";

fn live() -> (Live, Arc<Rec>) {
    let rec = Rec::new(strict_fake());
    (start(rec.clone()), rec)
}

/// A live controller with two projects, `P-1` and `P-2`.
fn live_with_projects() -> (Live, Arc<Rec>) {
    let (live, rec) = live();
    for name in ["alpha", "beta"] {
        add_project(&live, &repo(&live, name));
    }
    (live, rec)
}

/// `settings get <args> --json`, as one record when a key is named.
fn get(live: &Live, args: &[&str]) -> Value {
    let mut all = vec!["settings", "get"];
    all.extend_from_slice(args);
    cli_json(live, &all)
}

fn record(live: &Live, key: &str, scope: &[&str]) -> Value {
    let mut args = vec![key];
    args.extend_from_slice(scope);
    let v = get(live, &args);
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

fn rows(live: &Live) -> Vec<SettingRow> {
    live.controller.store().setting_rows().unwrap()
}

fn admissions(live: &Live) -> usize {
    live.controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .len()
}

fn setting_lines(live: &Live) -> Vec<String> {
    controller_ledger(live)
        .into_iter()
        .filter(|l| l.contains(" setting_set ") || l.contains(" setting_unset "))
        .collect()
}

fn from_kind(r: &Value) -> &str {
    r["from"]["kind"].as_str().unwrap()
}

fn masked_values(r: &Value) -> Vec<Value> {
    r["masked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["value"].clone())
        .collect()
}

#[test]
fn a_fresh_store_reads_the_two_built_ins() {
    let (live, _) = live();
    let v = get(&live, &[]);
    assert_eq!(v["schema_version"], 1);
    let settings = v["settings"].as_array().unwrap();
    assert_eq!(settings.len(), 2, "{v}");
    let keys: Vec<&str> = settings
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, [DENY, CAP]);
    for r in settings {
        assert_eq!(from_kind(r), "built_in", "{r}");
        assert_eq!(r["merge"], "override", "{r}");
        assert!(r["masked"].as_array().unwrap().is_empty(), "{r}");
        assert!(r["layers"].as_array().unwrap().is_empty(), "{r}");
        assert_eq!(r["value"], r["built_in"], "{r}");
    }
    assert_eq!(settings[1]["value"], 3);
    assert_eq!(settings[1]["applies"], "next_turn");
    assert_eq!(settings[0]["value"], json!(BUILT_IN));
    assert_eq!(settings[0]["applies"], "next_launch");

    let out = live.cli(&["settings", "get"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("chat.self_turn_cap 3 (built-in)"), "{text}");
    assert!(
        text.contains("chat.deny_commands [\"git push -f*\""),
        "{text}"
    );
    assert!(text.contains("(built-in)"), "{text}");
    assert!(rows(&live).is_empty());
}

#[test]
fn the_most_specific_scope_wins_and_each_unset_uncovers_the_next() {
    let (live, _) = live_with_projects();
    let ctx = ["--project", "P-1", "--agent", "orchestrator"];

    set(&live, CAP, "5", &[]);
    set(&live, CAP, "6", &["--agent", "orchestrator"]);
    set(&live, CAP, "7", &["--project", "P-1"]);
    let answer = set(&live, CAP, "8", &ctx);
    assert_eq!(answer["schema_version"], 1);
    assert_eq!(answer["replayed"], false);
    assert_eq!(answer["value"], 8);
    assert_eq!(answer["scope"]["kind"], "project_agent");

    let r = record(&live, CAP, &ctx);
    assert_eq!(r["value"], 8);
    assert_eq!(from_kind(&r), "project_agent");
    let layers: Vec<i64> = r["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["value"].as_i64().unwrap())
        .collect();
    assert_eq!(layers, [8, 7, 6, 5], "most specific first: {r}");
    // Ruling 7: the winner hides the three other rows.
    assert_eq!(masked_values(&r), [json!(7), json!(6), json!(5)]);

    let gone = unset(&live, CAP, &ctx);
    assert_eq!(gone["value"], Value::Null);
    let r = record(&live, CAP, &ctx);
    assert_eq!((r["value"].as_i64(), from_kind(&r)), (Some(7), "project"));
    unset(&live, CAP, &["--project", "P-1"]);
    let r = record(&live, CAP, &ctx);
    assert_eq!((r["value"].as_i64(), from_kind(&r)), (Some(6), "agent"));

    // A second project with no rows reads the agent's row while it stands.
    let r = record(&live, CAP, &["--project", "P-2", "--agent", "orchestrator"]);
    assert_eq!((r["value"].as_i64(), from_kind(&r)), (Some(6), "agent"));

    unset(&live, CAP, &["--agent", "orchestrator"]);
    let r = record(&live, CAP, &ctx);
    assert_eq!((r["value"].as_i64(), from_kind(&r)), (Some(5), "server"));
    unset(&live, CAP, &[]);
    let r = record(&live, CAP, &ctx);
    assert_eq!((r["value"].as_i64(), from_kind(&r)), (Some(3), "built_in"));
    assert!(rows(&live).is_empty());
}

#[test]
fn a_more_specific_value_masks_the_others_and_get_names_them() {
    let (live, _) = live_with_projects();
    let ctx = ["--project", "P-1", "--agent", "orchestrator"];
    set(
        &live,
        DENY,
        r#"["npm publish"]"#,
        &["--agent", "orchestrator"],
    );
    set(&live, DENY, r#"["cargo publish"]"#, &["--project", "P-1"]);

    let r = record(&live, DENY, &ctx);
    assert_eq!(r["value"], json!(["cargo publish"]));
    assert_eq!(from_kind(&r), "project");
    assert_eq!(r["merge"], "override");
    let masked = r["masked"].as_array().unwrap();
    assert_eq!(masked.len(), 1, "{r}");
    assert_eq!(masked[0]["scope"]["kind"], "agent");
    assert_eq!(masked[0]["scope"]["agent_id"], "orchestrator");
    assert_eq!(masked[0]["value"], json!(["npm publish"]));

    let out = live.cli(&[
        "settings",
        "get",
        DENY,
        "--project",
        "P-1",
        "--agent",
        "orchestrator",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(r#"chat.deny_commands ["cargo publish"] (project P-1)"#),
        "{text}"
    );
    assert!(
        text.contains(r#"  masks agent:orchestrator ["npm publish"]"#),
        "{text}"
    );

    // A server row too: the agent and server rows are masked, most specific first.
    set(&live, DENY, r#"["pip publish"]"#, &[]);
    let r = record(&live, DENY, &ctx);
    assert_eq!(
        masked_values(&r),
        [json!(["npm publish"]), json!(["pip publish"])]
    );

    // The four rows of the cap: the pair's value hides the three others.
    set(&live, CAP, "5", &[]);
    set(&live, CAP, "6", &["--agent", "orchestrator"]);
    set(&live, CAP, "7", &["--project", "P-1"]);
    set(&live, CAP, "8", &ctx);
    let r = record(&live, CAP, &ctx);
    assert_eq!(masked_values(&r).len(), 3, "{r}");
}

#[test]
fn each_admitted_write_is_one_admission_and_one_ledger_line() {
    let (live, _) = live_with_projects();
    let before_adm = admissions(&live);
    let before_lines = setting_lines(&live).len();

    let out = live.cli(&[
        "settings",
        "set",
        CAP,
        "7",
        "--project",
        "P-1",
        "--command-id",
        "set-1",
        "--json",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = live.cli(&[
        "settings",
        "unset",
        CAP,
        "--project",
        "P-1",
        "--command-id",
        "unset-1",
        "--json",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let lines = setting_lines(&live);
    assert_eq!(lines.len(), before_lines + 2, "{lines:#?}");
    let set_line = &lines[before_lines];
    let unset_line = &lines[before_lines + 1];
    assert!(
        set_line.contains(
            "setting_set chat.self_turn_cap admitted scope=project:P-1 value=7 by=cli command=set-1"
        ),
        "{set_line}"
    );
    assert!(
        unset_line.contains(
            "setting_unset chat.self_turn_cap admitted scope=project:P-1 by=cli command=unset-1"
        ),
        "{unset_line}"
    );
    let all = live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap();
    assert_eq!(all.len(), before_adm + 2);
    let tail = &all[all.len() - 2..];
    assert_eq!(tail[0].command_id, "set-1");
    assert_eq!(tail[0].kind, "setting_set");
    assert_eq!(tail[0].outcome, AdmissionOutcome::Admitted);
    assert_eq!(
        tail[0].unit_id.as_deref(),
        Some("chat.self_turn_cap@project:P-1")
    );
    assert_eq!(tail[1].command_id, "unset-1");
    assert_eq!(tail[1].kind, "setting_unset");

    // The row's `set_by` is `cli`.
    set(&live, CAP, "9", &["--project", "P-1"]);
    let stored = rows(&live);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].set_by, "cli");
    assert_eq!(stored[0].value, "9");
}

/// A refused write: exit 1 with the reason, one refused admission, one line and no row change.
fn refused_write(live: &Live, args: &[&str], reason: &str) {
    let rows_before = rows(live);
    let adm_before = admissions(live);
    let lines_before = setting_lines(live).len();
    let why = cli_refused(live, args);
    assert!(
        why.contains(reason),
        "{args:?}: wanted {reason:?} in {why:?}"
    );
    assert_eq!(rows(live), rows_before, "{args:?}: no row changes");
    assert_eq!(admissions(live), adm_before + 1, "{args:?}: one admission");
    let all = live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap();
    assert_eq!(
        all.last().unwrap().outcome,
        AdmissionOutcome::Refused,
        "{args:?}"
    );
    let lines = setting_lines(live);
    assert_eq!(lines.len(), lines_before + 1, "{args:?}: one line");
    assert!(lines.last().unwrap().contains(" refused "), "{lines:#?}");
}

#[test]
fn a_bad_write_is_refused_with_its_reason_and_audited_once() {
    let (live, _) = live_with_projects();
    // A removed project: P-3 is added, then removed.
    let gone = add_project(&live, &repo(&live, "gone"));
    assert_eq!(gone, "P-3");
    let out = live.cli(&["project", "remove", &gone]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    refused_write(
        &live,
        &["settings", "set", "claim.required", "1"],
        "unknown setting claim.required",
    );
    // clap passes `-1` through, so the refusal is the controller's.
    refused_write(
        &live,
        &["settings", "set", CAP, "-1"],
        "whole number from 0 to 100",
    );
    refused_write(
        &live,
        &["settings", "set", CAP, "101"],
        "whole number from 0 to 100",
    );
    refused_write(
        &live,
        &["settings", "set", CAP, "\"3\""],
        "whole number from 0 to 100",
    );
    refused_write(
        &live,
        &["settings", "set", CAP, "3.5"],
        "whole number from 0 to 100",
    );
    refused_write(
        &live,
        &["settings", "set", DENY, r#"["git push * :*"]"#],
        ":*",
    );
    let many: Vec<String> = (0..33).map(|n| format!("tool{n} run")).collect();
    refused_write(
        &live,
        &[
            "settings",
            "set",
            DENY,
            &serde_json::to_string(&many).unwrap(),
        ],
        "32",
    );
    refused_write(
        &live,
        &["settings", "set", DENY, r#"["npm publish","npm publish"]"#],
        "twice",
    );
    refused_write(
        &live,
        &["settings", "set", CAP, "2", "--project", "P-9"],
        "no project P-9",
    );
    refused_write(
        &live,
        &["settings", "set", CAP, "2", "--project", &gone],
        "removed",
    );
    refused_write(
        &live,
        &["settings", "set", CAP, "2", "--agent", "nobody"],
        "no agent nobody",
    );
    refused_write(
        &live,
        &["settings", "unset", CAP, "--project", "P-1"],
        "no value of chat.self_turn_cap is set at project:P-1",
    );
    assert!(rows(&live).is_empty());

    // `abc` is not JSON: the CLI refuses it with no call, so nothing is admitted.
    let adm = admissions(&live);
    let why = cli_refused(&live, &["settings", "set", CAP, "abc"]);
    assert!(why.contains("not JSON"), "{why}");
    assert_eq!(admissions(&live), adm, "no call reached the controller");
}

#[test]
fn a_replay_answers_replayed_and_a_changed_value_is_a_conflict() {
    let (live, _) = live_with_projects();
    let args = |value: &'static str| {
        vec![
            "settings",
            "set",
            CAP,
            value,
            "--project",
            "P-1",
            "--command-id",
            "same-id",
        ]
    };
    let first = cli_json(&live, &args("4"));
    assert_eq!(first["replayed"], false);
    let lines = setting_lines(&live).len();
    let adm = admissions(&live);

    let again = cli_json(&live, &args("4"));
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(setting_lines(&live).len(), lines, "a replay writes no line");
    assert_eq!(admissions(&live), adm, "a replay admits nothing");

    let mut other = args("5");
    other.push("--json");
    let why = cli_refused(&live, &other);
    assert!(why.contains("same-id"), "{why}");
    assert_eq!(rows(&live)[0].value, "4", "the row is untouched");
    assert_eq!(setting_lines(&live).len(), lines);
}

/// A command ID is for one verb: the verb is part of the content, so a set under the ID of an
/// unset is a conflict, never a replay (review B1).
#[test]
fn a_set_under_the_id_of_an_unset_is_a_conflict_and_not_a_replay() {
    let (live, _) = live();
    let first = cli_json(
        &live,
        &["settings", "set", CAP, "4", "--command-id", "seed"],
    );
    assert_eq!(first["replayed"], false, "{first}");
    let gone = cli_json(&live, &["settings", "unset", CAP, "--command-id", "reused"]);
    assert_eq!(gone["replayed"], false, "{gone}");
    assert!(rows(&live).is_empty(), "the unset deleted the row");
    let adm = admissions(&live);
    let lines = setting_lines(&live).len();

    // `null` reads as no value, so this set's fields equal the unset's. It must not replay it.
    let why = cli_refused(
        &live,
        &[
            "settings",
            "set",
            CAP,
            "null",
            "--command-id",
            "reused",
            "--json",
        ],
    );
    assert!(why.contains("reused"), "{why}");
    assert!(rows(&live).is_empty());
    assert_eq!(admissions(&live), adm, "a conflict admits nothing");
    assert_eq!(setting_lines(&live).len(), lines, "and writes no line");

    // And a valid set under that ID is a conflict too: the unset's ID is spent.
    let why = cli_refused(
        &live,
        &[
            "settings",
            "set",
            CAP,
            "9",
            "--command-id",
            "reused",
            "--json",
        ],
    );
    assert!(why.contains("reused"), "{why}");
    assert!(rows(&live).is_empty());
}

#[test]
fn a_caller_that_descends_from_a_live_chat_can_read_but_not_write() {
    use pf_ipc::Handler;
    // The caller is this test process, whose parent in a fake `/proc` is the chat's process 4242.
    let chat_proc = tempfile::tempdir().unwrap();
    fake_proc(chat_proc.path(), b"HOME=/x\0", 4242);
    let root = chat_proc.path().to_path_buf();
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), move |cfg| cfg.proc_root = root);
    // The CLI child would not pass a fake `/proc`, so the session starts through the controller.
    let added = live.controller.handle(pf_ipc::Request::new(
        "project_add",
        json!({"root": repo(&live, "alpha").display().to_string(), "name": null, "command_id": "add-1"}),
    ));
    assert!(added.ok, "{:?}", added.error);
    let started = live.controller.handle(pf_ipc::Request::new(
        "session_start",
        json!({"project_id": "P-1", "command_id": "start-1"}),
    ));
    assert!(started.ok, "{:?}", started.error);
    rec.pids
        .lock()
        .unwrap()
        .insert(rec.chats.lock().unwrap()[0].clone(), 4242);

    // The read answers for a descendant of the live chat.
    let resp = pf_ipc::call(&live.sb.state, "settings_get", json!({})).unwrap();
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result["settings"].as_array().unwrap().len(), 2);
    // The write is refused, and names the chat process. Its audit is AC-25's.
    let resp = pf_ipc::call(
        &live.sb.state,
        "settings_set",
        json!({"key": CAP, "value": 9, "command_id": "from-the-chat"}),
    )
    .unwrap();
    assert!(!resp.ok);
    assert!(
        resp.error.unwrap().contains("chat process 4242"),
        "the refusal names the chat process"
    );
    assert!(rows(&live).is_empty());
}

#[test]
fn halt_does_not_touch_settings() {
    let (live, _) = live_with_projects();
    // `halt` with no run may answer either way; the settings commands do not depend on it.
    let _ = live.cli(&["halt", "--reason", "settings test"]);
    let v = set(&live, CAP, "2", &[]);
    assert_eq!(v["value"], 2, "{v}");
    assert_eq!(rows(&live).len(), 1);
}

#[test]
fn a_stopped_controller_ledger_refuses_a_write_and_still_reads() {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec, |cfg| {
        // A directory takes the ledger's place, so the controller's commands are stopped.
        std::fs::create_dir(cfg.state_dir.join("controller.ledger")).unwrap();
    });
    let why = cli_refused(&live, &["settings", "set", CAP, "2"]);
    assert!(why.contains("stopped"), "{why}");
    assert!(rows(&live).is_empty());
    let r = record(&live, CAP, &[]);
    assert_eq!(r["value"], 3, "the read still answers: {r}");
}
