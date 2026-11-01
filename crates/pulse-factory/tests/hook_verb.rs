//! AC-6: the `hook` verb, end to end. The test runs `$PF_HOOK_CMD` through `/bin/sh -c`, as the
//! extension does, against a live controller and the shipped rule files of the fixture package.

mod common;

use common::ctl::*;
use common::*;
use serde_json::json;

fn seat(live: &Live, role: &str, unit: &str) -> pf_engine::port::agent::LaunchPlan {
    let settings = live.role_settings(role, unit);
    live.launch(role, unit, &settings).unwrap()
}

fn bash(
    live: &Live,
    plan: &pf_engine::port::agent::LaunchPlan,
    command: &str,
) -> serde_json::Value {
    let _ = live;
    let (out, code) = run_hook(plan, &call("bash", json!({ "command": command })), |_| {});
    assert_eq!(code, 0, "{out}");
    decision(&out)
}

#[test]
fn an_allowed_call_answers_allow_for_every_role() {
    let live = Live::start();
    live.arm();
    for (i, role) in ["lane", "implementer", "verifier", "close"]
        .iter()
        .enumerate()
    {
        let plan = seat(&live, role, &format!("u{i}"));
        let d = bash(&live, &plan, "ls -la && cat Cargo.toml");
        assert_eq!(d["decision"], "allow", "{role}: {d}");
    }
}

#[test]
fn the_landed_denies_hold_for_every_role() {
    let live = Live::start();
    live.arm();
    let denied = [
        "gh pr merge 1",
        "git push --force origin work/u0",
        "git push origin main",
        "git push origin --delete work/u0",
        "git commit --no-verify -m x",
        "git commit -m x -m 'Co-Authored-By: Claude <noreply@anthropic.com>'",
    ];
    for (i, role) in ["lane", "implementer", "verifier", "close"]
        .iter()
        .enumerate()
    {
        let plan = seat(&live, role, &format!("u{i}"));
        for command in denied {
            let d = bash(&live, &plan, command);
            assert_eq!(d["decision"], "deny", "{role}: {command}: {d}");
            assert!(
                !d["reason"].as_str().unwrap_or("").is_empty(),
                "{role}: {command}: the deny has a reason"
            );
        }
    }
}

#[test]
fn close_may_push_its_own_branch_and_implementer_may_not() {
    let live = Live::start();
    live.arm();
    let close = seat(&live, "close", "u1");
    // The seat works from the method's root, which is not its worktree. A push is judged on the
    // directory it runs in (`r0.s1.w5`), so the seat names its worktree.
    let wt = live.sb.spine_dir.join("wt-u1");
    let d = bash(
        &live,
        &close,
        &format!("git -C {} push origin work/u1", wt.display()),
    );
    assert_eq!(d["decision"], "allow", "{d}");
    let d = bash(&live, &close, "git push origin work/u1");
    assert_eq!(d["decision"], "deny", "from the method's root: {d}");
    let implementer = seat(&live, "implementer", "u2");
    let d = bash(&live, &implementer, "git push origin work/u2");
    assert_eq!(d["decision"], "deny", "{d}");
}

#[test]
fn a_lane_seat_with_no_worktree_may_run_an_ordinary_call() {
    let live = Live::start();
    live.arm();
    let plan = seat(&live, "lane", "lane");
    assert_eq!(plan.settings["worktree"], "");
    assert_eq!(
        bash(&live, &plan, "echo hello > /dev/null")["decision"],
        "allow"
    );
}

#[test]
fn write_tools_are_judged_on_their_path() {
    let live = Live::start();
    live.arm();
    let plan = seat(&live, "implementer", "u1");
    let wt = live.sb.spine_dir.join("wt-u1");
    let inside = wt.join("src/lib.rs").display().to_string();
    let report = live
        .sb
        .spine_dir
        .join("work-u1/report.md")
        .display()
        .to_string();
    let run = |tool: &str, input: serde_json::Value| {
        let (out, code) = run_hook(&plan, &call(tool, input), |_| {});
        assert_eq!(code, 0, "{out}");
        decision(&out)
    };
    assert_eq!(
        run("write", json!({ "path": inside, "content": "x" }))["decision"],
        "allow"
    );
    assert_eq!(
        run("write", json!({ "path": report, "content": "x" }))["decision"],
        "allow"
    );
    assert_eq!(
        run("write", json!({ "path": "/etc/passwd", "content": "x" }))["decision"],
        "deny"
    );
    assert_eq!(
        run("write", json!({ "content": "x" }))["decision"],
        "deny",
        "no path fails closed"
    );
    // `edit` in OMP's `replace` shape carries its path as `path`; other tools use `file_path`.
    assert_eq!(
        run(
            "edit",
            json!({ "path": inside, "old_text": "a", "new_text": "b" })
        )["decision"],
        "allow"
    );
    assert_eq!(
        run(
            "edit",
            json!({ "path": "/etc/hosts", "old_text": "a", "new_text": "b" })
        )["decision"],
        "deny"
    );
    assert_eq!(
        run("edit", json!({ "file_path": inside }))["decision"],
        "allow"
    );
    assert_eq!(
        run("edit", json!({ "edits": [] }))["decision"],
        "deny",
        "the hashline shape has no path"
    );
    // A tool the rules do not limit is allowed.
    assert_eq!(
        run("read", json!({ "path": "/etc/hosts" }))["decision"],
        "allow"
    );
}

