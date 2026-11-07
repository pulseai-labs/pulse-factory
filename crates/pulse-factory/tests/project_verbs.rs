//! AC-4 of `r1.s2.w1`: `project add`, `project list` and `project remove` through the socket. The
//! controller runs in the test process on a `FakeAdapter`, and the CLI runs as a child process
//! against its socket. Nothing spends money or reaches the network.

mod common;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::ctl::*;
use common::*;
use pf_engine::ledger::format_line;
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry, NEW_ID,
    NewAdmission, NewProject, Store,
};
use pulse_factory::controller::{Config, Controller};
use serde_json::{Value, json};

/// A directory under the sandbox, made now.
fn dir(live: &Live, name: &str) -> PathBuf {
    let d = live.sb.root.path().join("work").join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(&d).unwrap()
}

/// A git repo under the sandbox.
fn repo(live: &Live, name: &str) -> PathBuf {
    let d = dir(live, name);
    git(&d, &["init", "-q", "-b", "main"]);
    d
}

fn run(live: &Live, args: &[&str]) -> Output {
    live.cli(args)
}

fn ok_json(out: &Output) -> Value {
    assert_eq!(code(out), 0, "stderr: {}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", stdout(out)))
}

/// The refusal: exit 1, nothing on stdout, and the reason on stderr.
fn refused(out: &Output) -> String {
    assert_eq!(
        code(out),
        1,
        "stdout: {} stderr: {}",
        stdout(out),
        stderr(out)
    );
    assert_eq!(stdout(out), "", "a refusal prints nothing on stdout");
    stderr(out)
}

fn listed(live: &Live) -> Vec<(String, String, String)> {
    let v = ok_json(&run(live, &["project", "list", "--json"]));
    v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_string(),
                p["name"].as_str().unwrap().to_string(),
                p["root"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn ledger_path(live: &Live) -> PathBuf {
    live.sb.state.join("controller.ledger")
}

fn ledger_lines(live: &Live) -> Vec<String> {
    std::fs::read_to_string(ledger_path(live))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn path_str(p: &Path) -> String {
    p.display().to_string()
}

#[test]
fn add_on_a_git_repo_and_on_a_plain_folder_prints_the_text_form() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let f = dir(&live, "plain");

    let out = run(&live, &["project", "add", &path_str(&r)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("project P-1 added: gitty ({})\n", r.display())
    );

    let out = run(
        &live,
        &["project", "add", &path_str(&f), "--name", "folder"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("project P-2 added: folder ({})\n", f.display())
    );
}

#[test]
fn add_json_holds_the_version_the_replay_flag_and_the_project() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let v = ok_json(&run(&live, &["project", "add", &path_str(&r), "--json"]));
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["replayed"], false);
    let p = &v["project"];
    assert_eq!(p["id"], "P-1");
    assert_eq!(p["name"], "gitty");
    assert_eq!(p["root"], path_str(&r));
    assert!(p["created_at"].as_str().is_some_and(|t| !t.is_empty()));
    assert_eq!(p["removed_at"], Value::Null);
    let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, ["created_at", "id", "name", "removed_at", "root"]);
}

#[test]
fn list_shows_the_projects_in_order_and_remove_takes_one_off_the_list() {
    let live = Live::start();
    let out = run(&live, &["project", "list"]);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout(&out), "no projects\n");
    let v = ok_json(&run(&live, &["project", "list", "--json"]));
    assert_eq!(v, json!({ "schema_version": 1, "projects": [] }));

    let r = repo(&live, "gitty");
    let f = dir(&live, "plain");
    run(&live, &["project", "add", &path_str(&r)]);
    run(&live, &["project", "add", &path_str(&f)]);

    assert_eq!(
        listed(&live),
        vec![
            ("P-1".to_string(), "gitty".to_string(), path_str(&r)),
            ("P-2".to_string(), "plain".to_string(), path_str(&f)),
        ]
    );
    let out = run(&live, &["project", "list"]);
    assert_eq!(
        stdout(&out),
        format!("P-1  gitty  {}\nP-2  plain  {}\n", r.display(), f.display())
    );

    let out = run(&live, &["project", "remove", "P-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("project P-1 removed: gitty ({})\n", r.display())
    );
    assert_eq!(
        listed(&live),
        vec![("P-2".to_string(), "plain".to_string(), path_str(&f))]
    );

    // The JSON of a remove holds the removed row.
    let v = ok_json(&run(&live, &["project", "remove", "P-2", "--json"]));
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["replayed"], false);
    assert_eq!(v["project"]["id"], "P-2");
    assert!(v["project"]["removed_at"].as_str().is_some());
    assert!(listed(&live).is_empty());
}

