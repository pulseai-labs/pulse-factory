//! AC-25 of `r1.s4.w4`: a chat or a seat is refused on a settings write, and the refusal is audited
//! in coalesced lines (rulings 6 and 8). The controller runs in the test process on a `FakeChat`.
//! A caller is a real shell whose PID the test registers as the chat's (or a seat's) process, and
//! the CLI runs below it as child processes, so the controller's own walk of `/proc` finds them.
//! Nothing here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::chat::*;
use common::ctl::Live;
use common::{BIN, code, stderr};
use pf_engine::port::store::{AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, Store};
use serde_json::Value;

const CAP: &str = "chat.self_turn_cap";
const DENY: &str = "chat.deny_commands";
const TOKEN: &str = "tok-SECRET-0123456789";

/// The window of the burst tests. It is far longer than the 300 ms the spec names, because the burst
/// starts a hundred real processes and must fit inside one window to be counted as one. The tests
/// never time the burst: the window only has to be much longer than a burst on a loaded host.
const BURST_WINDOW: Duration = Duration::from_secs(12);

/// The most the tests wait, past the window, for the controller's tick to write the close lines.
const CLOSE_BOUND: Duration = Duration::from_secs(30);

/// A live controller with a chat runtime, a refusal window of `window`, and one live chat `S-1`.
fn live(window: Duration) -> (Live, Arc<Rec>) {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), move |cfg| cfg.refusal_window = window);
    let (_, s) = started(&live, "alpha");
    assert_eq!(s, "S-1");
    (live, rec)
}

/// Who is registered as the process of the chat while a closure runs.
enum As {
    Chat,
    Seat,
    Nobody,
}

/// Run `body` (shell text, with `$BIN` the CLI) in a shell that the controller takes to be `who`.
/// The shell waits for a go file, so its PID is registered before the CLI runs. It answers the
/// shell's exit code and what the shell wrote on stderr (`$DIR/err`).
fn below(live: &Live, rec: &Rec, who: As, env: &[(&str, &str)], body: &str) -> (i32, String) {
    let dir = live.sb.root.path().join("below");
    std::fs::create_dir_all(&dir).unwrap();
    let go = dir.join("go");
    let _ = std::fs::remove_file(&go);
    let _ = std::fs::remove_file(dir.join("err"));
    let script_file = dir.join("body.sh");
    std::fs::write(&script_file, body).unwrap();
    let script = r#"exec 2>"$DIR/err"
while [ ! -e "$GO" ]; do sleep 0.02; done
. "$BODYFILE"
"#;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
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
        .env("BODYFILE", &script_file);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    match who {
        As::Chat => {
            let chat = rec.chats.lock().unwrap()[0].clone();
            rec.pids.lock().unwrap().insert(chat, child.id());
        }
        As::Seat => {
            let refs = live.session_refs();
            assert!(!refs.is_empty(), "a tracked seat needs a session row");
            live.ports
                .pids
                .lock()
                .unwrap()
                .insert(refs[0].0.clone(), child.id());
        }
        As::Nobody => {}
    }
    SHELL_PID.with(|p| p.set(child.id()));
    std::fs::write(&go, "").unwrap();
    let status = child.wait().unwrap();
    rec.pids.lock().unwrap().clear();
    live.ports.pids.lock().unwrap().clear();
    let err = std::fs::read_to_string(dir.join("err")).unwrap_or_default();
    (status.code().unwrap_or(-1), err)
}

