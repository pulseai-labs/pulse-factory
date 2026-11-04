//! AC-2: a workflow-declared answer hook. `on_answer` is an optional list of argv commands on a
//! `park_on` entry and on the stage's `stop_file`. It runs when an answer resumes the unit, before
//! the unit becomes `running`. A failing hook leaves the request answered and parks the unit
//! again, with a new request. A `clear` runs no hook.

#[path = "../../pf-engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{Env, TrackedSource, env, item_workflow, unit};
use pf_engine::claim::sha256_hex;
use pf_engine::launch::LaunchStep;
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus};
use pf_engine::render::{RenderContext, UnitView, render};
use pf_engine::{EndStatus, Status};
use pf_workflow::WorkflowFile;
use pf_workflow::commands::{AnswerOutcome, CommandSpec, answer_through_hook};
use pf_workflow::package::Package;

// ------------------------------------------------------------------ the file

const BASE: &str = r#"version: 1
name: demo-flow
params: [repo, briefs, spine_dir]
roles:
  worker:
    runtime: omp
    model: small-model
    rules: rules/worker.yaml
unit_sources:
  - dir: "{run.dir}/queue"
    track: item
    vars: [handoff_path]
tracks:
  item:
    stages:
      - name: build
        role: worker
        brief: [briefs/item.md]
        message: "Build {unit.vars.handoff_path}."
        cwd: "{param.repo}"
        stall_minutes: 15
        first_turn_seconds: 180
@STOP@
        done:
          - file: "{run.dir}/out/{unit.id}/report.md"
@PARK@
"#;

const STOP_PLAIN: &str = r#"        stop_file: "{run.dir}/out/{unit.id}/stop""#;

const STOP_HOOK: &str = r#"        stop_file:
          file: "{run.dir}/out/{unit.id}/stop"
          on_answer:
            - [sh, "{run.package}/scripts/rule.sh", "{run.dir}/out/{unit.id}/rulings.md", "{answer.file}", "{answer.request}", "{answer.revision}", "{answer.generation}", "{request.body_file}"]"#;

const PARK_HOOK: &str = r#"        park_on:
          - file: "{run.dir}/out/{unit.id}/gaps.json"
            reason: "gaps surfaced for {unit.id}"
            on_answer:
              - [sh, "{run.package}/scripts/clarify.sh", "{unit.vars.handoff_path}", "{request.body_file}", "{answer.file}", "{answer.request}", "{answer.generation}", "{param.spine_dir}"]
              - argv: [true]
                timeout_seconds: 5"#;

fn flow(stop: &str, park: &str) -> String {
    BASE.replace("@STOP@", stop).replace("@PARK@", park)
}

fn parse(text: &str) -> Result<WorkflowFile, String> {
    WorkflowFile::parse(text).map_err(|e| e.to_string())
}

fn refused(text: &str) -> String {
    match parse(text) {
        Ok(_) => panic!("the file should be refused:\n{text}"),
        Err(e) => e,
    }
}