#[test]
fn a_removed_root_can_be_added_again_with_a_new_id() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    run(&live, &["project", "add", &path_str(&r)]);
    run(&live, &["project", "remove", "P-1"]);
    let v = ok_json(&run(&live, &["project", "add", &path_str(&r), "--json"]));
    assert_eq!(v["project"]["id"], "P-2");
    assert_eq!(listed(&live).len(), 1);
}

#[test]
fn the_name_defaults_to_the_last_component_and_a_flag_sets_it() {
    let live = Live::start();
    let a = dir(&live, "deep/er/leaf");
    let b = dir(&live, "other");
    let v = ok_json(&run(&live, &["project", "add", &path_str(&a), "--json"]));
    assert_eq!(v["project"]["name"], "leaf");
    let v = ok_json(&run(
        &live,
        &[
            "project",
            "add",
            &path_str(&b),
            "--name",
            "Named Thing",
            "--json",
        ],
    ));
    assert_eq!(v["project"]["name"], "Named Thing");
}

#[test]
fn a_relative_root_resolves_against_the_cli_directory() {
    let live = Live::start();
    let parent = dir(&live, "parent");
    let child = dir(&live, "parent/child");
    let sibling = dir(&live, "parent/sibling");

    let mut cmd = live.sb.cmd(&["project", "add", "child", "--json"]);
    let out = cmd.current_dir(&parent).output().unwrap();
    assert_eq!(ok_json(&out)["project"]["root"], path_str(&child));

    // A `..` spelling is made canonical: it names the sibling.
    let mut cmd = live.sb.cmd(&["project", "add", "../sibling", "--json"]);
    let out = cmd.current_dir(&child).output().unwrap();
    assert_eq!(ok_json(&out)["project"]["root"], path_str(&sibling));

    // The controller's own directory is not the CLI's: the same word from another directory is
    // another root.
    let mut cmd = live.sb.cmd(&["project", "add", "child"]);
    let err = refused(&cmd.current_dir(&sibling).output().unwrap());
    assert!(
        err.contains("not exist") || err.contains("directory"),
        "{err}"
    );
}

#[test]
fn the_refusals_each_exit_1_with_the_reason_and_change_nothing() {
    let live = Live::start();
    let kept = repo(&live, "kept");
    run(&live, &["project", "add", &path_str(&kept)]);
    let other = dir(&live, "other");
    let file = live.sb.root.path().join("work/a-file");
    std::fs::write(&file, "x").unwrap();
    let link = live.sb.root.path().join("work/link-to-kept");
    symlink(&kept, &link).unwrap();
    let missing = live.sb.root.path().join("work/missing");

    let cases: Vec<(Vec<String>, &str)> = vec![
        (vec!["add".into(), path_str(&missing)], "does not exist"),
        (vec!["add".into(), path_str(&file)], "not a directory"),
        (vec!["add".into(), "/".into()], "file system root"),
        (vec!["add".into(), path_str(&link)], "root"),
        (
            vec![
                "add".into(),
                path_str(&other),
                "--name".into(),
                "kept".into(),
            ],
            "name",
        ),
        (
            vec![
                "add".into(),
                path_str(&other),
                "--name".into(),
                "a\nb".into(),
            ],
            "name",
        ),
        (
            vec![
                "add".into(),
                path_str(&other),
                "--name".into(),
                "x".repeat(65),
            ],
            "name",
        ),
        (
            vec![
                "add".into(),
                path_str(&other),
                "--name".into(),
                String::new(),
            ],
            "name",
        ),
        (
            vec![
                "add".into(),
                path_str(&other),
                "--name".into(),
                "tab\there".into(),
            ],
            "name",
        ),
        (vec!["remove".into(), "P-9".into()], "P-9"),
    ];
    let mut refusals = 0;
    for (args, needle) in &cases {
        let mut full: Vec<String> = vec!["project".into()];
        full.extend(args.iter().cloned());
        let out = live.sb.run(&full);
        let err = refused(&out);
        assert!(err.contains(needle), "{args:?} -> {err}");
        refusals += 1;
        // Nothing changed.
        assert_eq!(listed(&live).len(), 1, "{args:?}");
    }
    // The symlink to a registered root is refused as that root.
    let err = refused(&run(&live, &["project", "add", &path_str(&link)]));
    assert!(err.contains(&path_str(&kept)), "{err}");

    // Each refusal is one admission with its own ledger line, after the first add's line.
    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), 1 + refusals + 1, "{lines:#?}");
    for l in &lines[1..] {
        assert!(l.contains(" refused reason="), "{l}");
        assert!(l.contains("command="), "{l}");
    }
}

