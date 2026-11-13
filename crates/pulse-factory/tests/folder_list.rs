//! AC-1 of `r1.s5.w5`: the `folder_list` verb of the controller, through the socket handler. It lists
//! the folders of the server's home folder (`Config::home`) for the Add a project picker, marks git
//! repositories with their branch and the folders that are projects, and is confined to the home
//! folder. The controller runs in the test process on a `FakeAdapter`; the home folder is a temp
//! directory. Nothing spends money or reaches the network.

mod common;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use common::chat::{add_project, cli_json};
use common::ctl::Live;
use common::*;
use pf_ipc::{Handler, Request};
use serde_json::{Value, json};

/// A live controller whose home folder is a fresh temp directory (canonical), and a second
/// directory outside it.
struct Home {
    live: Live,
    home: PathBuf,
    outside: PathBuf,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn start_with_home(set: bool) -> Home {
    let home_dir = tempfile::tempdir().unwrap();
    let outside_dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home_dir.path()).unwrap();
    let outside = std::fs::canonicalize(outside_dir.path()).unwrap();
    let for_cfg = home.clone();
    let live = Live::start_config(BIN.into(), move |cfg| {
        if set {
            cfg.home = Some(for_cfg);
        }
        // The controller's own git calls read no host configuration.
        cfg.git_env = vec![
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ];
    });
    Home {
        live,
        home,
        outside,
        _dirs: (home_dir, outside_dir),
    }
}

fn start() -> Home {
    start_with_home(true)
}