#[test]
fn on_answer_validates_on_a_park_on_entry_and_on_the_stop_file() {
    let file = parse(&flow(STOP_HOOK, PARK_HOOK)).unwrap_or_else(|e| panic!("{e}"));
    let stage = &file.track("item").unwrap().stages[0];

    assert_eq!(stage.stop_file, "{run.dir}/out/{unit.id}/stop");
    assert_eq!(stage.stop_on_answer.len(), 1);
    assert_eq!(stage.stop_on_answer[0].argv[0], "sh");
    assert_eq!(stage.stop_on_answer[0].argv.len(), 8);

    assert_eq!(stage.park_on.len(), 1);
    let hook = &stage.park_on[0].on_answer;
    assert_eq!(hook.len(), 2, "two commands, in order");
    assert_eq!(hook[0].argv[1], "{run.package}/scripts/clarify.sh");
    assert_eq!(hook[0].timeout_seconds, 60, "the default time bound");
    assert_eq!(hook[1].argv, ["true"]);
    assert_eq!(
        hook[1].timeout_seconds, 5,
        "the long form sets its own bound"
    );

    // A file with no hook, and a stop file that is a plain path, still parse as before.
    let plain = parse(&flow(
        STOP_PLAIN,
        r#"        park_on:
          - file: "{run.dir}/out/{unit.id}/gaps.json"
            reason: "gaps""#,
    ))
    .unwrap();
    let stage = &plain.track("item").unwrap().stages[0];
    assert!(stage.stop_on_answer.is_empty());
    assert!(stage.park_on[0].on_answer.is_empty());
    // An empty list declares no hook.
    let empty = parse(&flow(
        "        stop_file:\n          file: \"{run.dir}/stop\"\n          on_answer: []",
        "",
    ))
    .unwrap();
    assert!(
        empty.track("item").unwrap().stages[0]
            .stop_on_answer
            .is_empty()
    );
}

