//! AC-4: the command line, the refusals before any process starts, and the declared options. A shell
//! stub is the managed copy in a temporary `share_dir`, with a test pin made from its SHA-256.

mod common;

use common::*;
use pf_adapter_claude::launch::{argv, validate};
use pf_adapter_claude::pinned;
use pf_engine::port::agent::SessionRef;
use pf_engine::port::chat::{ChatError, ChatPort};

const TIMERS: &str = "ScheduleWakeup,CronCreate,CronDelete,CronList,RemoteTrigger";

fn fixed_prefix(mode: &str, model: &str, effort: &str) -> Vec<String> {
    [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
        "--allow-dangerously-skip-permissions",
        "--permission-mode",
        mode,
        "--model",
        model,
        "--effort",
        effort,
        "--disallowedTools",
        TIMERS,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn write_transcript(config_dir: &std::path::Path, id: &str) {
    let dir = config_dir.join("projects").join("-some-project");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{id}.jsonl")), "{}\n").unwrap();
}

#[test]
fn a_new_session_gets_the_exact_command_line_and_a_fresh_uuid() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let opened = c.open(&spec(work.path())).expect("open");
    assert!(opened.qualified);
    assert_eq!(opened.runtime_version, pinned::CLI_VERSION);
    let args = recorded_argv(&m);
    let mut want = fixed_prefix("bypassPermissions", "claude-opus-5-5", "high");
    want.extend(["--append-system-prompt".to_string(), "be brief".to_string()]);
    want.push("--session-id".to_string());
    assert_eq!(&args[..args.len() - 1], &want[..], "the command line");
    let id = args.last().unwrap();
    assert!(is_uuid_v4(id), "{id} is a version-4 UUID");
    assert_eq!(
        opened.session,
        SessionRef(id.clone()),
        "ChatOpened.session is that UUID"
    );
    c.stop(&opened.chat).unwrap();

    // Another session gets another id.
    let opened2 = c.open(&spec(work.path())).expect("open");
    assert_ne!(opened2.session, opened.session);
    c.stop(&opened2.chat).unwrap();
}

#[test]
fn a_resume_differs_from_a_new_session_only_in_the_session_pair() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let new = c.open(&spec(work.path())).expect("open");
    let new_args = recorded_argv(&m);
    c.stop(&new.chat).unwrap();

    let id = "5d4d8243-e0af-4b43-b611-949446c09be1";
    write_transcript(&config(&m).config_dir, id);
    let mut s = spec(work.path());
    s.resume = Some(SessionRef(id.to_string()));
    let resumed = c.open(&s).expect("a resume with a transcript opens");
    assert_eq!(resumed.session, SessionRef(id.to_string()));
    let resume_args = recorded_argv(&m);
    c.stop(&resumed.chat).unwrap();

    let n = new_args.len();
    assert_eq!(new_args[n - 2], "--session-id");
    assert_eq!(resume_args[n - 2], "--resume");
    assert_eq!(resume_args[n - 1], id);
    assert_eq!(
        &new_args[..n - 2],
        &resume_args[..n - 2],
        "the two differ only in that pair"
    );
    assert_eq!(resume_args.len(), n);
}

#[test]
fn the_command_line_is_a_pure_function_of_the_open_and_the_session_id() {
    let work = tempfile::tempdir().unwrap();
    let s = spec(work.path());
    let id = "11111111-1111-4111-8111-111111111111";
    let a = argv(&s, id);
    assert_eq!(a, argv(&s, id));
    assert_eq!(&a[a.len() - 2..], ["--session-id", id]);
    let mut resumed = s.clone();
    resumed.resume = Some(SessionRef(id.to_string()));
    let b = argv(&resumed, id);
    assert_eq!(a.len(), b.len());
    assert_eq!(&b[b.len() - 2..], ["--resume", id]);
    assert_eq!(a[..a.len() - 2], b[..b.len() - 2]);
    let mut quiet = s.clone();
    quiet.instructions.clear();
    assert!(
        !argv(&quiet, "x")
            .iter()
            .any(|x| x == "--append-system-prompt")
    );
}

