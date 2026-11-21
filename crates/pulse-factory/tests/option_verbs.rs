//! AC-5 of `r2.s11.w2`: `answer --option`, `clear --option --rev` and `ack --option` each do what
//! the stored option says and print it. The controller runs in the test process on a
//! `FakeAdapter`; the CLI runs as a child process. A stop's own hook records the text and the
//! source that it was given, so the tests see what reached the seat. Nothing here reaches the
//! network.

mod common;
#[path = "common/needs_you_rig.rs"]
mod rig;

use common::ctl::*;
use common::*;
use serde_json::Value;

const STOP: &str =
    "Which branch?\nOptions:\n- build on main\n- [recommended] build on the spine branch\n";

/// The stop hook: it copies the answer file to `$3` and writes the source beside it.
const NOTE: &str = "#!/bin/sh\ncp \"$1\" \"$3\"\nprintf '%s\\n' \"$2\" > \"$3.source\"\n";

const STOP_HOOK: &str = r#"        stop_file:
          file: "{run.dir}/out/{unit.id}/stop"
          on_answer:
            - [sh, "{run.package}/scripts/note.sh", "{answer.file}", "{answer.source}", "{run.dir}/out/{unit.id}/noted"]
"#;

fn start() -> (Live, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    write_script(&scripts.join("note.sh"), NOTE);
    let o = Options {
        implement_stop: STOP_HOOK.to_string(),
        scripts_from: Some(scripts),
        ..Options::default()
    };
    let live = Live::start_package(BIN.into(), &o, |_| {});
    live.arm_with_units(&["u1", "u2", "u3"]);
    assert_eq!(code(&live.cli(&["claim", "--holder", "ann@host"])), 0);
    (live, dir)
}

/// The seat of `unit` writes its stop file and a tick opens the request. Returns its ID.
fn stop(live: &Live, unit: &str) -> String {
    let out = live.run_dir().join("out").join(unit);
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("stop"), STOP).unwrap();
    live.controller.tick_all();
    let view = live.handle().engine.view().unwrap();
    view.open_request_of(unit)
        .unwrap_or_else(|| panic!("no open request for {unit}"))
        .id
        .clone()
}

fn json_of(out: &std::process::Output) -> Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn noted(live: &Live, unit: &str) -> std::path::PathBuf {
    live.run_dir().join("out").join(unit).join("noted")
}