#[test]
fn on_answer_is_refused_anywhere_else() {
    let hook = "on_answer:\n  - [true]";
    let at = |indent: usize, extra: &str| {
        let pad = " ".repeat(indent);
        extra
            .lines()
            .map(|l| format!("{pad}{l}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // On the stage itself.
    let stage = flow(&format!("{STOP_PLAIN}\n{}", at(8, hook)), "");
    let err = refused(&stage);
    assert!(
        err.contains("unknown field `on_answer`") && err.contains("a stage"),
        "{err}"
    );
    // In a done condition.
    let done = flow(STOP_PLAIN, "").replace(
        "- file: \"{run.dir}/out/{unit.id}/report.md\"",
        "- file: \"{run.dir}/out/{unit.id}/report.md\"\n            on_answer: [[true]]",
    );
    let err = refused(&done);
    assert!(err.contains("on_answer"), "{err}");
    // In a command of `before`.
    let before = flow(STOP_PLAIN, "").replace(
        "        stall_minutes: 15",
        "        before:\n          - argv: [true]\n            on_answer: [[true]]\n        stall_minutes: 15",
    );
    let err = refused(&before);
    assert!(err.contains("unknown field `on_answer`"), "{err}");
    // On a track, in the merge definition and at the top.
    let top =
        flow(STOP_PLAIN, "").replace("name: demo-flow", "name: demo-flow\non_answer: [[true]]");
    assert!(refused(&top).contains("unknown field `on_answer`"));
    let track = flow(STOP_PLAIN, "").replace(
        "  item:\n    stages:",
        "  item:\n    on_answer: [[true]]\n    stages:",
    );
    assert!(refused(&track).contains("unknown field `on_answer`"));
    // A `park_on` entry takes a list of commands, not a command.
    let not_list = flow(
        STOP_PLAIN,
        "        park_on:\n          - file: \"{run.dir}/gaps.json\"\n            reason: r\n            on_answer: \"true\"",
    );
    let err = refused(&not_list);
    assert!(err.contains("`on_answer` must be a list"), "{err}");
    // A command is an argv list, never a shell string.
    let shell = flow(
        STOP_PLAIN,
        "        park_on:\n          - file: \"{run.dir}/gaps.json\"\n            reason: r\n            on_answer:\n              - \"sh -c true\"",
    );
    let err = refused(&shell);
    assert!(err.contains("a shell string is not accepted"), "{err}");
}

#[test]
fn an_unknown_placeholder_is_refused_at_load() {
    let with = |arg: &str| {
        flow(
            &format!(
                "        stop_file:\n          file: \"{{run.dir}}/stop\"\n          on_answer:\n            - [echo, \"{arg}\"]"
            ),
            "",
        )
    };
    for bad in [
        "{answer.text}",
        "{answer.fil}",
        "{request.body}",
        "{request.id}",
        "{run.packages}",
        "{param.nope}",
        "{unit.vars.nope}",
    ] {
        let err = refused(&with(bad));
        assert!(
            err.contains(bad.trim_matches(|c| c == '{' || c == '}')),
            "{bad}: {err}"
        );
    }
    // Every name of the closed set is accepted in an answer hook.
    for good in [
        "{answer.file}",
        "{answer.request}",
        "{answer.revision}",
        "{answer.generation}",
        "{request.body_file}",
        "{run.package}/scripts/x.sh",
        "{run.dir}",
        "{run.id}",
        "{unit.id}",
        "{unit.vars.handoff_path}",
        "{param.repo}",
    ] {
        parse(&with(good)).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
}

#[test]
fn the_answer_placeholders_are_refused_outside_an_answer_hook() {
    let cases = [
        // In the stage's own templates.
        flow(STOP_PLAIN, "").replace("message: \"Build", "message: \"{answer.file} Build"),
        flow(STOP_PLAIN, "").replace("cwd: \"{param.repo}\"", "cwd: \"{request.body_file}\""),
        // In a command of `before`.
        flow(STOP_PLAIN, "").replace(
            "        stall_minutes: 15",
            "        before:\n          - [echo, \"{answer.request}\"]\n        stall_minutes: 15",
        ),
        // In the reason and the file of a `park_on` entry.
        flow(
            STOP_PLAIN,
            "        park_on:\n          - file: \"{run.dir}/gaps.json\"\n            reason: \"{answer.generation}\"",
        ),
        flow(
            STOP_PLAIN,
            "        park_on:\n          - file: \"{answer.file}\"\n            reason: r",
        ),
        // In the stop file's own path.
        flow(
            "        stop_file:\n          file: \"{answer.file}\"\n          on_answer: []",
            "",
        ),
    ];
    for text in cases {
        let err = refused(&text);
        assert!(
            err.contains("is allowed only in an `on_answer` command"),
            "{err}"
        );
    }
    // `{run.package}` is a run placeholder: a stage may use it where a run exists.
    parse(&flow(STOP_PLAIN, "").replace(
        "        stall_minutes: 15",
        "        before:\n          - [sh, \"{run.package}/scripts/prep.sh\"]\n        stall_minutes: 15",
    ))
    .unwrap();
    // Not in `plan_check`: no run exists there.
    let err = refused(&flow(STOP_PLAIN, "").replace(
        "unit_sources:",
        "plan_check: [echo, \"{run.package}\"]\nunit_sources:",
    ));
    assert!(err.contains("needs a run"), "{err}");
    // Rendering outside a hook says the same.
    let params = BTreeMap::new();
    let vars = BTreeMap::new();
    let ctx = RenderContext {
        run_id: "run-1",
        run_dir: "/run/dir",
        params: &params,
        unit: Some(UnitView {
            id: "u1",
            vars: &vars,
        }),
    };
    let err = render("{answer.file}", &ctx).unwrap_err().to_string();
    assert!(
        err.contains("allowed only in an `on_answer` command"),
        "{err}"
    );
    assert_eq!(
        render("{run.package}/x", &ctx).unwrap().text,
        "/run/dir/package/x"
    );
    // A value that holds a placeholder name is not rendered a second time.
    let vars = BTreeMap::from([("x".to_string(), "{param.repo} {answer.file}".to_string())]);
    let ctx = RenderContext {
        unit: Some(UnitView {
            id: "u1",
            vars: &vars,
        }),
        ..ctx
    };
    assert_eq!(
        render("{unit.vars.x}", &ctx).unwrap().text,
        "{param.repo} {answer.file}"
    );
}

// ------------------------------------------------------------------ a package with scripts

fn package(dir: &Path, workflow: &str, scripts: &[(&str, &str)]) {
    std::fs::create_dir_all(dir.join("briefs")).unwrap();
    std::fs::create_dir_all(dir.join("rules")).unwrap();
    std::fs::write(dir.join("workflow.yaml"), workflow).unwrap();
    std::fs::write(dir.join("briefs/item.md"), "# item\n").unwrap();
    std::fs::write(
        dir.join("rules/worker.yaml"),
        "version: 1\nrole: worker\nallow:\n  - delete_own_scratch\n",
    )
    .unwrap();
    for (name, text) in scripts {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts").join(name), text).unwrap();
    }
}

#[test]
fn the_scripts_directory_is_pinned_like_the_briefs_and_a_hook_may_run_only_pinned_files() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("pkg");
    package(
        &dir,
        &flow(STOP_HOOK, PARK_HOOK),
        &[("rule.sh", "exit 0\n"), ("clarify.sh", "exit 0\n")],
    );
    let loaded = Package::load(&dir).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        loaded.scripts,
        [
            PathBuf::from("scripts/clarify.sh"),
            PathBuf::from("scripts/rule.sh")
        ]
    );
    let pinned = loaded.pinned_files();
    for file in [
        "scripts/clarify.sh",
        "scripts/rule.sh",
        "workflow.yaml",
        "briefs/item.md",
    ] {
        assert!(
            pinned.contains(&PathBuf::from(file)),
            "{file} in {pinned:?}"
        );
    }
    let pin = pf_workflow::pin::pin_package(&loaded).unwrap();
    assert!(pin.files.contains_key("scripts/rule.sh"));
    // A change to a script is a change to the pin.
    std::fs::write(dir.join("scripts/rule.sh"), "exit 1\n").unwrap();
    let mismatch = pf_workflow::pin::check(&pin).unwrap_err().to_string();
    assert!(mismatch.contains("scripts/rule.sh"), "{mismatch}");

    // A hook that names a script the package lacks is refused at load.
    let lacking = tempfile::tempdir().unwrap();
    package(
        &lacking.path().join("pkg"),
        &flow(STOP_HOOK, PARK_HOOK),
        &[("clarify.sh", "exit 0\n")],
    );
    let err = Package::load(&lacking.path().join("pkg"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("scripts/rule.sh") && err.contains("does not pin"),
        "{err}"
    );
    // A package with no scripts directory is a package with no scripts.
    let none = tempfile::tempdir().unwrap();
    package(&none.path().join("pkg"), &flow(STOP_PLAIN, ""), &[]);
    assert!(
        Package::load(&none.path().join("pkg"))
            .unwrap()
            .scripts
            .is_empty()
    );
    // A link in the scripts directory is not a plain file.
    let linked = tempfile::tempdir().unwrap();
    package(
        &linked.path().join("pkg"),
        &flow(STOP_PLAIN, ""),
        &[("a.sh", "exit 0\n")],
    );
    std::os::unix::fs::symlink("/etc/passwd", linked.path().join("pkg/scripts/b.sh")).unwrap();
    let err = Package::load(&linked.path().join("pkg"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("scripts/b.sh") && err.contains("plain file"),
        "{err}"
    );
}

// ------------------------------------------------------------------ running the hook

const HANDOFF_VAR: &str = "path with space 'q' \"dq\"\nand a newline";

/// An engine with one unit `u1` parked at `implement` by a request of `kind`. Its session has
/// ended, as it has when the driver parks the unit. The record holds an awkward variable.
fn parked(kind: RequestKind) -> (Env, RequestRow, u64) {
    let e = env(item_workflow(&["implement", "verify"], false));
    let mut record = unit("u1", "g1", 1);
    record.vars = BTreeMap::from([("handoff_path".to_string(), HANDOFF_VAR.to_string())]);
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![record],
        })
        .unwrap();
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    e.engine
        .park_with(
            "u1",
            kind,
            "needs an answer",
            Some(&serde_json::json!({ "gaps": [{"section": "s", "question": "q", "severity": "blocking"}] })),
        )
        .unwrap();
    e.engine
        .end("u1/implement#1", EndStatus::Failed, "ended")
        .unwrap();
    let generation = e
        .engine
        .claim("op@host", &sha256_hex(b"t1"))
        .unwrap()
        .generation;
    let request = e.engine.requests().unwrap().remove(0);
    (e, request, generation)
}

fn hook(commands: &[&[&str]]) -> Vec<CommandSpec> {
    commands.iter().map(|argv| CommandSpec::new(argv)).collect()
}

struct Rendering {
    params: BTreeMap<String, String>,
    vars: BTreeMap<String, String>,
}

impl Rendering {
    fn new() -> Rendering {
        Rendering {
            params: BTreeMap::from([(
                "quoted".to_string(),
                "it's \"two\" words;\n$(not run)".to_string(),
            )]),
            vars: BTreeMap::from([("handoff_path".to_string(), HANDOFF_VAR.to_string())]),
        }
    }

    fn ctx(&self) -> RenderContext<'_> {
        RenderContext {
            run_id: "run-1",
            run_dir: "/run/dir",
            params: &self.params,
            unit: Some(UnitView {
                id: "u1",
                vars: &self.vars,
            }),
        }
    }
}

fn run_hook(
    e: &Env,
    request: &RequestRow,
    generation: u64,
    text: &str,
    specs: &[CommandSpec],
    files_dir: &Path,
) -> Result<AnswerOutcome, pf_engine::EngineError> {
    let r = Rendering::new();
    answer_through_hook(
        &e.engine,
        request,
        request.revision,
        generation,
        text,
        specs,
        &r.ctx(),
        files_dir,
    )
}

fn requests(e: &Env) -> Vec<RequestRow> {
    e.engine.requests().unwrap()
}

fn ledger_lines(e: &Env, needle: &str) -> Vec<String> {
    e.ledger_text()
        .lines()
        .filter(|l| l.contains(needle))
        .map(str::to_string)
        .collect()
}

#[test]
fn each_placeholder_reaches_the_argv_as_one_word() {
    let (e, request, generation) = parked(RequestKind::Gaps);
    let work = tempfile::tempdir().unwrap();
    // The directory of the answer files holds a space and a quote.
    let files_dir = work.path().join("answers dir 'x'");
    let record = work.path().join("record");
    let script = work.path().join("record.sh");
    // The script writes each argument it got, NUL-separated, and what it finds in the two files.
    std::fs::write(
        &script,
        r#"record=$1; shift
printf '%s\0' "$@" > "$record"
printf '%s' "$(stat -c %a "$2")" > "$record.mode.answer"
printf '%s' "$(stat -c %a "$(dirname "$2")")" > "$record.mode.dir"
cat "$2" > "$record.answer"
cat "$6" > "$record.body"
"#,
    )
    .unwrap();
    let text = "go with the first branch\nit's \"fine\"; $(touch pwned)\n{param.quoted}";
    let specs = hook(&[&[
        "sh",
        script.to_str().unwrap(),
        record.to_str().unwrap(),
        "{unit.vars.handoff_path}",
        "{answer.file}",
        "{answer.request}",
        "{answer.revision}",
        "{answer.generation}",
        "{request.body_file}",
        "pre-{param.quoted}-post",
        "{run.package}/scripts/x.sh",
    ]]);

    let outcome = run_hook(&e, &request, generation, text, &specs, &files_dir).unwrap();
    assert_eq!(outcome, AnswerOutcome::Resumed);

    let raw = std::fs::read(&record).unwrap();
    let words: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|w| String::from_utf8(w.to_vec()).unwrap())
        .collect();
    // Eight words, and an empty piece after the last NUL.
    assert_eq!(words.len(), 9, "{words:?}");
    assert_eq!(
        words[0], HANDOFF_VAR,
        "a space, a quote and a newline: one word"
    );
    let answer_file = &words[1];
    assert!(
        answer_file.starts_with(files_dir.to_str().unwrap()) && answer_file.contains(" dir 'x'"),
        "{answer_file}"
    );
    assert_eq!(words[2], "R-1");
    assert_eq!(words[3], "1");
    assert_eq!(words[4], generation.to_string());
    let body_file = &words[5];
    assert_eq!(words[6], "pre-it's \"two\" words;\n$(not run)-post");
    assert_eq!(words[7], "/run/dir/package/scripts/x.sh");
    assert_eq!(words[8], "");

    // The files held the text and the body, and were private while the hook ran.
    assert_eq!(
        std::fs::read_to_string(format!("{}.answer", record.display())).unwrap(),
        text
    );
    assert_eq!(
        std::fs::read_to_string(format!("{}.body", record.display())).unwrap(),
        request.body
    );
    assert_eq!(
        std::fs::read_to_string(format!("{}.mode.answer", record.display())).unwrap(),
        "600"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{}.mode.dir", record.display())).unwrap(),
        "700"
    );
    // Nothing ran the text, and the controller removed its files after the hook.
    assert!(!Path::new("pwned").exists());
    assert!(!Path::new(answer_file).exists() && !Path::new(body_file).exists());
    assert!(!files_dir.join("R-1").exists());
}