#[test]
fn a_remove_of_a_removed_id_is_a_refusal() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    run(&live, &["project", "add", &path_str(&r)]);
    run(&live, &["project", "remove", "P-1"]);
    let err = refused(&run(&live, &["project", "remove", "P-1"]));
    assert!(err.contains("P-1") && err.contains("removed"), "{err}");
}

#[test]
fn a_name_of_64_characters_is_accepted_by_characters_not_bytes() {
    let live = Live::start();
    let a = dir(&live, "a");
    let name = "é".repeat(64);
    assert!(name.len() > 64);
    let v = ok_json(&run(
        &live,
        &["project", "add", &path_str(&a), "--name", &name, "--json"],
    ));
    assert_eq!(v["project"]["name"], name);
}

#[test]
fn the_name_check_is_exact_and_not_case_folded() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    run(&live, &["project", "add", &path_str(&a), "--name", "Alpha"]);
    let out = run(&live, &["project", "add", &path_str(&b), "--name", "alpha"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

#[test]
fn add_and_remove_refuse_under_a_seat_token_and_list_still_works() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    run(&live, &["project", "add", &path_str(&a)]);
    let ledger_before = ledger_lines(&live);

    let seat = |args: &[&str]| {
        live.sb
            .cmd(args)
            .env("PF_SEAT_TOKEN", "a-seat")
            .output()
            .unwrap()
    };
    let err = refused(&seat(&["project", "add", &path_str(&b)]));
    assert!(err.contains("PF_SEAT_TOKEN"), "{err}");
    let err = refused(&seat(&["project", "remove", "P-1"]));
    assert!(err.contains("PF_SEAT_TOKEN"), "{err}");

    // `list` is a read and stays open.
    let out = seat(&["project", "list", "--json"]);
    let v = ok_json(&out);
    assert_eq!(v["projects"].as_array().unwrap().len(), 1);

    // Nothing changed: the list, the admissions and the ledger.
    assert_eq!(listed(&live).len(), 1);
    assert_eq!(ledger_lines(&live), ledger_before);
    assert_eq!(
        live.controller
            .store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_same_command_id_with_the_same_arguments_is_a_replay() {
    let live = Live::start();
    let a = dir(&live, "a");
    let first = ok_json(&run(
        &live,
        &[
            "project",
            "add",
            &path_str(&a),
            "--command-id",
            "cmd-1",
            "--json",
        ],
    ));
    assert_eq!(first["replayed"], false);
    let again = ok_json(&run(
        &live,
        &[
            "project",
            "add",
            &path_str(&a),
            "--command-id",
            "cmd-1",
            "--json",
        ],
    ));
    assert_eq!(again["replayed"], true);
    assert_eq!(again["project"], first["project"]);
    assert_eq!(listed(&live).len(), 1);

    // After the project is removed, the replay still returns the first record's project.
    run(&live, &["project", "remove", "P-1", "--command-id", "rm-1"]);
    let later = ok_json(&run(
        &live,
        &[
            "project",
            "add",
            &path_str(&a),
            "--command-id",
            "cmd-1",
            "--json",
        ],
    ));
    assert_eq!(later["replayed"], true);
    assert_eq!(later["project"]["id"], "P-1");
    assert!(later["project"]["removed_at"].as_str().is_some());
    assert!(listed(&live).is_empty(), "a replay adds nothing");

    // A remove replays the same way.
    let rm = ok_json(&run(
        &live,
        &["project", "remove", "P-1", "--command-id", "rm-1", "--json"],
    ));
    assert_eq!(rm["replayed"], true);
    assert_eq!(rm["project"]["id"], "P-1");
}

#[test]
fn the_same_command_id_with_other_arguments_is_a_conflict_that_names_the_id() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    run(
        &live,
        &["project", "add", &path_str(&a), "--command-id", "cmd-1"],
    );
    let err = refused(&run(
        &live,
        &["project", "add", &path_str(&b), "--command-id", "cmd-1"],
    ));
    assert!(err.contains("cmd-1"), "{err}");
    // A remove with an ID that an add used is a conflict as well.
    let err = refused(&run(
        &live,
        &["project", "remove", "P-1", "--command-id", "cmd-1"],
    ));
    assert!(err.contains("cmd-1"), "{err}");
    assert_eq!(listed(&live).len(), 1);
    assert_eq!(ledger_lines(&live).len(), 1, "a conflict writes no line");
}

#[test]
fn a_replayed_refusal_is_the_same_refusal() {
    let live = Live::start();
    let missing = live.sb.root.path().join("work/missing");
    let first = refused(&run(
        &live,
        &[
            "project",
            "add",
            &path_str(&missing),
            "--command-id",
            "cmd-1",
        ],
    ));
    let again = refused(&run(
        &live,
        &[
            "project",
            "add",
            &path_str(&missing),
            "--command-id",
            "cmd-1",
        ],
    ));
    assert_eq!(first, again);
    assert_eq!(ledger_lines(&live).len(), 1, "no line for a replay");
}

#[test]
fn a_bad_command_id_is_an_error_and_records_nothing() {
    let live = Live::start();
    let a = dir(&live, "a");
    let long = "x".repeat(129);
    for bad in ["", "has space", "new\nline", long.as_str()] {
        let err = refused(&run(
            &live,
            &["project", "add", &path_str(&a), "--command-id", bad],
        ));
        assert!(err.contains("command ID"), "{bad:?} -> {err}");
    }
    assert_eq!(ledger_lines(&live).len(), 0);
    assert!(listed(&live).is_empty());
}

#[test]
fn the_controller_ledger_has_one_line_per_new_command_in_order_and_none_for_a_replay() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    let missing = live.sb.root.path().join("work/missing");

    run(
        &live,
        &["project", "add", &path_str(&a), "--command-id", "id-add-a"],
    );
    run(
        &live,
        &["project", "add", &path_str(&a), "--command-id", "id-add-a"],
    ); // replay
    run(
        &live,
        &[
            "project",
            "add",
            &path_str(&missing),
            "--command-id",
            "id-bad",
        ],
    ); // refused
    run(
        &live,
        &["project", "add", &path_str(&b), "--command-id", "id-add-b"],
    );
    run(
        &live,
        &["project", "remove", "P-1", "--command-id", "id-rm-a"],
    );
    run(
        &live,
        &["project", "remove", "P-7", "--command-id", "id-rm-7"],
    ); // refused
    run(&live, &["project", "list"]); // a read: no line

    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), 5, "{lines:#?}");
    let want = [
        format!(
            "- #1 project_add P-1 admitted name=a root={} command=id-add-a",
            a.display()
        ),
        "- #2 project_add refused reason=\"".to_string(),
        format!(
            "- #3 project_add P-2 admitted name=b root={} command=id-add-b",
            b.display()
        ),
        "- #4 project_remove P-1 admitted command=id-rm-a".to_string(),
        "- #5 project_remove refused reason=\"".to_string(),
    ];
    assert_eq!(lines[0], want[0]);
    assert!(lines[1].starts_with(&want[1]), "{}", lines[1]);
    assert!(lines[1].ends_with(" command=id-bad"), "{}", lines[1]);
    assert_eq!(lines[2], want[2]);
    assert_eq!(lines[3], want[3]);
    assert!(lines[4].starts_with(&want[4]), "{}", lines[4]);
    assert!(lines[4].ends_with(" command=id-rm-7"), "{}", lines[4]);

    // The admissions agree: five rows, in the same order, under the scope `@controller`.
    let admissions = live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap();
    let ids: Vec<&str> = admissions.iter().map(|a| a.command_id.as_str()).collect();
    assert_eq!(
        ids,
        ["id-add-a", "id-bad", "id-add-b", "id-rm-a", "id-rm-7"]
    );
    let outcomes: Vec<AdmissionOutcome> = admissions.iter().map(|a| a.outcome).collect();
    assert_eq!(
        outcomes,
        [
            AdmissionOutcome::Admitted,
            AdmissionOutcome::Refused,
            AdmissionOutcome::Admitted,
            AdmissionOutcome::Admitted,
            AdmissionOutcome::Refused
        ]
    );
    // No project command is a run: no run row holds the scope.
    assert!(
        live.controller
            .store()
            .run(CONTROLLER_SCOPE)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_command_without_an_id_gets_a_random_128_bit_hex_one_for_each_call() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    run(&live, &["project", "add", &path_str(&a)]);
    run(&live, &["project", "add", &path_str(&b)]);
    run(&live, &["project", "remove", "P-1"]);
    let ids: Vec<String> = ledger_lines(&live)
        .iter()
        .map(|l| l.rsplit("command=").next().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3);
    for id in &ids {
        assert_eq!(id.len(), 32, "{id}");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 3, "each call has its own ID");
}

