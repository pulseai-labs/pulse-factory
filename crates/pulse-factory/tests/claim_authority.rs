//! AC-5: only the current claim answers, and a seat can never claim. The controller runs in the
//! test process on a `FakeAdapter`; the CLI runs as a child process against its socket. A seat
//! process is a real child process whose PID the test registers with the adapter, so the caller
//! check has something to find.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::ctl::*;
use common::*;
use pf_engine::port::store::RequestStatus;
use serde_json::{Value, json};

const SEAT_TOKEN: &str = "seat-token-value-1234";

fn token_path(live: &Live) -> PathBuf {
    live.sb.state.join("claim-token")
}

fn read_token(live: &Live) -> String {
    std::fs::read_to_string(token_path(live))
        .unwrap()
        .trim()
        .to_string()
}

/// A controller with a unit `u1` that is parked, and its request.
fn parked() -> (Live, String, u32) {
    let live = Live::start();
    live.arm_with_units(&["u1"]);
    live.park_unit("u1", "the reviewer found a defect");
    let view = live.handle().engine.view().unwrap();
    let open = view
        .requests
        .iter()
        .find(|r| r.status == RequestStatus::Open && r.unit_id.as_deref() == Some("u1"))
        .expect("an open request for u1")
        .clone();
    assert_eq!(live.unit_status("u1"), "parked");
    (live, open.id, open.revision)
}

fn open_requests(live: &Live) -> usize {
    live.handle()
        .engine
        .view()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.status == RequestStatus::Open)
        .count()
}

fn claim_count(live: &Live) -> usize {
    live.handle().engine.view().unwrap().claims.len()
}

fn ok(out: &std::process::Output) {
    assert_eq!(code(out), 0, "{}", stderr(out));
}

// ------------------------------------------------------------------ the seat token

#[test]
fn the_client_refuses_claim_answer_and_clear_with_a_seat_token() {
    let (live, id, rev) = parked();
    let run = live.handle().info.run_id.clone();
    let rev = rev.to_string();
    for args in [
        vec!["claim", "--holder", "seat"],
        vec!["answer", &id, "--rev", &rev, "--text", "go on"],
        vec!["clear", &run, "u1"],
    ] {
        let out = live
            .sb
            .cmd(&args)
            .env("PF_SEAT_TOKEN", SEAT_TOKEN)
            .output()
            .unwrap();
        assert_eq!(code(&out), 1, "{args:?}");
        let err = stderr(&out);
        assert!(
            err.contains("refused") && err.contains("PF_SEAT_TOKEN is set"),
            "{args:?}: {err}"
        );
        assert!(!err.contains(SEAT_TOKEN), "the refusal repeats the token");
    }
    assert_eq!(claim_count(&live), 0, "no claim was made");
    assert!(!token_path(&live).exists());
    assert_eq!(open_requests(&live), 1);
}

/// Run the test binary again as the helper below, with a seat token in its environment, and call
/// the controller from it without the client check. `/proc/<pid>/environ` shows the environment a
/// process started with, so only a new process can carry the variable.
fn call_as_a_seat(live: &Live, verb: &str, args: &Value) -> Value {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "helper_calls_the_controller",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", live.sb.path_env())
        .env("PF_SEAT_TOKEN", SEAT_TOKEN)
        .env("PF_TEST_HELPER_STATE", &live.sb.state)
        .env("PF_TEST_HELPER_VERB", verb)
        .env("PF_TEST_HELPER_ARGS", args.to_string())
        .output()
        .unwrap();
    let text = stdout(&out);
    let line = text
        .lines()
        .find_map(|l| l.split_once("HELPER_RESPONSE:").map(|(_, rest)| rest))
        .unwrap_or_else(|| panic!("no response from the helper: {text}\n{}", stderr(&out)));
    serde_json::from_str(line).unwrap()
}

/// Not a test: the process that `call_as_a_seat` starts. Run on its own it does nothing.
#[test]
fn helper_calls_the_controller() {
    let Ok(state) = std::env::var("PF_TEST_HELPER_STATE") else {
        return;
    };
    let verb = std::env::var("PF_TEST_HELPER_VERB").unwrap();
    let args: Value = serde_json::from_str(&std::env::var("PF_TEST_HELPER_ARGS").unwrap()).unwrap();
    let resp = pf_ipc::call(Path::new(&state), &verb, args).unwrap();
    println!("HELPER_RESPONSE:{}", serde_json::to_string(&resp).unwrap());
}