#[test]
fn a_hook_that_exits_0_resumes_the_unit_and_writes_its_ledger_line() {
    let (e, request, generation) = parked(RequestKind::Gaps);
    let work = tempfile::tempdir().unwrap();
    let mark = work.path().join("ran");
    let specs = hook(&[
        &[
            "sh",
            "-c",
            "echo one >> \"$1\"",
            "x",
            mark.to_str().unwrap(),
        ],
        &[
            "sh",
            "-c",
            "echo two >> \"$1\"",
            "x",
            mark.to_str().unwrap(),
        ],
    ]);
    let outcome = run_hook(&e, &request, generation, "go on", &specs, work.path()).unwrap();
    assert_eq!(outcome, AnswerOutcome::Resumed);
    assert_eq!(
        std::fs::read_to_string(&mark).unwrap(),
        "one\ntwo\n",
        "in order"
    );
    assert_eq!(e.status_of("u1"), (Status::Running, None));
    let all = requests(&e);
    assert_eq!(all.len(), 1, "no new request");
    assert_eq!(all[0].status, RequestStatus::Answered);
    assert_eq!(all[0].answer_generation, Some(generation));
    let lines = ledger_lines(&e, "| answer hook R-");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with("| answer hook R-1: rc 0"),
        "{}",
        lines[0]
    );
    // The hook's line comes before the answer's.
    let ledger = e.ledger_text();
    let hook_at = ledger.find("answer hook R-1").unwrap();
    let answer_at = ledger.find("answer R-1@1 by claim 1").unwrap();
    assert!(hook_at < answer_at, "{ledger}");
}