#[test]
fn the_ledger_redacts_a_secret_in_a_path_and_names_no_secret() {
    let live = Live::start();
    let secret = dir(&live, "API_TOKEN=hunter2");
    let out = run(
        &live,
        &["project", "add", &path_str(&secret), "--name", "plain"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = std::fs::read_to_string(ledger_path(&live)).unwrap();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("project_add P-1 admitted"), "{text}");
}

#[test]
fn a_name_that_holds_spaces_keeps_the_ledger_line_readable() {
    let live = Live::start();
    let a = dir(&live, "a");
    run(
        &live,
        &[
            "project",
            "add",
            &path_str(&a),
            "--name",
            "two words",
            "--command-id",
            "c1",
        ],
    );
    let lines = ledger_lines(&live);
    assert_eq!(
        lines[0],
        format!(
            "- #1 project_add P-1 admitted name=\"two words\" root={} command=c1",
            a.display()
        )
    );
}

// ---- restart: the ledger is recovered with the run ledgers' recovery

fn reopen(live: &Live) -> std::sync::Arc<Controller> {
    Controller::open(
        Config::new(live.sb.state.clone(), 3_600_000, BIN.into()),
        live.ports.clone(),
    )
    .unwrap()
}

#[test]
fn a_controller_restart_appends_no_second_copy() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    run(&live, &["project", "add", &path_str(&a)]);
    run(&live, &["project", "add", &path_str(&b)]);
    let before = std::fs::read_to_string(ledger_path(&live)).unwrap();
    assert_eq!(before.lines().count(), 2);

    let restarted = reopen(&live);
    drop(restarted);
    assert_eq!(std::fs::read_to_string(ledger_path(&live)).unwrap(), before);
}