#[test]
fn answer_option_resumes_the_unit_with_the_options_text_and_prints_the_option() {
    let (live, _dir) = start();
    let id = stop(&live, "u1");
    let out = live.cli(&["answer", &id, "--rev", "1", "--option", "s2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("answered {id}@1 by claim 1 with option s2")),
        "{text}"
    );
    // The seat's hook got the option's text and the source `option:<id>:<by>`.
    assert_eq!(
        read(&noted(&live, "u1")).trim(),
        "build on the spine branch"
    );
    assert_eq!(
        read(&noted(&live, "u1").with_extension("source")).trim(),
        "option:s2:seat"
    );
    assert_eq!(live.unit_status("u1"), "running");

    // `show` of the answered request names the option, who wrote it and the recommendation.
    let shown = stdout(&live.cli(&["show", &id]));
    assert!(shown.contains("option: s2 by seat"), "{shown}");
    assert!(shown.contains("recommended: yes"), "{shown}");
    let json = json_of(&live.cli(&["show", &id, "--json"]));
    assert_eq!(
        json["answer"]["option"],
        serde_json::json!({ "id": "s2", "by": "seat", "recommended": true })
    );
    assert_eq!(json["answer"]["text"], "build on the spine branch");
}

#[test]
fn an_option_the_seat_did_not_recommend_prints_recommended_no() {
    let (live, _dir) = start();
    let id = stop(&live, "u1");
    let out = live.cli(&["answer", &id, "--rev", "1", "--option", "s1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let shown = stdout(&live.cli(&["show", &id]));
    assert!(shown.contains("option: s1 by seat"), "{shown}");
    assert!(shown.contains("recommended: no"), "{shown}");
}

#[test]
fn the_json_of_each_verb_holds_the_option() {
    let (live, _dir) = start();
    let (a, b) = (stop(&live, "u1"), stop(&live, "u2"));

    let answered = json_of(&live.cli(&["answer", &a, "--rev", "1", "--option", "s1", "--json"]));
    assert_eq!(answered["option"], "s1", "{answered}");
    assert!(answered["to_stage"].is_null(), "{answered}");

    // A typed answer holds `option: null`.
    let typed = json_of(&live.cli(&["answer", &b, "--rev", "1", "--text", "go on", "--json"]));
    assert!(typed["option"].is_null(), "{typed}");
    assert_eq!(typed["schema_version"], 1);

    // `clear` with an option, on a stall park, whose only option is `resume`.
    live.park_unit("u3", "the build is red");
    let run = live.handle().info.run_id.clone();
    let view = live.handle().engine.view().unwrap();
    let r3 = view.open_request_of("u3").unwrap().id.clone();
    let cleared = json_of(&live.cli(&[
        "clear", &run, "u3", "--option", "resume", "--rev", "1", "--json",
    ]));
    assert_eq!(cleared["option"], "resume", "{cleared}");
    assert_eq!(cleared["request"], r3.as_str(), "{cleared}");
}

#[test]
fn answer_text_with_a_stage_resumes_there_and_stores_a_leading_dash_whole() {
    let (live, _dir) = start();
    let id = stop(&live, "u1");
    let out = live.cli(&[
        "answer",
        &id,
        "--rev",
        "1",
        "--text",
        "-n is fine",
        "--to",
        "implement",
        "--json",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v = json_of(&out);
    assert_eq!(v["to_stage"], "implement", "{v}");
    assert!(v["option"].is_null(), "{v}");
    // The text is the text, not a flag, and the hook got it whole.
    assert_eq!(read(&noted(&live, "u1")).trim(), "-n is fine");
    assert_eq!(
        read(&noted(&live, "u1").with_extension("source")).trim(),
        "typed"
    );
    let shown = json_of(&live.cli(&["show", &id, "--json"]));
    assert_eq!(shown["answer"]["text"], "-n is fine");
    assert_eq!(shown["answer"]["to_stage"], "implement");
    assert!(shown["answer"].get("option").is_none(), "{shown}");
    assert_eq!(live.unit_status("u1"), "running");

    // A stage outside the track is refused and the request stays open.
    let id2 = stop(&live, "u2");
    let out = live.cli(&[
        "answer",
        &id2,
        "--rev",
        "1",
        "--text",
        "x",
        "--to",
        "no-such-stage",
    ]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("no-such-stage"), "{}", stderr(&out));
    assert_eq!(live.unit_status("u2"), "parked");
}

#[test]
fn clear_option_resumes_the_unit_and_prints_the_option() {
    let (live, _dir) = start();
    live.park_unit("u2", "the build is red");
    let run = live.handle().info.run_id.clone();
    let out = live.cli(&["clear", &run, "u2", "--option", "resume", "--rev", "1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("with option resume"), "{text}");
    assert!(text.contains("it resumes at stage implement"), "{text}");
    assert_eq!(live.unit_status("u2"), "running");
    let ledger = live.sb.read_ledger();
    assert!(
        ledger.contains("clear u2 --to implement")
            && ledger.contains("option resume by controller"),
        "{ledger}"
    );
}

#[test]
fn clear_option_at_another_revision_is_refused_naming_the_current_one() {
    let (live, _dir) = start();
    live.park_unit("u2", "the build is red");
    let run = live.handle().info.run_id.clone();
    let out = live.cli(&["clear", &run, "u2", "--option", "resume", "--rev", "4"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains('4') && err.contains('1'), "{err}");
    assert_eq!(live.unit_status("u2"), "parked");
}

#[test]
fn ack_option_approves_the_merge_and_prints_the_option() {
    let api = rig::api();
    let s = &api.s;
    let (id, head) = api.to_merge_ack();
    assert_eq!(code(&s.live.cli(&["claim", "--holder", "ann@host"])), 0);
    let shown = json_of(&s.live.cli(&["show", &id, "--json"]));
    assert_eq!(shown["decision"]["options"][0]["id"], "approve", "{shown}");
    let out = s
        .live
        .cli(&["ack", &id, "--rev", "1", "--option", "approve"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("acked {id}@1 by claim 1 with option approve")),
        "{text}"
    );
    let ledger = s.ledger();
    assert!(
        ledger.contains(&format!("ack {id}@1 by claim 1:")) && ledger.contains("option=approve"),
        "{ledger}"
    );
    assert!(ledger.contains(&format!("sha={head}")), "{ledger}");
    let after = json_of(&s.live.cli(&["show", &id, "--json"]));
    assert_eq!(after["answer"]["option"]["id"], "approve", "{after}");
    assert_eq!(after["answer"]["head_sha"], head.as_str(), "{after}");
}

#[test]
fn a_wrong_verb_is_refused_and_names_the_right_one() {
    let (live, _dir) = start();
    let id = stop(&live, "u1");
    // `s1` is an answer option: `clear` and `ack` refuse it.
    let run = live.handle().info.run_id.clone();
    let out = live.cli(&["clear", &run, "u1", "--option", "s1", "--rev", "1"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("answer"), "{}", stderr(&out));
    // A resume option on a stall park is taken with `clear`, not `answer`.
    live.park_unit("u2", "the build is red");
    let r2 = live
        .handle()
        .engine
        .view()
        .unwrap()
        .open_request_of("u2")
        .unwrap()
        .id
        .clone();
    let out = live.cli(&["answer", &r2, "--rev", "1", "--option", "resume"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("clear"),
        "the refusal names the right verb: {}",
        stderr(&out)
    );
    assert_eq!(live.unit_status("u1"), "parked", "{id}");
    assert_eq!(live.unit_status("u2"), "parked");
}

#[test]
fn the_cli_refuses_an_option_with_a_text_a_stage_a_bad_id_or_no_rev() {
    let (live, _dir) = start();
    let id = stop(&live, "u1");
    let run = live.handle().info.run_id.clone();
    let cases: Vec<Vec<&str>> = vec![
        vec!["answer", &id, "--rev", "1", "--option", "s1", "--text", "x"],
        vec![
            "answer",
            &id,
            "--rev",
            "1",
            "--option",
            "s1",
            "--to",
            "implement",
        ],
        vec!["answer", &id, "--rev", "1", "--option", "S 1"],
        vec!["answer", &id, "--rev", "1", "--option", "a/b"],
        vec!["clear", &run, "u1", "--option", "resume"],
        vec![
            "clear",
            &run,
            "u1",
            "--option",
            "resume",
            "--rev",
            "1",
            "--to",
            "implement",
        ],
        vec!["ack", &id, "--rev", "1", "--option", "no way"],
    ];
    for args in cases {
        let out = live.cli(&args);
        assert_ne!(code(&out), 0, "{args:?} was accepted: {}", stdout(&out));
    }
    // Nothing was admitted.
    assert_eq!(live.unit_status("u1"), "parked");
    assert!(
        !live.sb.read_ledger().contains("answer"),
        "{}",
        live.sb.read_ledger()
    );
}

#[test]
fn an_option_act_with_a_changed_pinned_file_is_refused_and_names_the_file() {
    let (live, dir) = start();
    let id = stop(&live, "u1");
    // The run's copy of the package is pinned at arm. A changed script is a changed pin.
    let script = live.run_dir().join("package/scripts/note.sh");
    let _ = dir;
    let original = read(&script);
    std::fs::write(&script, format!("{original}\n# changed\n")).unwrap();
    let out = live.cli(&["answer", &id, "--rev", "1", "--option", "s1"]);
    assert_eq!(code(&out), 1, "{}", stdout(&out));
    assert!(stderr(&out).contains("scripts/note.sh"), "{}", stderr(&out));
    assert_eq!(live.unit_status("u1"), "parked");
    assert!(!noted(&live, "u1").exists());
}