#[test]
fn a_failing_hook_leaves_the_request_answered_and_parks_the_unit_with_a_new_request() {
    let (e, request, generation) = parked(RequestKind::Gaps);
    let work = tempfile::tempdir().unwrap();
    let mark = work.path().join("second");
    let specs = hook(&[
        &["sh", "-c", "echo 'cannot write the handoff' >&2; exit 3"],
        // A second command does not run after a failure.
        &["touch", mark.to_str().unwrap()],
    ]);
    let outcome = run_hook(
        &e,
        &request,
        generation,
        "use the first",
        &specs,
        work.path(),
    )
    .unwrap();
    let AnswerOutcome::HookFailed(cause) = outcome else {
        panic!("the hook failed: {outcome:?}");
    };
    assert!(
        cause.contains("on_answer command 1 (sh)")
            && cause.contains("exited with code 3: cannot write the handoff"),
        "{cause}"
    );
    assert!(!mark.exists(), "the hook stops at its first failure");

    let all = requests(&e);
    assert_eq!(all.len(), 2, "{all:?}");
    // The request stays answered, with the text and the claim.
    assert_eq!(all[0].status, RequestStatus::Answered);
    assert_eq!(all[0].answer_generation, Some(generation));
    assert!(all[0].answer.as_deref().unwrap().contains("use the first"));
    // The unit parked again, with the cause, and a new request is open.
    let (status, reason) = e.status_of("u1");
    assert_eq!(status, Status::Parked);
    let reason = reason.unwrap();
    assert!(reason.starts_with("answer hook failed: "), "{reason}");
    assert!(reason.contains("cannot write the handoff"), "{reason}");
    assert_eq!(all[1].status, RequestStatus::Open);
    assert_eq!(all[1].kind, RequestKind::Park);
    assert_eq!(all[1].reason, reason);
    assert_eq!(all[1].unit_id.as_deref(), Some("u1"));
    // The ledger line carries the exit code.
    let lines = ledger_lines(&e, "| answer hook R-");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with("| answer hook R-1: rc 3: cannot write the handoff"),
        "{}",
        lines[0]
    );
    // The unit never ran: its next request is answered like any other.
    e.engine
        .answer_request("R-2", 1, generation, "fixed")
        .unwrap();
    assert_eq!(e.status_of("u1").0, Status::Running);
}