/// Commit a project add through the store port with a ledger entry and do not append it: the
/// state a stop leaves between the commit and the append.
fn commit_without_append(live: &Live, command_id: &str, name: &str, root: &Path) -> String {
    let body = format!(
        "project_add {NEW_ID} admitted name={name} root={} command={command_id}",
        root.display()
    );
    let c = Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: json!({ "name": name, "root": path_str(root) }).to_string(),
        admission: NewAdmission {
            kind: "project_add".to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: Some(NEW_ID.to_string()),
            role: None,
            model: None,
            attempt: None,
            admitted_at: "2026-11-07T00:00:00.000Z".to_string(),
        },
        effects: vec![Effect::AddProject(NewProject {
            name: name.to_string(),
            root: path_str(root),
            created_at: "2026-11-07T00:00:00.000Z".to_string(),
        })],
        ledger: Some(LedgerEntry {
            ledger_file: path_str(&ledger_path(live)),
            body,
        }),
    };
    match live.controller.store().commit(c).unwrap() {
        Committed::Applied { ledger, .. } => {
            let line = ledger.unwrap();
            format_line(line.seq, &line.body)
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_line_committed_but_not_appended_is_appended_once_at_the_next_start() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    run(&live, &["project", "add", &path_str(&a)]);
    let line = commit_without_append(&live, "lost", "b", &b);

    // The line is in the outbox and not in the file.
    assert_eq!(ledger_lines(&live).len(), 1);
    drop(reopen(&live));
    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(format!("{}\n", lines[1]), line);
    assert!(
        lines[1].contains("project_add P-2 admitted"),
        "{}",
        lines[1]
    );

    // A second start adds nothing.
    drop(reopen(&live));
    assert_eq!(ledger_lines(&live), lines);
    // The project is on the list, and a new command continues the numbering.
    assert_eq!(listed(&live).len(), 2);
    let c = dir(&live, "c");
    let v = ok_json(&run(&live, &["project", "add", &path_str(&c), "--json"]));
    assert_eq!(v["project"]["id"], "P-3");
    assert_eq!(ledger_lines(&live).len(), 3);
    assert!(
        ledger_lines(&live)[2].starts_with("- #3 "),
        "{:?}",
        ledger_lines(&live)
    );
}

#[test]
fn a_line_appended_but_not_marked_is_not_appended_twice() {
    let live = Live::start();
    let b = dir(&live, "b");
    let line = commit_without_append(&live, "half", "b", &b);
    // The stop came after the append and before the mark.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ledger_path(&live))
            .unwrap();
        f.write_all(line.as_bytes()).unwrap();
    }
    drop(reopen(&live));
    assert_eq!(ledger_lines(&live).len(), 1);
    drop(reopen(&live));
    assert_eq!(ledger_lines(&live).len(), 1);
}