#[test]
fn a_write_call_with_two_mib_of_content_is_answered() {
    let live = Live::start();
    live.arm();
    let plan = seat(&live, "implementer", "u1");
    let path = live
        .sb
        .spine_dir
        .join("wt-u1/big.txt")
        .display()
        .to_string();
    let content = "x".repeat(2 << 20);
    let (out, code) = run_hook(
        &plan,
        &call("write", json!({ "path": path, "content": content })),
        |_| {},
    );
    assert_eq!(code, 0);
    assert_eq!(decision(&out)["decision"], "allow", "{out}");
}

#[test]
fn every_failure_fails_closed_to_deny_with_exit_0() {
    let live = Live::start();
    live.arm();
    let plan = seat(&live, "implementer", "u1");
    let ok = call("bash", json!({ "command": "ls" }));
    // No key.
    let (out, code) = run_hook(&plan, &ok, |e| {
        e.remove("PF_SESSION_KEY");
    });
    assert_eq!(
        (code, decision(&out)["decision"].clone()),
        (0, json!("deny")),
        "{out}"
    );
    // An unknown key.
    let (out, code) = run_hook(&plan, &ok, |e| {
        e.insert("PF_SESSION_KEY".into(), "pfk_unknown".into());
    });
    assert_eq!(
        (code, decision(&out)["decision"].clone()),
        (0, json!("deny")),
        "{out}"
    );
    // No controller.
    let empty = tempfile::tempdir().unwrap();
    let (out, code) = run_hook(&plan, &ok, |e| {
        e.insert("PF_STATE_DIR".into(), empty.path().display().to_string());
    });
    assert_eq!(
        (code, decision(&out)["decision"].clone()),
        (0, json!("deny")),
        "{out}"
    );
    assert!(
        decision(&out)["reason"]
            .as_str()
            .unwrap()
            .contains("controller"),
        "{out}"
    );
    // Malformed stdin, and stdin that is JSON of the wrong shape.
    for bad in [
        &b"this is not json"[..],
        b"[1,2]",
        b"",
        b"{\"tool\":\"bash\",\"input\":{}}",
    ] {
        let (out, code) = run_hook(&plan, bad, |_| {});
        assert_eq!(
            (code, decision(&out)["decision"].clone()),
            (0, json!("deny")),
            "{bad:?}: {out}"
        );
    }
    // And with no PF_HOOK_CMD the extension itself denies; the controller never sees it.
}

#[test]
fn a_hook_call_is_answered_while_a_tick_is_blocked() {
    let live = Live::start();
    live.arm();
    let plan = seat(&live, "implementer", "u1");
    live.ports.gate.close();
    let controller = live.controller.clone();
    let ticker = std::thread::spawn(move || controller.tick_all());
    live.ports.gate.wait_for_holder();
    let started = std::time::Instant::now();
    let d = bash(&live, &plan, "ls");
    assert_eq!(d["decision"], "allow");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "the answer waited on the tick"
    );
    assert!(!ticker.is_finished(), "the tick is still blocked");
    live.ports.gate.open();
    ticker.join().unwrap();
}

#[test]
fn each_decision_is_one_audit_line_and_no_secret_stays() {
    let live = Live::start();
    live.arm();
    let plan = seat(&live, "implementer", "u1");
    let key = plan.settings["env.PF_SESSION_KEY"].clone();
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    for command in ["ls", "gh pr merge 1", &format!("echo {token} {key}")] {
        let (out, code) = run_hook(&plan, &call("bash", json!({ "command": command })), |_| {});
        assert_eq!(code, 0, "{out}");
        assert!(
            !out.contains(&key),
            "the key must not reach the seat: {out}"
        );
    }
    let log = std::fs::read_to_string(live.run_dir().join("out/u1/audit.log")).unwrap();
    assert_eq!(log.lines().count(), 3, "{log}");
    assert!(log.contains("outcome=deny"), "{log}");
    assert!(!log.contains(token), "{log}");
    assert!(!log.contains(&key), "{log}");
    assert!(log.contains("[redacted]"), "{log}");
}

#[test]
fn a_binary_path_with_a_space_still_works() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = dir.path().join("a dir with spaces");
    std::fs::create_dir_all(&spaced).unwrap();
    let copy = spaced.join("pulse factory");
    std::fs::copy(BIN, &copy).unwrap();
    let live = Live::start_with(copy.clone());
    live.arm();
    let plan = seat(&live, "implementer", "u1");
    assert!(plan.settings["env.PF_HOOK_CMD"].contains("a dir with spaces"));
    let d = bash(&live, &plan, "ls");
    assert_eq!(d["decision"], "allow", "{d}");
}