#[test]
fn a_hook_that_cannot_start_or_cannot_render_fails_the_same_way() {
    for (argv, cause) in [
        (
            vec!["no-such-program-anywhere", "{answer.file}"],
            "could not start",
        ),
        (vec!["echo", "{unit.vars.missing}"], "cannot render"),
        (vec!["echo", "{param.absent}"], "cannot render"),
    ] {
        let (e, request, generation) = parked(RequestKind::Gaps);
        let work = tempfile::tempdir().unwrap();
        let specs = vec![CommandSpec::new(&argv)];
        let outcome = run_hook(&e, &request, generation, "x", &specs, work.path()).unwrap();
        let AnswerOutcome::HookFailed(why) = outcome else {
            panic!("{argv:?} should fail");
        };
        assert!(why.contains(cause), "{why}");
        assert_eq!(e.status_of("u1").0, Status::Parked, "{argv:?}");
        let reason = e.status_of("u1").1.unwrap();
        assert!(reason.starts_with("answer hook failed: "), "{reason}");
        assert_eq!(requests(&e).len(), 2);
        let lines = ledger_lines(&e, "| answer hook R-");
        assert!(lines[0].contains("answer hook R-1: rc none"), "{lines:?}");
    }
}

#[test]
fn a_hook_past_the_command_time_bound_fails_the_same_way() {
    let (e, request, generation) = parked(RequestKind::Gaps);
    let work = tempfile::tempdir().unwrap();
    let specs = vec![CommandSpec {
        timeout_seconds: 1,
        ..CommandSpec::new(&["sleep", "30"])
    }];
    let started = std::time::Instant::now();
    let outcome = run_hook(&e, &request, generation, "x", &specs, work.path()).unwrap();
    assert!(
        started.elapsed().as_secs() < 20,
        "the runner killed the process"
    );
    let AnswerOutcome::HookFailed(cause) = outcome else {
        panic!("a hook past its bound fails");
    };
    assert!(
        cause.contains("on_answer command 1 (sleep)") && cause.contains("timed out after 1s"),
        "{cause}"
    );
    assert_eq!(e.status_of("u1").0, Status::Parked);
    assert!(
        e.status_of("u1")
            .1
            .unwrap()
            .starts_with("answer hook failed: ")
    );
    assert_eq!(requests(&e)[0].status, RequestStatus::Answered);
    assert_eq!(requests(&e)[1].status, RequestStatus::Open);
    let lines = ledger_lines(&e, "| answer hook R-");
    assert!(
        lines[0].ends_with("| answer hook R-1: rc none: timed out after 1s and was killed"),
        "{}",
        lines[0]
    );
}