#[test]
fn empty_instructions_leave_out_the_system_prompt_flag() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut s = spec(work.path());
    s.instructions = String::new();
    let o = c.open(&s).expect("open");
    let args = recorded_argv(&m);
    assert!(
        !args.iter().any(|a| a == "--append-system-prompt"),
        "{args:?}"
    );
    assert_eq!(
        args.len(),
        fixed_prefix("bypassPermissions", "claude-opus-5-5", "high").len() + 2
    );
    c.stop(&o.chat).unwrap();
}

#[test]
fn instructions_with_quotes_newlines_and_substitutions_arrive_as_one_argument() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut s = spec(work.path());
    let nasty = "say \"hi\" and 'bye'\nline two $(touch pwned) `id` ; rm -rf / \\n \u{e9}\u{1f600}";
    s.instructions = nasty.to_string();
    let o = c.open(&s).expect("open");
    let args = recorded_argv(&m);
    let at = args
        .iter()
        .position(|a| a == "--append-system-prompt")
        .unwrap();
    assert_eq!(args[at + 1], nasty, "one argument, unchanged");
    assert!(
        !work.path().join("pwned").exists(),
        "no shell ran the substitution"
    );
    c.stop(&o.chat).unwrap();
}

#[test]
fn the_flags_that_would_change_the_transcript_or_the_settings_never_appear() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let o = c.open(&spec(work.path())).expect("open");
    let args = recorded_argv(&m);
    for banned in [
        "--bare",
        "--no-session-persistence",
        "--setting-sources",
        "--max-turns",
    ] {
        assert!(!args.iter().any(|a| a == banned), "{banned}");
    }
    // The timer tools are removed from the chat.
    let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
    assert_eq!(args[at + 1], TIMERS);
    c.stop(&o.chat).unwrap();
}

#[test]
fn a_managed_copy_of_another_version_is_refused_before_any_process_starts() {
    let m = managed(
        "2.1.318 (Claude Code)",
        &format!("{ANSWER_INITIALIZE}cat > /dev/null"),
    );
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let err = c
        .open(&spec(work.path()))
        .expect_err("2.1.318 under a 2.1.319 pin");
    assert!(matches!(err, ChatError::Refused(_)), "{err}");
    assert!(err.to_string().contains("2.1.318"), "{err}");
    assert_eq!(
        recorded_calls(&m),
        vec!["version"],
        "no call other than --version"
    );
    assert!(!m.dir.join("argv").exists());
}

#[test]
fn a_resume_with_no_transcript_is_refused_and_starts_no_process() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut s = spec(work.path());
    s.resume = Some(SessionRef("5d4d8243-e0af-4b43-b611-949446c09be1".into()));
    let err = c.open(&s).expect_err("no transcript");
    assert!(matches!(err, ChatError::ResumeFailed(_)), "{err}");
    assert!(
        recorded_calls(&m).is_empty(),
        "the stub did not run at all: {:?}",
        recorded_calls(&m)
    );

    // A transcript of another session does not count.
    write_transcript(
        &config(&m).config_dir,
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    );
    let err = c.open(&s).expect_err("a transcript of another id");
    assert!(matches!(err, ChatError::ResumeFailed(_)), "{err}");
    assert!(recorded_calls(&m).is_empty());
}

