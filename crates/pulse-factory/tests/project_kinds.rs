//! AC-5 of `r1.s3.w2`: `project add` detects the kind of a root (`git` or `folder`) and its remote
//! key, and refuses a root that is neither. The controller runs in the test process on a
//! `FakeAdapter` (or, for the `GIT_DIR` case, as a child process), and the CLI runs as a child
//! process against its socket. Every repo is a temporary directory. A remote is a URL string set
//! with `git remote add`, and nothing is fetched.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::ctl::*;
use common::*;
use pf_engine::port::store::{CONTROLLER_SCOPE, ProjectKind, Store};
use pulse_factory::controller::worktrees::remote_key_of;
use serde_json::Value;

/// A directory under the sandbox, made now.
fn dir(live: &Live, name: &str) -> PathBuf {
    let d = live.sb.root.path().join("work").join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(&d).unwrap()
}

/// A git repo with one commit, under the sandbox.
fn repo(live: &Live, name: &str) -> PathBuf {
    let d = dir(live, name);
    git(&d, &["init", "-q", "-b", "main"]);
    commit(&d);
    d
}

fn commit(d: &Path) {
    git(
        d,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "x",
        ],
    );
}

fn path_str(p: &Path) -> String {
    p.display().to_string()
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

fn add_json(live: &Live, root: &Path) -> Value {
    ok_json(&live.cli(&["project", "add", &path_str(root), "--json"]))["project"].clone()
}

fn list_json(live: &Live) -> Value {
    ok_json(&live.cli(&["project", "list", "--json"]))
}

fn ledger_lines(live: &Live) -> Vec<String> {
    std::fs::read_to_string(live.sb.state.join("controller.ledger"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn admissions(live: &Live) -> usize {
    live.controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .len()
}

/// The stored key, read through the store.
fn stored_key(live: &Live, id: &str) -> Option<String> {
    live.controller
        .store()
        .project(id)
        .unwrap()
        .unwrap()
        .remote_key
}

/// Run `add` on a root that must be refused: one admission, one ledger line, and no project.
fn refusal_of(live: &Live, root: &Path) -> String {
    let (a, l) = (admissions(live), ledger_lines(live).len());
    let why = refused(&live.cli(&["project", "add", &path_str(root)]));
    assert_eq!(admissions(live), a + 1, "one admission");
    let lines = ledger_lines(live);
    assert_eq!(lines.len(), l + 1, "one ledger line");
    assert!(
        lines
            .last()
            .unwrap()
            .contains("project_add refused reason="),
        "{lines:?}"
    );
    assert!(live.controller.store().projects().unwrap().is_empty());
    why
}

#[test]
fn a_main_worktree_is_git_and_a_plain_folder_and_a_subdirectory_are_folders() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let p = add_json(&live, &r);
    assert_eq!(p["kind"], "git");
    assert_eq!(
        p["remote_key"],
        Value::Null,
        "a repo with no remote has no key"
    );

    let f = dir(&live, "plain");
    let p = add_json(&live, &f);
    assert_eq!(p["kind"], "folder");
    assert_eq!(p["remote_key"], Value::Null);

    // A subdirectory of a repo is a folder project: its git operations never reach outside it.
    let sub = r.join("src").join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    git(
        &r,
        &["remote", "add", "origin", "https://example.invalid/o/r.git"],
    );
    let p = add_json(&live, &sub);
    assert_eq!(p["kind"], "folder");
    assert_eq!(
        p["remote_key"],
        Value::Null,
        "a folder has no key, even in a repo with a remote"
    );

    // The kind is stored, and a line names it.
    let rows = live.controller.store().projects().unwrap();
    assert_eq!(rows[0].kind, Some(ProjectKind::Git));
    assert_eq!(rows[1].kind, Some(ProjectKind::Folder));
    assert_eq!(rows[2].kind, Some(ProjectKind::Folder));
    let lines = ledger_lines(&live);
    assert!(lines[0].contains(" kind=git command="), "{}", lines[0]);
    assert!(lines[1].contains(" kind=folder command="), "{}", lines[1]);
}

#[test]
fn a_repo_that_git_refuses_for_dubious_ownership_is_refused_and_a_folder_is_not() {
    // The seam adds the variable to the controller's git calls only. The helper below runs git
    // outside the controller, so it builds the repos as usual.
    let live = Live::start_config(BIN.into(), |c| {
        c.git_env.push((
            "GIT_TEST_ASSUME_DIFFERENT_OWNER".to_string(),
            "1".to_string(),
        ));
    });
    let main = repo(&live, "main");
    let why = refusal_of(&live, &main);
    assert!(
        why.trim().ends_with(&format!(
            "the root is in a repository that git refuses for dubious ownership: {}",
            main.display()
        )),
        "{why}"
    );
    // A subdirectory of that repo is inside the refused repo too: not a folder by default.
    let sub = main.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let why = refusal_of(&live, &sub);
    assert!(why.contains("dubious ownership"), "{why}");
    // A plain folder holds no repo, so git has no ownership to doubt.
    assert_eq!(add_json(&live, &dir(&live, "plain"))["kind"], "folder");
}

#[test]
fn a_linked_worktree_a_bare_repo_and_a_git_directory_are_refused() {
    let live = Live::start();
    let main = repo(&live, "main");
    let linked = dir(&live, "linked-parent").join("feature");
    git(
        &main,
        &["worktree", "add", "-q", &path_str(&linked), "-b", "feature"],
    );
    let linked = std::fs::canonicalize(&linked).unwrap();

    let why = refusal_of(&live, &linked);
    assert!(why.contains("the root is a linked worktree"), "{why}");
    assert!(
        why.contains(&format!(
            "add its main worktree instead: {}",
            main.display()
        )),
        "{why}"
    );

    let bare = dir(&live, "bare.git");
    git(&bare, &["init", "-q", "--bare"]);
    let why = refusal_of(&live, &bare);
    assert!(why.contains("the root is a bare repository"), "{why}");

    let why = refusal_of(&live, &main.join(".git"));
    assert!(why.contains("the root is inside a git directory"), "{why}");

    // The main worktree itself is fine, and so is a folder of the same sandbox.
    assert_eq!(add_json(&live, &main)["kind"], "git");
}

#[test]
fn a_submodule_checkout_and_a_separate_git_dir_are_refused_as_not_main_worktrees() {
    let live = Live::start();
    // `--separate-git-dir`: the repo's git directory lies elsewhere, so `<root>/.git` is a file.
    let root = dir(&live, "separate");
    let gitdir = dir(&live, "elsewhere.git");
    git(
        &root,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--separate-git-dir={}", gitdir.display()),
        ],
    );
    let why = refusal_of(&live, &root);
    assert!(why.contains("the root is not a main worktree"), "{why}");
    assert!(why.contains("git directory"), "{why}");
}

#[test]
fn the_controllers_own_git_variables_do_not_steer_its_git_calls() {
    // A real controller process, with GIT_DIR and GIT_WORK_TREE in its environment. Those would
    // make every git call answer about one repo, and they are removed from each call.
    let sb = Sandbox::new();
    let elsewhere = sb.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    git(&elsewhere, &["init", "-q", "-b", "main"]);
    let folder = sb.root.path().join("folder");
    std::fs::create_dir_all(&folder).unwrap();
    let folder = std::fs::canonicalize(&folder).unwrap();

    let child = sb
        .cmd(&["controller", "--tick-ms", "3600000"])
        .env("GIT_DIR", elsewhere.join(".git"))
        .env("GIT_WORK_TREE", &elsewhere)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the controller");
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _controller = Kill(child);
    for _ in 0..400 {
        if pf_ipc::call(&sb.state, "runs", serde_json::json!({})).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let out = sb.run(&[
        "project".into(),
        "add".into(),
        path_str(&folder),
        "--json".into(),
    ]);
    let v = ok_json(&out);
    assert_eq!(v["project"]["kind"], "folder");
}

#[test]
fn the_remote_key_is_origin_else_the_one_remote_else_none() {
    let live = Live::start();

    // origin, with a user, a password, an upper-case host and a `.git` suffix.
    let a = repo(&live, "a");
    git(
        &a,
        &[
            "remote",
            "add",
            "origin",
            "https://user:tok@GitHub.com/Owner/Repo.git",
        ],
    );
    let p = add_json(&live, &a);
    assert_eq!(p["remote_key"], "github.com/Owner/Repo");
    assert_eq!(
        stored_key(&live, "P-1").as_deref(),
        Some("github.com/Owner/Repo")
    );

    // One remote, not named origin.
    let b = repo(&live, "b");
    git(
        &b,
        &["remote", "add", "up", "git@example.com:team/proj.git"],
    );
    assert_eq!(add_json(&live, &b)["remote_key"], "example.com/team/proj");

    // Two remotes and no origin: no key. And origin wins over another remote.
    let c = repo(&live, "c");
    git(
        &c,
        &["remote", "add", "one", "https://example.com/x/one.git"],
    );
    git(
        &c,
        &["remote", "add", "two", "https://example.com/x/two.git"],
    );
    assert_eq!(add_json(&live, &c)["remote_key"], Value::Null);
    let d = repo(&live, "d");
    git(
        &d,
        &["remote", "add", "other", "https://example.com/x/other.git"],
    );
    git(
        &d,
        &["remote", "add", "origin", "https://example.com/x/main.git"],
    );
    assert_eq!(add_json(&live, &d)["remote_key"], "example.com/x/main");

    // No remote, and a remote that is a local path.
    let e = repo(&live, "e");
    assert_eq!(add_json(&live, &e)["remote_key"], Value::Null);
    let f = repo(&live, "f");
    git(&f, &["remote", "add", "origin", "/srv/git/repo.git"]);
    assert_eq!(add_json(&live, &f)["remote_key"], Value::Null);

    // Nothing of the password reaches an answer, an error line or the ledger.
    let ledger = ledger_lines(&live).join("\n");
    assert!(!ledger.contains("tok"), "{ledger}");
    assert!(!ledger.contains("https://"), "{ledger}");
    assert!(!ledger.contains("user:"), "{ledger}");
}

#[test]
fn remote_key_of_gives_the_key_of_each_url_form_and_none_for_the_rest() {
    let keys = [
        (
            "https://github.com/pulseai-labs/pulse-factory.git",
            "github.com/pulseai-labs/pulse-factory",
        ),
        ("https://github.com/o/r", "github.com/o/r"),
        ("http://example.com/o/r.git", "example.com/o/r"),
        (
            "https://user:tok@GitHub.com/Owner/Repo.git",
            "github.com/Owner/Repo",
        ),
        ("https://tok@github.com/o/r.git", "github.com/o/r"),
        ("https://github.com:8443/o/r.git", "github.com/o/r"),
        ("git@github.com:o/r.git", "github.com/o/r"),
        ("git@GitHub.com:o/r", "github.com/o/r"),
        ("host.example:o/r.git", "host.example/o/r"),
        ("ssh://git@Example.com:2222/o/r.git", "example.com/o/r"),
        ("ssh://example.com/o/r.git", "example.com/o/r"),
        ("git://host.example/a/b/c/d.git", "host.example/a/b/c/d"),
        ("https://host.example/path/", "host.example/path"),
        ("https://host.example/path.git/", "host.example/path"),
        ("  https://host.example/o/r.git\n", "host.example/o/r"),
    ];
    for (url, key) in keys {
        assert_eq!(remote_key_of(url).as_deref(), Some(key), "{url}");
    }
    for url in [
        "",
        "repo",
        "/srv/git/repo.git",
        "../repo.git",
        "./repo",
        "file:///srv/git/repo.git",
        "https://github.com",
        "https://github.com/",
        "https://user:tok@github.com",
        "ftp://host.example/o/r.git",
        "https:///o/r.git",
        "https://host.example/with space/r.git",
        "git@:o/r.git",
        "git@github.com:",
    ] {
        assert_eq!(remote_key_of(url), None, "{url:?}");
    }
    // Whatever the URL, the key never holds the password or the user.
    for url in [
        "https://user:tok@GitHub.com/Owner/Repo.git",
        "ssh://user:tok@h:22/o/r.git",
    ] {
        let key = remote_key_of(url).unwrap();
        assert!(!key.contains("tok") && !key.contains("user"), "{key}");
    }
}

#[test]
fn a_list_reads_the_key_again_and_writes_nothing() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    git(
        &r,
        &["remote", "add", "origin", "https://github.com/o/old.git"],
    );
    add_json(&live, &r);
    assert_eq!(
        stored_key(&live, "P-1").as_deref(),
        Some("github.com/o/old")
    );

    git(
        &r,
        &[
            "remote",
            "set-url",
            "origin",
            "https://user:tok@github.com/o/new.git",
        ],
    );
    let (a, l) = (admissions(&live), ledger_lines(&live));
    let v = list_json(&live);
    assert_eq!(
        v["projects"][0]["remote_key"], "github.com/o/new",
        "the key read live"
    );
    assert_eq!(admissions(&live), a, "a list adds no admission row");
    assert_eq!(ledger_lines(&live), l, "a list adds no ledger line");
    assert_eq!(
        stored_key(&live, "P-1").as_deref(),
        Some("github.com/o/old"),
        "a list stores nothing"
    );
    assert!(!stdout(&live.cli(&["project", "list", "--json"])).contains("tok"));

    // With the root's git broken, the list answers the stored key and writes nothing.
    std::fs::rename(r.join(".git"), r.join(".git-off")).unwrap();
    let v = list_json(&live);
    assert_eq!(v["projects"][0]["remote_key"], "github.com/o/old");
    assert_eq!(v["projects"][0]["kind"], "git");
    assert_eq!(admissions(&live), a);
    assert_eq!(ledger_lines(&live), l);
    assert_eq!(
        stored_key(&live, "P-1").as_deref(),
        Some("github.com/o/old")
    );

    // The same with a root that is no longer a directory.
    std::fs::remove_dir_all(&r).unwrap();
    let v = list_json(&live);
    assert_eq!(v["projects"][0]["remote_key"], "github.com/o/old");
    assert_eq!(admissions(&live), a);
}

#[test]
fn the_url_and_the_token_are_in_no_answer_no_stderr_line_and_no_ledger_line() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let url = "https://user:tok@github.com/o/r.git";
    git(&r, &["remote", "add", "origin", url]);
    let mut seen = Vec::new();
    for args in [
        vec!["project", "add", &path_str(&r)],
        vec!["project", "list"],
        vec!["project", "list", "--json"],
        vec!["project", "add", &path_str(&r), "--json"],
    ] {
        let out = live.cli(&args);
        seen.push(stdout(&out));
        seen.push(stderr(&out));
    }
    seen.push(ledger_lines(&live).join("\n"));
    let store = live.controller.store();
    for a in store.admissions(CONTROLLER_SCOPE).unwrap() {
        seen.push(format!("{a:?}"));
    }
    seen.push(format!("{:?}", store.projects().unwrap()));
    for text in &seen {
        for secret in ["tok", url, "user:", "https://user"] {
            assert!(!text.contains(secret), "{secret} appears in {text}");
        }
    }
}

#[test]
fn a_replay_of_an_add_made_before_the_remote_changed_is_still_a_replay() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    git(
        &r,
        &["remote", "add", "origin", "https://github.com/o/old.git"],
    );
    let first = ok_json(&live.cli(&[
        "project",
        "add",
        &path_str(&r),
        "--command-id",
        "add-1",
        "--json",
    ]));
    assert_eq!(first["replayed"], false);
    git(
        &r,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/o/new.git",
        ],
    );
    let lines = ledger_lines(&live);
    let again = ok_json(&live.cli(&[
        "project",
        "add",
        &path_str(&r),
        "--command-id",
        "add-1",
        "--json",
    ]));
    assert_eq!(again["replayed"], true);
    assert_eq!(
        again["project"]["remote_key"], "github.com/o/old",
        "the first record's key"
    );
    assert_eq!(ledger_lines(&live), lines, "a replay writes no line");
}