#[test]
fn a_refused_answer_runs_no_hook() {
    let (e, request, generation) = parked(RequestKind::Gaps);
    let work = tempfile::tempdir().unwrap();
    let mark = work.path().join("ran");
    let specs = hook(&[&["touch", mark.to_str().unwrap()]]);

    // A fenced claim.
    let newer = e
        .engine
        .claim("other@host", &sha256_hex(b"t2"))
        .unwrap()
        .generation;
    let err = run_hook(&e, &request, generation, "x", &specs, work.path())
        .unwrap_err()
        .to_string();
    assert!(err.contains("fenced"), "{err}");
    // A stale revision.
    let mut stale = request.clone();
    stale.revision = 7;
    let err = run_hook(&e, &stale, newer, "x", &specs, work.path())
        .unwrap_err()
        .to_string();
    assert!(err.contains("revision 1"), "{err}");
    // An empty text.
    let err = run_hook(&e, &request, newer, "   ", &specs, work.path())
        .unwrap_err()
        .to_string();
    assert!(err.contains("needs text"), "{err}");

    assert!(!mark.exists(), "no hook ran");
    assert!(ledger_lines(&e, "| answer hook R-").is_empty());
    assert_eq!(e.status_of("u1").0, Status::Parked);
    assert_eq!(requests(&e).len(), 1);
    assert_eq!(requests(&e)[0].status, RequestStatus::Open);
    assert!(!work.path().join("R-1").exists(), "no answer files either");

    // The same answer in the current claim runs the hook once.
    run_hook(&e, &request, newer, "x", &specs, work.path()).unwrap();
    assert!(mark.exists());
}