#[test]
fn the_controller_refuses_a_caller_whose_environment_holds_the_seat_token() {
    let (live, id, rev) = parked();
    let run = live.handle().info.run_id.clone();
    // A real claim, so that a refusal can only come from the caller check.
    ok(&live.cli(&["claim", "--holder", "ann@host"]));
    let token = read_token(&live);

    let calls = [
        ("claim", json!({ "holder": "seat" })),
        (
            "answer",
            json!({ "request": id, "rev": rev, "text": "go on", "token": token }),
        ),
        ("clear", json!({ "run": run, "unit": "u1", "token": token })),
    ];
    for (verb, args) in &calls {
        let resp = call_as_a_seat(&live, verb, args);
        assert_eq!(resp["ok"], false, "{verb}: {resp}");
        let why = resp["error"].as_str().unwrap();
        assert!(
            why.contains(&format!("{verb} refused"))
                && why.contains("PF_SEAT_TOKEN is in the caller's environment"),
            "{verb}: {why}"
        );
        assert!(!why.contains(SEAT_TOKEN) && !why.contains(&token), "{why}");
    }
    assert_eq!(claim_count(&live), 1, "no second claim");
    assert_eq!(open_requests(&live), 1, "nothing was answered");
    assert_eq!(live.unit_status("u1"), "parked");
}

// ------------------------------------------------------------------ the process tree

/// Run the CLI as the child of a shell that the adapter reports as a seat process, when
/// `as_seat` is set. The shell waits for a go file, so the PID is registered before the CLI runs.
fn cli_below_a_shell(live: &Live, as_seat: bool, args: &[&str]) -> (i32, String) {
    let dir = live.sb.root.path().join("below");
    std::fs::create_dir_all(&dir).unwrap();
    let go = dir.join("go");
    let _ = std::fs::remove_file(&go);
    let script = r#"while [ ! -e "$GO" ]; do sleep 0.02; done
"$BIN" "$@" >"$DIR/out" 2>"$DIR/err"
echo $? >"$DIR/rc""#;
    let _ = std::fs::remove_file(dir.join("rc"));
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .args(args)
        .env_clear()
        .env("PATH", live.sb.path_env())
        .env("HOME", live.sb.root.path())
        .env("PF_STATE_DIR", &live.sb.state)
        .env("PF_SHARE_DIR", &live.sb.share)
        .env("GO", &go)
        .env("DIR", &dir)
        .env("BIN", BIN)
        .spawn()
        .unwrap();
    let refs = live.session_refs();
    assert!(!refs.is_empty(), "a tracked seat needs a session row");
    if as_seat {
        live.ports
            .pids
            .lock()
            .unwrap()
            .insert(refs[0].0.clone(), child.id());
    }
    std::fs::write(&go, "").unwrap();
    child.wait().unwrap();
    live.ports.pids.lock().unwrap().clear();
    let rc = std::fs::read_to_string(dir.join("rc")).unwrap();
    let err = std::fs::read_to_string(dir.join("err")).unwrap();
    (rc.trim().parse().unwrap(), err)
}