#[test]
fn project_list_json_holds_the_three_fields_for_every_project() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    git(
        &r,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    add_json(&live, &r);
    add_json(&live, &dir(&live, "plain"));
    let v = list_json(&live);
    assert_eq!(v["schema_version"], 1);
    for p in v["projects"].as_array().unwrap() {
        let o = p.as_object().unwrap();
        for key in ["kind", "remote_key", "kind_error"] {
            assert!(o.contains_key(key), "{key} is missing: {p}");
        }
        assert_eq!(
            p["kind_error"],
            Value::Null,
            "a project with a kind has no cause"
        );
    }
    assert_eq!(v["projects"][0]["kind"], "git");
    assert_eq!(v["projects"][0]["remote_key"], "github.com/o/r");
    assert_eq!(v["projects"][1]["kind"], "folder");
    assert_eq!(v["projects"][1]["remote_key"], Value::Null);
}

#[test]
fn the_text_forms_of_add_and_list_are_unchanged_for_a_project_with_a_kind() {
    let live = Live::start();
    let r = repo(&live, "gitty");
    let f = dir(&live, "plain");
    let out = live.cli(&["project", "add", &path_str(&r)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("project P-1 added: gitty ({})\n", r.display())
    );
    let out = live.cli(&["project", "add", &path_str(&f)]);
    assert_eq!(
        stdout(&out),
        format!("project P-2 added: plain ({})\n", f.display())
    );
    let out = live.cli(&["project", "list"]);
    assert_eq!(
        stdout(&out),
        format!("P-1  gitty  {}\nP-2  plain  {}\n", r.display(), f.display())
    );
}