thread_local! {
    /// The PID of the shell the last `below` ran.
    static SHELL_PID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn shell_pid() -> u32 {
    SHELL_PID.with(std::cell::Cell::get)
}

/// One CLI call below the shell, answering its exit code and its stderr.
fn one(live: &Live, rec: &Rec, who: As, env: &[(&str, &str)], args: &str) -> (i32, String) {
    let body = format!("\"$BIN\" {args} 2>\"$DIR/cli.err\"\necho $? >\"$DIR/rc\"\n");
    below(live, rec, who, env, &body);
    let dir = live.sb.root.path().join("below");
    let rc = std::fs::read_to_string(dir.join("rc")).unwrap();
    let err = std::fs::read_to_string(dir.join("cli.err")).unwrap_or_default();
    (rc.trim().parse().unwrap(), err)
}

fn refused_lines(live: &Live) -> Vec<String> {
    controller_ledger(live)
        .into_iter()
        .filter(|l| l.contains("caller-refused-"))
        .collect()
}

fn refused_admissions(live: &Live) -> Vec<AdmissionRow> {
    live.controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .into_iter()
        .filter(|a| a.command_id.starts_with("caller-refused-"))
        .collect()
}

fn setting_rows(live: &Live) -> usize {
    live.controller.store().setting_rows().unwrap().len()
}

/// The sum of the `refused=<n>` fields of some lines.
fn refused_sum(lines: &[String]) -> u64 {
    lines
        .iter()
        .map(|l| {
            let at = l.find(" refused=").expect("a refused= field") + " refused=".len();
            l[at..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .sum()
}

#[test]
fn a_descendant_of_a_chat_is_refused_on_set_and_unset_and_audited_once() {
    let (live, rec) = live(Duration::from_secs(60));
    let rows = setting_rows(&live);

    let (rc, err) = one(
        &live,
        &rec,
        As::Chat,
        &[],
        &format!("settings set {CAP} 50 --command-id chat-cmd-1"),
    );
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("chat process"), "{err}");
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    let line = &lines[0];
    assert!(
        line.contains(&format!(
            "setting_set refused caller=chat:S-1 bucket={CAP} refused=1 reason=\""
        )),
        "{line}"
    );
    assert!(line.contains("chat process"), "{line}");
    assert!(line.contains(" command=caller-refused-"), "{line}");
    let admissions = refused_admissions(&live);
    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0].outcome, AdmissionOutcome::Refused);
    assert_eq!(admissions[0].kind, "setting_set");
    assert!(line.ends_with(&format!("command={}", admissions[0].command_id)));
    assert_eq!(setting_rows(&live), rows, "no row changes");

    // The same for an unset. It is its own bucket's first refusal only when the key differs, so it
    // names another key, and gets its own line.
    let (rc, err) = one(
        &live,
        &rec,
        As::Chat,
        &[],
        &format!("settings unset {DENY} --command-id chat-cmd-2"),
    );
    assert_eq!(rc, 1, "{err}");
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[1].contains(&format!(
            "setting_unset refused caller=chat:S-1 bucket={DENY} refused=1"
        )),
        "{}",
        lines[1]
    );
    assert_eq!(setting_rows(&live), rows);
}

#[test]
fn a_seat_token_caller_and_a_descendant_of_a_tracked_seat_are_refused() {
    let (live, rec) = live(Duration::from_secs(60));
    live.arm_with_units(&["u1"]);
    let rows = setting_rows(&live);

    // The environment holds PF_SEAT_TOKEN: the CLI does not pre-refuse, so the controller does.
    let out = live
        .sb
        .cmd(&["settings", "set", CAP, "50"])
        .env("PF_SEAT_TOKEN", TOKEN)
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(stderr(&out).contains("PF_SEAT_TOKEN"), "{}", stderr(&out));
    assert!(!stderr(&out).contains(TOKEN), "{}", stderr(&out));
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains(&format!(
            "setting_set refused caller=seat-token bucket={CAP} refused=1"
        )),
        "{}",
        lines[0]
    );

    // A process below a tracked seat.
    let (rc, err) = one(
        &live,
        &rec,
        As::Seat,
        &[],
        &format!("settings unset {CAP} --command-id seat-cmd"),
    );
    assert_eq!(rc, 1, "{err}");
    assert!(err.contains("seat process"), "{err}");
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[1].contains(&format!(
            "setting_unset refused caller=seat:{} bucket={CAP} refused=1",
            shell_pid()
        )),
        "{}",
        lines[1]
    );
    assert_eq!(refused_admissions(&live).len(), 2);
    assert_eq!(setting_rows(&live), rows, "no row changes");
    for line in controller_ledger(&live) {
        assert!(!line.contains(TOKEN), "the token is in a line: {line}");
    }
}