impl Home {
    fn dir(&self, rel: &str) -> PathBuf {
        let d = self.home.join(rel);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn repo(&self, rel: &str) -> PathBuf {
        let d = self.dir(rel);
        git(&d, &["init", "-q", "-b", "main"]);
        d
    }

    /// The raw answer of `folder_list`.
    fn call(&self, args: Value) -> pf_ipc::Response {
        self.live
            .controller
            .handle(Request::new("folder_list", args))
    }

    /// The listing of a path (the home folder when `None`). It fails the test on a refusal.
    fn list(&self, path: Option<&Path>) -> Value {
        let args = match path {
            Some(p) => json!({ "path": p }),
            None => json!({}),
        };
        let r = self.call(args);
        assert!(r.ok, "folder_list refused: {:?}", r.error);
        r.result
    }

    /// The refusal text of a path. It fails the test when the verb answers a listing.
    fn refused(&self, path: &str) -> String {
        let r = self.call(json!({ "path": path }));
        assert!(!r.ok, "folder_list listed {path}: {}", r.result);
        assert!(
            r.result.is_null(),
            "a refusal carries no listing: {}",
            r.result
        );
        r.error.unwrap_or_default()
    }

    fn ledger(&self) -> Vec<String> {
        std::fs::read_to_string(self.live.sb.state.join("controller.ledger"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn names(list: &Value) -> Vec<String> {
    list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

fn entry<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("no entry {name} in {list}"))
}

fn commit(dir: &Path) {
    git(
        dir,
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

#[test]
fn the_home_folder_lists_its_folders_sorted_and_nothing_else() {
    let h = start();
    for d in ["zeta", "alpha", "Beta", "mid dle", ".hidden", ".config"] {
        h.dir(d);
    }
    std::fs::write(h.home.join("notes.txt"), "a file").unwrap();
    std::fs::write(h.home.join("secret"), "mode 000").unwrap();
    std::fs::set_permissions(
        h.home.join("secret"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let fifo = h.home.join("a-fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let _socket = UnixListener::bind(h.home.join("a-socket")).unwrap();
    symlink(h.home.join("missing"), h.home.join("broken-link")).unwrap();

    let list = h.list(None);

    assert_eq!(list["schema_version"], 1);
    assert_eq!(list["home"], h.home.to_str().unwrap());
    assert_eq!(list["path"], h.home.to_str().unwrap());
    assert!(
        list["parent"].is_null(),
        "the home folder has no parent: {list}"
    );
    assert_eq!(
        names(&list),
        ["Beta", "alpha", "mid dle", "zeta"],
        "byte order, no dot folder, no file, FIFO, socket or broken link"
    );
    assert_eq!(list["truncated"], false);
    let alpha = entry(&list, "alpha");
    assert_eq!(alpha["path"], h.home.join("alpha").to_str().unwrap());
    assert_eq!(alpha["git"], false);
    assert!(alpha["branch"].is_null());
    assert!(alpha["project_id"].is_null());
}

#[test]
fn a_subfolder_lists_with_its_parent() {
    let h = start();
    h.dir("code/one");
    h.dir("code/two");
    let list = h.list(Some(&h.home.join("code")));
    assert_eq!(names(&list), ["one", "two"]);
    assert_eq!(list["path"], h.home.join("code").to_str().unwrap());
    assert_eq!(list["parent"], h.home.to_str().unwrap());
    // The one level up from `code/one` is `code`.
    let deeper = h.list(Some(&h.home.join("code/one")));
    assert_eq!(deeper["parent"], h.home.join("code").to_str().unwrap());
    assert!(names(&deeper).is_empty());
}

#[test]
fn a_git_folder_shows_its_branch_and_a_detached_head_shows_none() {
    let h = start();
    h.repo("on-main");
    let detached = h.repo("detached");
    commit(&detached);
    git(&detached, &["checkout", "-q", "--detach"]);
    let unborn = h.repo("unborn");
    git(
        &unborn,
        &["symbolic-ref", "HEAD", "refs/heads/work/feature"],
    );
    h.dir("plain");

    let list = h.list(None);

    let main = entry(&list, "on-main");
    assert_eq!(main["git"], true);
    assert_eq!(main["branch"], "main");
    let d = entry(&list, "detached");
    assert_eq!(d["git"], true, "a detached HEAD is still a repository");
    assert!(d["branch"].is_null(), "{d}");
    assert_eq!(entry(&list, "unborn")["branch"], "work/feature");
    let plain = entry(&list, "plain");
    assert_eq!(plain["git"], false);
    assert!(plain["branch"].is_null());
}

#[test]
fn a_folder_that_is_a_live_project_shows_its_id_and_a_removed_one_shows_none() {
    let h = start();
    let kept = h.dir("kept");
    let gone = h.dir("gone");
    h.dir("free");
    let kept_id = add_project(&h.live, &kept);
    let gone_id = add_project(&h.live, &gone);

    let list = h.list(None);
    assert_eq!(entry(&list, "kept")["project_id"], kept_id.as_str());
    assert_eq!(entry(&list, "gone")["project_id"], gone_id.as_str());
    assert!(entry(&list, "free")["project_id"].is_null());

    cli_json(&h.live, &["project", "remove", &gone_id]);
    let list = h.list(None);
    assert_eq!(entry(&list, "kept")["project_id"], kept_id.as_str());
    assert!(
        entry(&list, "gone")["project_id"].is_null(),
        "a removed project is no project: {list}"
    );
}

#[test]
fn repo_names_the_repository_around_the_listed_path() {
    let h = start();
    let outer = h.repo("outer");
    std::fs::create_dir_all(outer.join("src/inner")).unwrap();
    h.dir("elsewhere/leaf");

    // The home folder and a plain folder are in no repository.
    assert!(h.list(None)["repo"].is_null());
    assert!(h.list(Some(&h.home.join("elsewhere")))["repo"].is_null());
    // The repository itself, and a folder inside it, name the repository's top level.
    assert_eq!(h.list(Some(&outer))["repo"], outer.to_str().unwrap());
    assert_eq!(
        h.list(Some(&outer.join("src")))["repo"],
        outer.to_str().unwrap()
    );
    assert_eq!(
        h.list(Some(&outer.join("src/inner")))["repo"],
        outer.to_str().unwrap()
    );
}

#[test]
fn a_path_outside_the_home_folder_is_refused_with_no_listing() {
    let h = start();
    std::fs::create_dir_all(h.outside.join("sub")).unwrap();
    for path in ["/etc", "/", h.outside.to_str().unwrap()] {
        assert_eq!(
            h.refused(path),
            "the path is outside the home folder",
            "{path}"
        );
    }
    // A folder whose name only starts with the home folder's is not inside it (whole components).
    let sibling = format!("{}-sibling", h.home.display());
    std::fs::create_dir_all(&sibling).unwrap();
    let refusal = h.refused(&sibling);
    std::fs::remove_dir_all(&sibling).unwrap();
    assert_eq!(refusal, "the path is outside the home folder");
}

#[test]
fn a_dot_dot_that_climbs_out_of_the_home_folder_is_refused() {
    let h = start();
    h.dir("code");
    let climbing = format!("{}/code/../../../../../../etc", h.home.display());
    assert_eq!(h.refused(&climbing), "the path is outside the home folder");
    // A `..` that stays inside is resolved, and lists the folder it reaches.
    let inside = format!("{}/code/../code", h.home.display());
    let list = h.list(Some(Path::new(&inside)));
    assert_eq!(list["path"], h.home.join("code").to_str().unwrap());
}

#[test]
fn a_symlink_that_leaves_the_home_folder_is_refused_and_not_listed() {
    let h = start();
    std::fs::create_dir_all(h.outside.join("secret-dir")).unwrap();
    symlink(&h.outside, h.home.join("escape")).unwrap();
    symlink("/etc", h.home.join("to-etc")).unwrap();
    // A symlink to a folder inside the home folder is listed, as the folder it points to.
    let real = h.dir("real");
    symlink(&real, h.home.join("shortcut")).unwrap();
    // A symlink to a file is no folder.
    std::fs::write(h.outside.join("f"), "x").unwrap();
    symlink(h.outside.join("f"), h.home.join("file-link")).unwrap();

    let list = h.list(None);

    assert_eq!(names(&list), ["real", "shortcut"], "{list}");
    assert_eq!(entry(&list, "shortcut")["path"], real.to_str().unwrap());
    // Asked for by path, the symlink that leaves is refused.
    assert_eq!(
        h.refused(h.home.join("escape").to_str().unwrap()),
        "the path is outside the home folder"
    );
    assert_eq!(
        h.refused(h.home.join("escape/secret-dir").to_str().unwrap()),
        "the path is outside the home folder"
    );
    // The symlink to a folder inside lists that folder.
    let via = h.list(Some(&h.home.join("shortcut")));
    assert_eq!(via["path"], real.to_str().unwrap());
}

#[test]
fn a_missing_path_a_file_path_and_a_relative_path_are_refused_with_their_cause() {
    let h = start();
    h.dir("code");
    std::fs::write(h.home.join("code/readme.md"), "text").unwrap();
    let missing = h.home.join("code/nope");
    assert_eq!(
        h.refused(missing.to_str().unwrap()),
        format!("the path does not exist: {}", missing.display())
    );
    let file = h.home.join("code/readme.md");
    assert_eq!(
        h.refused(file.to_str().unwrap()),
        format!("the path is not a directory: {}", file.display())
    );
    assert!(
        h.refused("code")
            .starts_with("the path is not an absolute path")
    );
    assert!(
        h.refused("")
            .starts_with("the path is not an absolute path")
    );
    // A path that does not exist outside the home folder gets the one answer of a path outside:
    // the verb does not say which paths outside the home folder exist.
    assert_eq!(
        h.refused("/definitely/not/here"),
        "the path is outside the home folder"
    );
}

#[test]
fn a_server_with_no_home_folder_refuses() {
    let h = start_with_home(false);
    let r = h.call(json!({}));
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("the server has no home folder"));
    assert_eq!(h.refused("/etc"), "the server has no home folder");

    // A home that is not there, and one that is a file, are refused the same way.
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    std::fs::write(&file, "x").unwrap();
    for bad in [tmp.path().join("missing"), file] {
        let for_cfg = bad.clone();
        let live = Live::start_config(BIN.into(), move |cfg| cfg.home = Some(for_cfg));
        let r = live
            .controller
            .handle(Request::new("folder_list", json!({})));
        assert_eq!(
            r.error.as_deref(),
            Some("the server has no home folder"),
            "{}",
            bad.display()
        );
    }
}

#[test]
fn a_fifo_and_a_file_with_no_permission_do_not_stop_the_listing() {
    let h = start();
    h.dir("folder");
    let fifo = h.home.join("pipe");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let locked = h.home.join("locked");
    std::fs::write(&locked, "never read").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _socket = UnixListener::bind(h.home.join("sock")).unwrap();
    // A folder with no permission is listed, and its inside is never read.
    let sealed = h.dir("sealed");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();

    // The verb opens no file, so a FIFO with no writer cannot block it. The call runs on a thread,
    // with a limit of 10 seconds, so a blocked open fails the test and does not hang it.
    let (tx, rx) = mpsc::channel();
    let controller = h.live.controller.clone();
    std::thread::spawn(move || {
        let _ = tx.send(controller.handle(Request::new("folder_list", json!({}))));
    });
    let r = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("folder_list did not answer within 10 seconds");
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(r.ok, "{:?}", r.error);
    let names: Vec<_> = r.result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["folder", "sealed"]);
}

#[test]
fn more_than_500_folders_give_500_entries_and_truncated() {
    let h = start();
    for i in 0..501 {
        std::fs::create_dir(h.home.join(format!("d{i:03}"))).unwrap();
    }
    let list = h.list(None);
    assert_eq!(list["entries"].as_array().unwrap().len(), 500);
    assert_eq!(list["truncated"], true);
    // The first 500 by name: d000 to d499.
    assert_eq!(names(&list).first().map(String::as_str), Some("d000"));
    assert_eq!(names(&list).last().map(String::as_str), Some("d499"));

    // Exactly 500 is not truncated.
    std::fs::remove_dir(h.home.join("d500")).unwrap();
    let list = h.list(None);
    assert_eq!(list["entries"].as_array().unwrap().len(), 500);
    assert_eq!(list["truncated"], false);
}

#[test]
fn a_call_writes_no_controller_ledger_line_and_changes_nothing_on_disk() {
    let h = start();
    h.repo("repo");
    h.dir("plain");
    let before = h.ledger();
    let tree = listing(&h.home);

    h.list(None);
    h.list(Some(&h.home.join("repo")));
    assert!(!h.call(json!({ "path": "/etc" })).ok);

    assert_eq!(
        h.ledger(),
        before,
        "a read writes no controller.ledger line"
    );
    assert_eq!(listing(&h.home), tree, "a read changes nothing on disk");
}

#[test]
fn an_unreadable_argument_is_refused() {
    let h = start();
    let r = h.call(json!({ "path": 7 }));
    assert!(!r.ok);
    assert!(
        r.error
            .unwrap_or_default()
            .starts_with("the arguments are not readable"),
        "{:?}",
        r.result
    );
}