#[test]
fn clear_runs_no_hook() {
    let (e, request, generation) = parked(RequestKind::Gaps);
    // The hook exists only in the workflow; the engine's clear does not read a hook, and the
    // only path that runs one is `answer_through_hook`.
    let work = tempfile::tempdir().unwrap();
    let mark = work.path().join("ran");
    let _specs = hook(&[&["touch", mark.to_str().unwrap()]]);
    e.engine
        .clear_unit("u1", None, generation, &request.id)
        .unwrap();
    assert_eq!(e.status_of("u1").0, Status::Running);
    assert!(!mark.exists(), "a clear runs no hook");
    assert!(ledger_lines(&e, "| answer hook R-").is_empty());
    let all = requests(&e);
    assert_eq!(all.len(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(all[0].answer.as_deref().unwrap()).unwrap(),
        serde_json::json!({"verb": "clear", "to_stage": "implement"})
    );
}

#[test]
fn a_second_run_for_the_same_request_is_a_second_ledger_line() {
    // A hook that ran and then lost the race for the answer ran once: its line stays, and a
    // retry writes its own line instead of conflicting with the first.
    let (e, request, generation) = parked(RequestKind::Gaps);
    e.engine
        .record_answer_hook("R-1", 1, Some(1), "first")
        .unwrap();
    let work = tempfile::tempdir().unwrap();
    run_hook(
        &e,
        &request,
        generation,
        "x",
        &hook(&[&["true"]]),
        work.path(),
    )
    .unwrap();
    let lines = ledger_lines(&e, "| answer hook R-");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].ends_with("rc 1: first"), "{}", lines[0]);
    assert!(lines[1].ends_with("rc 0"), "{}", lines[1]);
}

#[test]
fn the_answer_files_are_private_even_in_a_directory_made_for_them() {
    // The directory of the files is made on the way, as the controller makes `answers/`.
    let (e, request, generation) = parked(RequestKind::Gaps);
    let work = tempfile::tempdir().unwrap();
    let probe = work.path().join("perm");
    let specs = hook(&[&[
        "sh",
        "-c",
        "stat -c %a \"$1\" \"$(dirname \"$1\")\" > \"$2\"",
        "x",
        "{answer.file}",
        probe.to_str().unwrap(),
    ]]);
    run_hook(
        &e,
        &request,
        generation,
        "x",
        &specs,
        &work.path().join("a"),
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&probe).unwrap(), "600\n700\n");
}