#[test]
fn a_child_of_a_tracked_seat_is_refused_on_claim_answer_and_clear() {
    let (live, id, rev) = parked();
    // The lane has a session too, so a tracked seat exists even though u1 is parked.
    let run = live.handle().info.run_id.clone();
    let rev = rev.to_string();

    // Control: the same chain, with the shell not registered as a seat, claims.
    let (rc, err) = cli_below_a_shell(&live, false, &["claim", "--holder", "ann@host"]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(claim_count(&live), 1);
    let before_token = read_token(&live);

    for args in [
        vec!["claim", "--holder", "seat"],
        vec!["answer", &id, "--rev", &rev, "--text", "go on"],
        vec!["clear", &run, "u1"],
    ] {
        let (rc, err) = cli_below_a_shell(&live, true, &args);
        assert_eq!(rc, 1, "{args:?}");
        assert!(
            err.contains(&format!("{} refused", args[0]))
                && err.contains("descends from, seat process"),
            "{args:?}: {err}"
        );
    }
    assert_eq!(claim_count(&live), 1, "the seat made no claim");
    assert_eq!(
        read_token(&live),
        before_token,
        "the token file is as it was"
    );
    assert_eq!(open_requests(&live), 1, "nothing was answered or cleared");
    assert_eq!(live.unit_status("u1"), "parked");

    // The same answer from a plain caller is accepted: the refusal was the process tree.
    ok(&live.cli(&["answer", &id, "--rev", &rev, "--text", "go on"]));
    assert_eq!(live.unit_status("u1"), "running");
}

// ------------------------------------------------------------------ fencing and tokens

#[test]
fn a_second_claim_fences_the_first() {
    let (live, id, rev) = parked();
    let rev = rev.to_string();
    ok(&live.cli(&["claim", "--holder", "ann@host"]));
    let first = live.sb.root.path().join("first-token");
    std::fs::copy(token_path(&live), &first).unwrap();
    ok(&live.cli(&["claim", "--holder", "bob@host"]));
    assert_eq!(read_token(&live).len(), 64);
    assert_ne!(
        std::fs::read_to_string(&first).unwrap().trim(),
        read_token(&live)
    );

    let out = live.cli(&[
        "answer",
        &id,
        "--rev",
        &rev,
        "--text",
        "old",
        "--claim-token-file",
        first.to_str().unwrap(),
    ]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("claim 1 was fenced by claim 2"),
        "{}",
        stderr(&out)
    );
    assert_eq!(open_requests(&live), 1, "the fenced token answered nothing");

    // The clear is fenced the same way.
    let run = live.handle().info.run_id.clone();
    let out = live.cli(&[
        "clear",
        &run,
        "u1",
        "--claim-token-file",
        first.to_str().unwrap(),
    ]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("claim 1 was fenced by claim 2"),
        "{}",
        stderr(&out)
    );

    // The new token works.
    ok(&live.cli(&["answer", &id, "--rev", &rev, "--text", "new"]));
    assert_eq!(open_requests(&live), 0);
}

#[test]
fn a_wrong_token_is_refused() {
    let (live, id, rev) = parked();
    let rev = rev.to_string();
    ok(&live.cli(&["claim", "--holder", "ann@host"]));
    let wrong = live.sb.root.path().join("wrong-token");
    let guess = "ab".repeat(32);
    std::fs::write(&wrong, format!("{guess}\n")).unwrap();
    let path = wrong.to_str().unwrap();
    let out = live.cli(&[
        "answer",
        &id,
        "--rev",
        &rev,
        "--text",
        "x",
        "--claim-token-file",
        path,
    ]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("matches no claim"),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains(&guess),
        "the refusal repeats the token"
    );

    // A token of the wrong shape is refused too, without being repeated.
    std::fs::write(&wrong, "not-a-token-at-all\n").unwrap();
    let out = live.cli(&[
        "answer",
        &id,
        "--rev",
        &rev,
        "--text",
        "x",
        "--claim-token-file",
        path,
    ]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("not 64 hex digits"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("not-a-token-at-all"));
    assert_eq!(open_requests(&live), 1);
}

#[test]
fn a_missing_token_file_is_refused_naming_the_path() {
    let (live, id, rev) = parked();
    let rev = rev.to_string();
    let run = live.handle().info.run_id.clone();
    // No claim was made: the default file does not exist.
    let default = token_path(&live);
    let out = live.cli(&["answer", &id, "--rev", &rev, "--text", "x"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains(&default.display().to_string()),
        "{}",
        stderr(&out)
    );
    let named = live.sb.root.path().join("nowhere/claim-token");
    let out = live.cli(&[
        "clear",
        &run,
        "u1",
        "--claim-token-file",
        named.to_str().unwrap(),
    ]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains(&named.display().to_string()),
        "{}",
        stderr(&out)
    );
    assert_eq!(open_requests(&live), 1);
}

#[test]
fn the_token_file_is_mode_0600_and_a_new_claim_replaces_it() {
    let live = Live::start();
    live.arm();
    ok(&live.cli(&["claim", "--holder", "ann@host"]));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&token_path(&live)), 0o600);
    let first = read_token(&live);
    // A file left behind with wide permissions is replaced, and the new file is 0600.
    std::fs::set_permissions(token_path(&live), std::fs::Permissions::from_mode(0o644)).unwrap();
    ok(&live.cli(&["claim", "--holder", "bob@host"]));
    assert_eq!(mode(&token_path(&live)), 0o600);
    assert_ne!(read_token(&live), first);
    // The claim prints the generation and the path, never the token.
    let out = live.cli(&["claim", "--holder", "cy@host"]);
    ok(&out);
    let printed = stdout(&out);
    assert!(printed.contains("claim 3"), "{printed}");
    assert!(printed.contains(&token_path(&live).display().to_string()));
    assert!(!printed.contains(&read_token(&live)));
    assert_eq!(
        live.sb
            .state
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("claim-token"))
            .count(),
        1,
        "no temporary token file is left"
    );
}