#[test]
fn a_torn_last_line_is_cut_at_the_next_start() {
    let live = Live::start();
    let a = dir(&live, "a");
    run(&live, &["project", "add", &path_str(&a)]);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(ledger_path(&live))
            .unwrap();
        f.write_all(b"- #2 project_add P-2 adm").unwrap();
    }
    drop(reopen(&live));
    let lines = ledger_lines(&live);
    assert_eq!(lines.len(), 1, "{lines:#?}");
}

#[test]
fn a_controller_ledger_that_cannot_be_written_stops_the_project_commands_and_not_the_reads() {
    let live = Live::start();
    let a = dir(&live, "a");
    let b = dir(&live, "b");
    let c = dir(&live, "c");
    run(&live, &["project", "add", &path_str(&a)]);
    // A directory takes the ledger's place: the append fails.
    std::fs::remove_file(ledger_path(&live)).unwrap();
    std::fs::create_dir(ledger_path(&live)).unwrap();
    let err = refused(&run(&live, &["project", "add", &path_str(&b)]));
    assert!(err.contains("ledger"), "{err}");
    assert!(
        err.contains("committed"),
        "the error says the command stood: {err}"
    );
    // The commit stood, and the reads still work.
    assert_eq!(listed(&live).len(), 2);
    // The commands stop, as a run's admission stops when its ledger fails.
    let err = refused(&run(&live, &["project", "add", &path_str(&c)]));
    assert!(err.contains("stopped"), "{err}");
    let err = refused(&run(&live, &["project", "remove", "P-1"]));
    assert!(err.contains("stopped"), "{err}");
    assert_eq!(listed(&live).len(), 2, "nothing was added or removed");
}
