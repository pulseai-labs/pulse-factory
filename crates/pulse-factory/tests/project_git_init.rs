//! AC-2 of `r1.s5.w5`: `project add` with `git_init`. A plain folder under the server's home folder
//! becomes a repository and a `git` project in one admitted command. The controller runs in the test
//! process on a `FakeAdapter`, with its home folder a temp directory, and the CLI runs as a child process
//! against its socket. A device's call goes through the real API (`api_listen` on a loopback port).
//! Nothing spends money or reaches beyond `127.0.0.1`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::chat::cli_json;
use common::ctl::Live;
use common::*;
use pf_engine::port::store::{CONTROLLER_SCOPE, Store};
use pf_ipc::{Handler, Request, Response};
use pulse_factory::controller::ports::LivePorts;
use pulse_factory::controller::{Config, Controller, api};
use serde_json::{Value, json};

/// A live controller whose home folder is a fresh temp directory (canonical).
struct Home {
    live: Live,
    home: PathBuf,
    _dir: tempfile::TempDir,
}

fn git_env() -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]
}

fn start() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    let for_cfg = home.clone();
    let live = Live::start_config(BIN.into(), move |cfg| {
        cfg.home = Some(for_cfg);
        cfg.git_env = git_env();
    });
    Home {
        live,
        home,
        _dir: dir,
    }
}