// ------------------------------------------------------------------ no secret in any output

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn the_token_text_appears_in_no_log_ledger_status_request_or_error() {
    let (live, id, rev) = parked();
    let rev = rev.to_string();
    let run = live.handle().info.run_id.clone();
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut keep = |what: &str, out: &std::process::Output| {
        seen.push((format!("{what} stdout"), stdout(out)));
        seen.push((format!("{what} stderr"), stderr(out)));
    };

    keep("claim", &live.cli(&["claim", "--holder", "ann@host"]));
    keep(
        "claim --json",
        &live.cli(&["claim", "--holder", "bob@host", "--json"]),
    );
    let token = read_token(&live);
    // Failures that carry the token in their input.
    let fenced = live.sb.root.path().join("fenced");
    std::fs::write(&fenced, format!("{}\n", "cd".repeat(32))).unwrap();
    for args in [
        vec!["answer", &id, "--rev", "9", "--text", "wrong revision"],
        vec![
            "answer",
            &id,
            "--rev",
            &rev,
            "--text",
            "x",
            "--claim-token-file",
            fenced.to_str().unwrap(),
        ],
        vec!["clear", &run, "nope"],
        vec!["clear", &run, "u1", "--to", "no-such-stage"],
    ] {
        keep(&args.join(" "), &live.cli(&args));
    }
    keep("status --json", &live.cli(&["status", "--json"]));
    keep("status", &live.cli(&["status"]));
    keep(
        "requests --all --json",
        &live.cli(&["requests", "--all", "--json"]),
    );
    keep("requests --all", &live.cli(&["requests", "--all"]));
    keep("show --json", &live.cli(&["show", &id, "--json"]));
    keep("show", &live.cli(&["show", &id]));
    keep(
        "answer",
        &live.cli(&["answer", &id, "--rev", &rev, "--text", "the ruling"]),
    );
    keep(
        "requests --all --json after",
        &live.cli(&["requests", "--all", "--json"]),
    );
    keep("show --json after", &live.cli(&["show", &id, "--json"]));

    for (what, text) in &seen {
        assert!(
            !text.contains(&token),
            "the token is in the output of {what}"
        );
    }
    let mut files = Vec::new();
    files_under(live.sb.root.path(), &mut files);
    assert!(files.len() > 5);
    let mut checked = 0;
    for file in files {
        if file == token_path(&live) {
            continue;
        }
        let bytes = std::fs::read(&file).unwrap_or_default();
        let found = bytes.windows(token.len()).any(|w| w == token.as_bytes());
        assert!(!found, "the token is in {}", file.display());
        checked += 1;
    }
    assert!(checked > 5, "files searched: {checked}");
    // The ledger names the claim and the answer, with no secret.
    let ledger = live.sb.read_ledger();
    assert!(ledger.contains("claim 2 by bob@host"), "{ledger}");
    assert!(
        ledger.contains(&format!("answer {id}@{rev} by claim 2: the ruling")),
        "{ledger}"
    );
}

// ------------------------------------------------------------------ the proc file system

#[test]
fn a_proc_read_failure_refuses() {
    // An empty directory stands for /proc: the caller's environ cannot be read.
    let empty = tempfile::tempdir().unwrap();
    let root = empty.path().to_path_buf();
    let live = Live::start_config(BIN.into(), move |cfg| cfg.proc_root = root);
    live.arm();
    let out = live.cli(&["claim", "--holder", "ann@host"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(
        err.contains("claim refused") && err.contains("cannot read"),
        "{err}"
    );
    assert_eq!(claim_count(&live), 0);
    assert!(!token_path(&live).exists());
}

#[test]
fn a_proc_that_gives_the_environment_but_not_the_parent_refuses() {
    // The caller's environ reads (and holds no seat token), but its stat does not: the parent
    // chain cannot be walked, so the controller cannot tell, and it refuses. The caller is this
    // process, so its PID is known and the stand-in proc holds only that entry.
    let fake = tempfile::tempdir().unwrap();
    let root = fake.path().to_path_buf();
    let live = Live::start_config(BIN.into(), {
        let root = root.clone();
        move |cfg| cfg.proc_root = root
    });
    live.arm();
    let me = root.join(std::process::id().to_string());
    std::fs::create_dir_all(&me).unwrap();
    std::fs::write(me.join("environ"), b"HOME=/x\0").unwrap();
    let resp = pf_ipc::call(&live.sb.state, "claim", json!({ "holder": "ann@host" })).unwrap();
    assert!(!resp.ok);
    let why = resp.error.unwrap();
    assert!(
        why.contains("claim refused") && why.contains("stat"),
        "{why}"
    );
    assert_eq!(claim_count(&live), 0);

    // With its stat line too, and a parent that is the first process, the same caller is cleared.
    std::fs::write(me.join("stat"), b"1 (test) S 1 1 1 0 0\n").unwrap();
    let resp = pf_ipc::call(&live.sb.state, "claim", json!({ "holder": "ann@host" })).unwrap();
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(claim_count(&live), 1);
}