#[test]
fn a_bad_cwd_model_effort_mode_artifacts_value_or_option_key_is_refused() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    type Edit = Box<dyn Fn(&mut pf_engine::port::chat::ChatOpen)>;
    let cases: Vec<(&str, Edit)> = vec![
        (
            "a missing cwd",
            Box::new(|s| s.cwd = "/no/such/directory/anywhere".into()),
        ),
        (
            "a cwd that is a file",
            Box::new(|s| {
                let f = std::env::temp_dir().join("pf-claude-not-a-dir");
                std::fs::write(&f, "x").unwrap();
                s.cwd = f.display().to_string();
            }),
        ),
        (
            "a model with a space",
            Box::new(|s| s.model = "claude opus".into()),
        ),
        ("an empty model", Box::new(|s| s.model = String::new())),
        (
            "a model with a shell character",
            Box::new(|s| s.model = "x;y".into()),
        ),
        (
            "a model of 65 characters",
            Box::new(|s| s.model = "m".repeat(65)),
        ),
        (
            "an effort outside its values",
            Box::new(|s| s.effort = "ultra".into()),
        ),
        (
            "a mode outside its values",
            Box::new(|s| s.permission_mode = "yolo".into()),
        ),
        (
            "an artifacts value other than on or off",
            Box::new(|s| {
                s.options.insert("artifacts".into(), "maybe".into());
            }),
        ),
        (
            "any other options key",
            Box::new(|s| {
                s.options.insert("verbosity".into(), "loud".into());
            }),
        ),
        (
            "a field option in the map",
            Box::new(|s| {
                s.options.insert("model".into(), "opus".into());
            }),
        ),
    ];
    for (name, change) in cases {
        let mut s = spec(work.path());
        change(&mut s);
        let err = c.open(&s).expect_err(name);
        assert!(matches!(err, ChatError::Refused(_)), "{name}: {err}");
    }
    assert!(
        recorded_calls(&m).is_empty(),
        "no refusal reached the binary: {:?}",
        recorded_calls(&m)
    );
    // The same checks, as a pure function.
    assert!(validate(&spec(work.path())).is_ok());
    let mut bad = spec(work.path());
    bad.effort = "ultra".into();
    assert!(validate(&bad).is_err());
    for model in [
        "opus",
        "claude-opus-5-5",
        "claude-haiku-4-5-20251001",
        "custom.model:v1[1m]",
    ] {
        let mut s = spec(work.path());
        s.model = model.into();
        assert!(validate(&s).is_ok(), "{model}");
    }
}

#[test]
fn options_declare_the_four_options_with_their_defaults_and_liveness() {
    let m = good_stub();
    let c = chat(&m);
    let options = c.options();
    let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, vec!["model", "effort", "permission_mode", "artifacts"]);
    let by = |id: &str| options.iter().find(|o| o.id == id).unwrap();
    let model = by("model");
    assert!(model.free && model.live);
    assert!(model.values.iter().any(|v| v == "opus"));
    assert!(
        model.values.iter().any(|v| v == "claude-opus-5-5"),
        "the id w2 seeds"
    );
    let effort = by("effort");
    assert_eq!(effort.values, vec!["low", "medium", "high", "xhigh", "max"]);
    assert!(effort.live && !effort.free);
    let mode = by("permission_mode");
    assert_eq!(mode.default, "bypassPermissions");
    assert_eq!(
        mode.values,
        vec![
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "manual",
            "dontAsk",
            "plan"
        ]
    );
    assert!(mode.live && !mode.free);
    let artifacts = by("artifacts");
    assert_eq!(artifacts.values, vec!["on", "off"]);
    assert_eq!(artifacts.default, "on");
    assert!(!artifacts.live, "the CLI reads the variable at start");
    assert!(!artifacts.free);
}

#[test]
fn set_option_of_artifacts_is_refused_because_it_is_not_live() {
    let m = good_stub();
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let o = c.open(&spec(work.path())).expect("open");
    let err = c
        .set_option(&o.chat, "artifacts", "off")
        .expect_err("not live");
    assert!(matches!(err, ChatError::Refused(_)), "{err}");
    let err = c
        .set_option(&o.chat, "no_such_option", "x")
        .expect_err("unknown");
    assert!(matches!(err, ChatError::Refused(_)), "{err}");
    c.stop(&o.chat).unwrap();
}