#[test]
fn the_refused_callers_command_id_is_not_used_up() {
    let (live, rec) = live(Duration::from_secs(60));
    let (rc, _) = one(
        &live,
        &rec,
        As::Chat,
        &[],
        &format!("settings set {CAP} 50 --command-id taken-by-nobody"),
    );
    assert_eq!(rc, 1);
    assert!(
        refused_admissions(&live)
            .iter()
            .all(|a| a.command_id != "taken-by-nobody"),
        "the audit uses the controller's own ID"
    );
    // The operator uses the same ID with the same content: it admits, it is no replay.
    let out = live.cli(&[
        "settings",
        "set",
        CAP,
        "50",
        "--command-id",
        "taken-by-nobody",
        "--json",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: Value = serde_json::from_str(&common::stdout(&out)).unwrap();
    assert_eq!(v["replayed"], false, "{v}");
    assert_eq!(setting_rows(&live), 1);
}

/// A burst of `n` writes from one shell: set and unset mixed, both known keys and 40 invented ones,
/// `n` distinct command IDs, each its own process. They run in parallel, as a forking chat would.
fn burst_body(n: usize, tag: &str) -> String {
    let mut body = String::new();
    for i in 0..n {
        let (key, value) = match i % 5 {
            0 => (CAP.to_string(), "1".to_string()),
            1 => (DENY.to_string(), "[]".to_string()),
            _ => (format!("x.key{}", i % 40), "1".to_string()),
        };
        let line = if i % 2 == 0 {
            format!("\"$BIN\" settings set {key} '{value}' --command-id {tag}-{i}")
        } else {
            format!("\"$BIN\" settings unset {key} --command-id {tag}-{i}")
        };
        body.push_str(&format!("{line} >/dev/null 2>&1 &\n"));
    }
    body.push_str("wait\n");
    body
}

/// Run the controller's tick, again and again, until `want` refusal lines are in the ledger or
/// `CLOSE_BOUND` has passed. A window opens at its first refusal, which is later than `start` by an
/// amount the host decides, so no fixed wait after `start` is right: the tick closes a window only
/// when the window is old enough, and this loop asks the ledger instead of the clock.
fn close_the_windows(live: &Live, start: Instant, window: Duration, want: usize) {
    let give_up = start + window + CLOSE_BOUND;
    let first = start + window;
    let now = Instant::now();
    if first > now {
        std::thread::sleep(first - now);
    }
    loop {
        live.controller.tick_all();
        if refused_lines(live).len() >= want || Instant::now() >= give_up {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn bucket_lines(lines: &[String], bucket: &str) -> usize {
    lines
        .iter()
        .filter(|l| l.contains(&format!("bucket={bucket} ")))
        .count()
}

#[test]
fn a_burst_from_a_chat_adds_at_most_six_lines_and_the_counts_add_up() {
    let (live, rec) = live(BURST_WINDOW);
    let rows = setting_rows(&live);
    let start = Instant::now();
    below(&live, &rec, As::Chat, &[], &burst_body(100, "burst"));

    // Inside the window: one open line per bucket, and no more.
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 3, "{lines:#?}");
    for bucket in [CAP, DENY, "other"] {
        assert_eq!(bucket_lines(&lines, bucket), 1, "{bucket}: {lines:#?}");
    }
    assert!(
        lines.iter().all(|l| l.contains("caller=chat:S-1")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().all(|l| l.contains(" refused=1 ")),
        "{lines:#?}"
    );
    assert_eq!(refused_admissions(&live).len(), 3);

    // After the window and one tick: one close line per bucket, and the counts add up to 100.
    close_the_windows(&live, start, BURST_WINDOW, 6);
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 6, "{lines:#?}");
    for bucket in [CAP, DENY, "other"] {
        assert_eq!(bucket_lines(&lines, bucket), 2, "{bucket}: {lines:#?}");
    }
    let closes: Vec<&String> = lines.iter().filter(|l| l.contains(" window=")).collect();
    assert_eq!(closes.len(), 3, "{lines:#?}");
    assert_eq!(refused_sum(&lines), 100, "{lines:#?}");
    assert_eq!(refused_admissions(&live).len(), 6);
    assert_eq!(setting_rows(&live), rows, "no row changes");

    // A second burst after the window starts new windows: still at most two lines per bucket each.
    let start = Instant::now();
    below(&live, &rec, As::Chat, &[], &burst_body(30, "again"));
    close_the_windows(&live, start, BURST_WINDOW, 12);
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 12, "{lines:#?}");
    for bucket in [CAP, DENY, "other"] {
        assert_eq!(bucket_lines(&lines, bucket), 4, "{bucket}: {lines:#?}");
    }
    assert_eq!(refused_sum(&lines), 130, "{lines:#?}");
    assert_eq!(setting_rows(&live), rows);
}

#[test]
fn a_burst_from_a_seat_token_caller_is_bounded_the_same_way() {
    let (live, rec) = live(BURST_WINDOW);
    let start = Instant::now();
    below(
        &live,
        &rec,
        As::Nobody,
        &[("PF_SEAT_TOKEN", TOKEN)],
        &burst_body(100, "seat"),
    );
    close_the_windows(&live, start, BURST_WINDOW, 6);
    let lines = refused_lines(&live);
    assert_eq!(lines.len(), 6, "{lines:#?}");
    assert!(
        lines.iter().all(|l| l.contains("caller=seat-token")),
        "{lines:#?}"
    );
    assert_eq!(refused_sum(&lines), 100);
    for line in controller_ledger(&live) {
        assert!(!line.contains(TOKEN), "the token is in a line: {line}");
    }
}

#[test]
fn a_stopped_ledger_still_refuses_the_caller_and_writes_no_line() {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), |cfg| {
        cfg.refusal_window = Duration::from_secs(60);
        // A directory takes the ledger's place, so the controller's commands are stopped.
        std::fs::create_dir(cfg.state_dir.join("controller.ledger")).unwrap();
    });
    let admissions = live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
        .len();
    let out = live
        .sb
        .cmd(&["settings", "set", CAP, "50"])
        .env("PF_SEAT_TOKEN", TOKEN)
        .output()
        .unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(stderr(&out).contains("PF_SEAT_TOKEN"), "{}", stderr(&out));
    assert!(controller_ledger(&live).is_empty(), "no ledger line");
    assert_eq!(
        live.controller
            .store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
            .len(),
        admissions,
        "nothing is admitted while the commands are stopped"
    );
    assert_eq!(setting_rows(&live), 0);
}