impl Home {
    fn dir(&self, rel: &str) -> PathBuf {
        let d = self.home.join(rel);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn add(&self, root: &Path, name: Option<&str>, git_init: Option<bool>, id: &str) -> Response {
        let mut args = json!({ "root": root, "command_id": id });
        if let Some(n) = name {
            args["name"] = json!(n);
        }
        if let Some(g) = git_init {
            args["git_init"] = json!(g);
        }
        self.live
            .controller
            .handle(Request::new("project_add", args))
    }

    fn ok(&self, r: Response) -> Value {
        assert!(r.ok, "refused: {:?}", r.error);
        r.result
    }

    fn refused(&self, r: Response) -> String {
        assert!(!r.ok, "added: {}", r.result);
        r.error.unwrap_or_default()
    }

    fn projects(&self) -> usize {
        self.live.controller.store().projects().unwrap().len()
    }

    fn ledger(&self) -> Vec<String> {
        std::fs::read_to_string(self.live.sb.state.join("controller.ledger"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn content_of(&self, id: &str) -> String {
        self.live
            .controller
            .store()
            .admission(CONTROLLER_SCOPE, id)
            .unwrap()
            .unwrap_or_else(|| panic!("no admission for {id}"))
            .content
    }
}

fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

#[test]
fn a_plain_folder_under_home_becomes_a_git_project_and_a_repository() {
    let h = start();
    let root = h.dir("code/scratch");

    let v = h.ok(h.add(&root, None, Some(true), "gi-1"));

    assert_eq!(v["replayed"], false);
    assert_eq!(v["project"]["name"], "scratch");
    assert_eq!(v["project"]["root"], root.to_str().unwrap());
    assert_eq!(
        v["project"]["kind"], "git",
        "the kind is decided after the init, so it is git from the first row: {v}"
    );
    assert!(is_repo(&root), "git init ran in the folder");
    assert_eq!(h.projects(), 1);
    // The folder is a repository on a branch git names (an unborn branch, no commit).
    let branch = git(&root, &["symbolic-ref", "--short", "HEAD"]);
    assert!(!branch.is_empty());
    // `git init` made no commit and no remote.
    assert_eq!(git(&root, &["remote"]), "");
    assert_eq!(git(&root, &["count-objects"]), "0 objects, 0 kilobytes");
}

#[test]
fn the_ledger_line_has_git_init_true_and_an_add_without_it_has_none() {
    let h = start();
    let with = h.dir("with-init");
    let without = h.dir("without-init");
    h.ok(h.add(&with, None, Some(true), "gi-with"));
    h.ok(h.add(&without, None, None, "gi-without"));

    let lines = h.ledger();
    let w = lines
        .iter()
        .find(|l| l.contains("command=gi-with"))
        .unwrap();
    assert!(w.contains(" git_init=true "), "{w}");
    assert!(w.contains("kind=git"), "{w}");
    let wo = lines
        .iter()
        .find(|l| l.contains("command=gi-without"))
        .unwrap();
    assert!(!wo.contains("git_init"), "{wo}");
    assert!(wo.contains("kind=folder"), "{wo}");
    assert!(
        !is_repo(&without),
        "an add without git_init makes no repository"
    );
}

#[test]
fn an_add_without_git_init_has_the_admission_content_it_always_had() {
    let h = start();
    let root = h.dir("plain");
    let root_text = root.to_str().unwrap();
    h.ok(h.add(&root, Some("plain"), None, "bytes-none"));
    let other = h.dir("other");
    h.ok(h.add(&other, Some("other"), Some(false), "bytes-false"));

    // Byte for byte as before the field existed: the name, then the root, and nothing more.
    assert_eq!(
        h.content_of("bytes-none"),
        format!(r#"{{"name":"plain","root":{}}}"#, json!(root_text))
    );
    assert_eq!(
        h.content_of("bytes-false"),
        format!(
            r#"{{"name":"other","root":{}}}"#,
            json!(other.to_str().unwrap())
        )
    );
    // With the field, the content gains it and only it.
    let third = h.dir("third");
    h.ok(h.add(&third, Some("third"), Some(true), "bytes-true"));
    assert_eq!(
        h.content_of("bytes-true"),
        format!(
            r#"{{"name":"third","root":{},"git_init":true}}"#,
            json!(third.to_str().unwrap())
        )
    );
}

#[test]
fn a_root_outside_the_home_folder_is_refused_and_nothing_changes_on_disk() {
    let h = start();
    let outside = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(outside.path()).unwrap();
    std::fs::write(root.join("keep"), "x").unwrap();
    let before = listing(&root);

    let why = h.refused(h.add(&root, None, Some(true), "gi-out"));

    assert!(
        why.contains("the root is not under the home folder"),
        "{why}"
    );
    assert_eq!(listing(&root), before, "nothing changed on disk");
    assert!(!is_repo(&root));
    assert_eq!(h.projects(), 0);
    // The home folder itself is not "under" it: no repository is made in the home folder.
    let why = h.refused(h.add(&h.home, None, Some(true), "gi-home"));
    assert!(
        why.contains("the root is not under the home folder"),
        "{why}"
    );
    assert!(!is_repo(&h.home));
}

#[test]
fn a_repository_and_a_subfolder_of_a_repository_are_refused_and_nothing_changes() {
    let h = start();
    let repo = h.dir("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    let sub = h.dir("repo/src/deep");
    let before = listing(&repo);

    let why = h.refused(h.add(&repo, None, Some(true), "gi-repo"));
    assert!(
        why.contains("the root is a git repository already"),
        "{why}"
    );
    let why = h.refused(h.add(&sub, None, Some(true), "gi-sub"));
    assert!(why.contains("the root is inside a git repository"), "{why}");

    assert_eq!(
        listing(&repo),
        before,
        "git init ran nowhere in the repository"
    );
    assert!(!is_repo(&sub));
    assert_eq!(h.projects(), 0);
    // Without git_init, the same two roots are accepted as today (a repo, and a folder project).
    h.ok(h.add(&repo, None, None, "plain-repo"));
    h.ok(h.add(&sub, None, None, "plain-sub"));
}

#[test]
fn a_bad_name_is_refused_before_git_init_runs() {
    let h = start();
    let root = h.dir("badname");
    let long = "n".repeat(65);

    let why = h.refused(h.add(&root, Some(&long), Some(true), "gi-name"));
    assert!(why.contains("the name has 65 characters"), "{why}");
    assert!(!is_repo(&root), "the folder is not a repository after");
    let why = h.refused(h.add(&root, Some(""), Some(true), "gi-empty"));
    assert!(why.contains("the name is empty"), "{why}");
    assert!(!is_repo(&root));
    assert_eq!(h.projects(), 0);

    // A missing root and a file root are refused before git init, too.
    let missing = h.home.join("missing");
    let why = h.refused(h.add(&missing, None, Some(true), "gi-missing"));
    assert!(why.contains("the root does not exist"), "{why}");
    assert!(!missing.exists());
}

#[test]
fn a_root_or_name_a_live_project_has_is_refused_before_git_init_runs() {
    let h = start();
    let first = h.dir("first");
    h.ok(h.add(&first, Some("taken"), None, "dup-first"));
    // The same root, now with git_init: refused, and the folder stays a plain folder.
    let why = h.refused(h.add(&first, Some("another"), Some(true), "dup-root"));
    assert!(why.contains("a live project already has the root"), "{why}");
    assert!(!is_repo(&first));
    // Another folder with a name a live project has.
    let second = h.dir("second");
    let why = h.refused(h.add(&second, Some("taken"), Some(true), "dup-name"));
    assert!(
        why.contains("a live project already has the name taken"),
        "{why}"
    );
    assert!(!is_repo(&second), "the name check ran before git init");
    assert_eq!(h.projects(), 1);
}

#[test]
fn a_replay_with_the_same_command_id_runs_no_second_git_init() {
    let h = start();
    let root = h.dir("replayed");
    let first = h.ok(h.add(&root, None, Some(true), "gi-replay"));
    assert_eq!(first["replayed"], false);
    assert!(is_repo(&root));
    // Take the repository away. A second `git init` would bring it back.
    std::fs::remove_dir_all(root.join(".git")).unwrap();

    let again = h.ok(h.add(&root, None, Some(true), "gi-replay"));

    assert_eq!(again["replayed"], true);
    assert_eq!(again["project"]["id"], first["project"]["id"]);
    assert!(!is_repo(&root), "a replay runs no git init");
    assert_eq!(h.projects(), 1);
    // The ledger holds one line for the command.
    let lines: Vec<_> = h
        .ledger()
        .into_iter()
        .filter(|l| l.contains("command=gi-replay"))
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    // The same ID with other content is a conflict, and runs no git init either.
    let other = h.dir("other-root");
    let why = h.refused(h.add(&other, None, Some(true), "gi-replay"));
    assert!(why.contains("used before with other arguments"), "{why}");
    assert!(!is_repo(&other));
}

#[test]
fn a_git_failure_refuses_the_add_with_its_cause_and_touches_no_project() {
    let h = start();
    let root = h.dir("readonly");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
    // A process with the right to write anyway (root) cannot make this case.
    if std::fs::write(root.join("probe"), "x").is_ok() {
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let why = h.refused(h.add(&root, None, Some(true), "gi-fail"));
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!why.is_empty());
    assert!(
        why.contains("ermission") || why.contains("ead-only"),
        "git's cause: {why}"
    );
    assert!(!is_repo(&root));
    assert_eq!(h.projects(), 0, "the store holds no project");
}

#[test]
fn the_cli_flag_sends_git_init_and_prints_the_project() {
    let h = start();
    let root = h.dir("via-cli");
    let out = h.live.cli(&[
        "project",
        "add",
        root.to_str().unwrap(),
        "--git-init",
        "--json",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["project"]["kind"], "git");
    assert!(is_repo(&root));

    // Without the flag, the CLI sends no field.
    let plain = h.dir("via-cli-plain");
    let v = cli_json(&h.live, &["project", "add", plain.to_str().unwrap()]);
    assert_eq!(v["project"]["kind"], "folder");
    assert!(!is_repo(&plain));
}

// ------------------------------------------------------------------ a device, through the real API

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// A controller in the test process with the API on a loopback port and the sandbox root as its home
/// folder.
struct Api {
    sb: Sandbox,
    addr: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Api {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Api {
    fn on() -> Api {
        let sb = Sandbox::new();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        pf_ipc::prepare_state_dir(&sb.state).unwrap();
        let bound = pf_ipc::bind(&sb.state).unwrap();
        let mut cfg = Config::new(sb.state.clone(), 3_600_000, BIN.into());
        cfg.api_listen = Some(addr.clone());
        cfg.home = Some(std::fs::canonicalize(sb.root.path()).unwrap());
        cfg.git_env = git_env();
        let controller = Controller::open(cfg, Arc::new(LivePorts)).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let served = controller.clone();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let _api = api::spawn_api(&served);
                pf_ipc::serve(bound, served, async {
                    let _ = rx.await;
                })
                .await;
            });
        });
        let rig = Api {
            sb,
            addr,
            stop: Some(tx),
            thread: Some(thread),
        };
        for _ in 0..400 {
            let up = pf_ipc::call(&rig.sb.state, "api_status", json!({}))
                .is_ok_and(|r| r.ok && r.result["state"] == "listening");
            if up {
                return rig;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the API never listened");
    }

    fn http(&self, path: &str, token: Option<&str>, body: &str) -> (u16, Value) {
        let spki = pf_ipc::call(&self.sb.state, "api_status", json!({}))
            .unwrap()
            .result["key_spki_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        let pin = format!("sha256//{}", b64(&unhex(&spki)));
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "-k", "--max-time", "20", "--pinnedpubkey", &pin])
            .args(["-X", "POST", "-w", "\n%{http_code}"])
            .args([
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                body,
            ])
            .arg(format!("https://{}{path}", self.addr))
            .stdout(Stdio::piped());
        if let Some(t) = token {
            cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
        }
        let out = cmd.output().expect("curl is on this host");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", "0"));
        (
            status.trim().parse().unwrap_or(0),
            serde_json::from_str(body).unwrap_or(Value::Null),
        )
    }

    fn token(&self) -> String {
        let out = self.sb.cmd(&["device", "pair", "--json"]).output().unwrap();
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
        let (status, paired) = self.http(
            "/v1/pair",
            None,
            &json!({ "code": v["code"], "device_name": "macbook" }).to_string(),
        );
        assert_eq!(status, 200, "{paired}");
        paired["token"].as_str().unwrap().to_string()
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.sb.state.join(file))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

#[test]
fn a_device_adds_with_git_init_through_the_api_and_gets_its_api_ledger_line() {
    let api = Api::on();
    let token = api.token();
    let root = api.sb.root.path().join("work/device-made");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let body = json!({ "root": root, "git_init": true, "command_id": "dev-gi-1" }).to_string();

    let (status, answer) = api.http("/v1/verbs/project_add", Some(&token), &body);

    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["project"]["kind"], "git");
    assert!(is_repo(&root));
    let lines: Vec<_> = api
        .lines("api.ledger")
        .into_iter()
        .filter(|l| l.contains("verb=project_add"))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "one api.ledger line for the call: {lines:?}"
    );
    for word in ["device=D-1", "status=200", "outcome=ok"] {
        assert!(lines[0].contains(word), "{word} in {}", lines[0]);
    }
    let ledger = api.lines("controller.ledger");
    assert!(
        ledger.iter().any(|l| l.contains("project_add")
            && l.contains("git_init=true")
            && l.contains("command=dev-gi-1")),
        "{ledger:?}"
    );
    // A replay of the same call over the API runs no second init and says so.
    std::fs::remove_dir_all(root.join(".git")).unwrap();
    let (status, again) = api.http("/v1/verbs/project_add", Some(&token), &body);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["replayed"], true);
    assert!(!is_repo(&root));
    // A root outside the home folder is refused to a device, and no repository is made there.
    let outside = tempfile::tempdir().unwrap();
    let body =
        json!({ "root": outside.path(), "git_init": true, "command_id": "dev-gi-2" }).to_string();
    let (status, refused) = api.http("/v1/verbs/project_add", Some(&token), &body);
    assert_eq!(status, 422, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("not under the home folder"),
        "{refused}"
    );
    assert!(!is_repo(outside.path()));
}
