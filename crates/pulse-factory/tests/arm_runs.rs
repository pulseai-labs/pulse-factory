//! AC-3: `arm` shows a summary, starts a run only with `--yes`, and records it through the
//! controller. The forge is a stub `gh`; the seats are never launched (the controller ticks once
//! an hour here).

mod common;

use common::ctl::Live;
use common::*;
use serde_json::Value;

const HOUR_MS: u64 = 3_600_000;

fn json_summary(sb: &Sandbox, extra: &[&str]) -> Value {
    let mut args = sb.arm_args();
    args.extend(["--dry-run".into(), "--json".into()]);
    args.extend(extra.iter().map(|s| (*s).to_string()));
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    serde_json::from_str(&stdout(&out)).unwrap()
}

#[test]
fn the_summary_holds_every_field() {
    let sb = Sandbox::new();
    let out = {
        let mut args = sb.arm_args();
        args.push("--dry-run".into());
        sb.run(&args)
    };
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    for want in [
        "spine: s1",
        "items: 2",
        "workflow: fixture-spine (file version 1)",
        "role lane: runtime omp, model model-a",
        "role implementer: runtime omp, model model-b",
        "role verifier: runtime omp, model model-c",
        "role close: runtime omp, model model-d",
        "caps: model_sessions 2, cold_builds 1",
        "caps: seed lane runs at most 3 times",
        "caps: one relaunch per stage",
        "plugin methodkit 1.26.1",
        "most launches: 18",
        "item PRs merge into spine/s1 on green checks with no ack",
        "forge: acme/widgets (default branch main)",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(!text.contains("replaces:"));
    assert!(!text.contains("paper env"));

    let v = json_summary(&sb, &[]);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["items"], 2);
    assert_eq!(v["most_launches"], 18);
    assert_eq!(v["caps"]["seeds"][0]["max"], 3);
}

#[test]
fn most_launches_follows_the_formula() {
    // 2 x (seed max 2 x 1 lane stage + 4 items x 3 item stages) = 28
    let o = Options {
        seed_max: 2,
        ..Options::default()
    };
    let sb = Sandbox::with(&o);
    let mut args = sb.arm_args();
    for item in args.iter_mut() {
        if item == "count=2" {
            *item = "count=4".into();
        }
    }
    args.extend(["--dry-run".into(), "--json".into()]);
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["items"], 4);
    assert_eq!(v["most_launches"], 28);
}

#[test]
fn with_no_flag_arm_exits_2_and_records_nothing() {
    let sb = Sandbox::new();
    let out = sb.run(&sb.arm_args());
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("re-run with --yes to start"),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("most launches"),
        "the summary is shown first"
    );
    assert!(!sb.state.exists(), "no state directory may be made");
}

#[test]
fn dry_run_exits_0_records_nothing_and_needs_no_controller() {
    let sb = Sandbox::new();
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("skipped"),
        "it must say the earlier-run checks were skipped"
    );
    assert!(!sb.state.exists());
    assert!(!sb.ledger.exists());
}

#[test]
fn forge_facts_come_from_gh_for_each_origin_form() {
    let sb = Sandbox::new();
    sb.gh_answers("Acme/Widgets-Renamed", "trunk");
    for origin in [
        "https://github.com/acme/widgets.git",
        "https://user:pw@github.com/acme/widgets/",
        "ssh://git@ssh.github.com:443/acme/widgets.git",
        "ssh://git@github.com:22/ACME/Widgets",
        "git@github.com:acme/widgets.git",
    ] {
        sb.set_origin(origin);
        let v = json_summary(&sb, &[]);
        assert_eq!(v["forge_repo"], "Acme/Widgets-Renamed", "origin {origin}");
        assert_eq!(v["default_branch"], "trunk", "origin {origin}");
    }
    let calls = sb.gh_calls();
    assert!(
        calls
            .iter()
            .all(|c| c.contains("repo view") && c.contains("nameWithOwner,defaultBranchRef")),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.contains("ACME/Widgets")),
        "gh gets the name as the origin spells it: {calls:?}"
    );
}

#[test]
fn yes_starts_a_stored_run_and_a_second_yes_is_refused() {
    let sb = Sandbox::new();
    let _ctl = Ctl::start(&sb, HOUR_MS);
    let mut args = sb.arm_args();
    args.push("--yes".into());
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("run: s1-1"), "{}", stdout(&out));

    let v = sb.status_json("s1-1");
    assert_eq!(v["state"], "active");
    assert_eq!(v["env"], "live");
    let run = sb.read_json("runs/s1-1/run.json");
    assert_eq!(run["forge_repo"], "acme/widgets");
    assert_eq!(run["default_branch"], "main");
    assert_eq!(run["params"]["spine"], "s1");
    assert_eq!(
        run["params"]["briefs"],
        sb.state
            .join("runs/s1-1/package/briefs")
            .display()
            .to_string()
    );

    // The manifest pins every file of the package and of the plugin copy.
    let manifest = sb.read_json("runs/s1-1/manifest.json");
    let mut package_files: Vec<String> = manifest["package"]["files"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    package_files.sort();
    assert_eq!(
        package_files,
        vec![
            "briefs/common.md",
            "rules/close.yaml",
            "rules/implementer.yaml",
            "rules/lane.yaml",
            "rules/verifier.yaml",
            "workflow.yaml"
        ]
    );
    let plugin_files: Vec<String> = manifest["plugins"][0]["files"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert!(
        plugin_files.contains(&"SKILL.md".to_string()),
        "{plugin_files:?}"
    );
    assert!(
        plugin_files.contains(&".claude-plugin/plugin.json".to_string()),
        "{plugin_files:?}"
    );
    assert!(
        sb.state
            .join("runs/s1-1/plugins/methodkit/SKILL.md")
            .is_file()
    );
    for d in ["queue", "out", "package"] {
        assert!(sb.state.join("runs/s1-1").join(d).is_dir(), "{d}");
    }

    // The state directory holds the socket, the lock, the store and the runs.
    let mut again = sb.arm_args();
    again.push("--yes".into());
    let out = sb.run(&again);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("s1-1"), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("not at an end state"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn replace_ends_the_first_run_and_a_finished_run_blocks_nothing() {
    let sb = Sandbox::new();
    let _ctl = Ctl::start(&sb, HOUR_MS);
    let arm = |extra: &[&str]| {
        let mut args = sb.arm_args();
        args.extend(extra.iter().map(|s| (*s).to_string()));
        sb.run(&args)
    };
    assert_eq!(code(&arm(&["--yes"])), 0);

    // The summary names the run it replaces.
    let preview = arm(&["--replace", "s1-1", "--dry-run"]);
    assert_eq!(code(&preview), 0, "{}", stderr(&preview));
    assert!(
        stdout(&preview).contains("replaces: s1-1"),
        "{}",
        stdout(&preview)
    );

    let out = arm(&["--replace", "s1-1", "--yes"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("run: s1-2"), "{}", stdout(&out));
    let first = sb.status_json("s1-1");
    assert_eq!(first["state"], "ended");
    assert_eq!(first["end_reason"], "replaced by s1-2");
    assert_eq!(sb.status_json("s1-2")["state"], "active");
    assert_eq!(
        sb.status_json("")["run"],
        "s1-2",
        "the latest run is the default"
    );

    // The ended run blocks nothing: the refusal names only the active run.
    let out = arm(&["--yes"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("s1-2") && !stderr(&out).contains("s1-1"),
        "{}",
        stderr(&out)
    );
    // And it keeps its reason after a third run replaces the second.
    assert_eq!(code(&arm(&["--replace", "s1-2", "--yes"])), 0);
    assert_eq!(sb.status_json("s1-1")["end_reason"], "replaced by s1-2");
}

#[test]
fn a_failed_pin_leaves_the_first_run_as_it_was() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let _ctl = Ctl::start(&sb, HOUR_MS);
    let mut args = sb.arm_args();
    args.push("--yes".into());
    assert_eq!(code(&sb.run(&args)), 0);
    let before = sb.status_json("s1-1");

    // The plugin cannot be copied now: the new run fails before it replaces the first.
    let secret = sb.plugin_dir("methodkit", "1.26.1").join("SKILL.md");
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut args = sb.arm_args();
    args.extend(["--replace".into(), "s1-1".into(), "--yes".into()]);
    let out = sb.run(&args);
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(code(&out), 1, "{}", stdout(&out));
    assert!(stderr(&out).contains("arm refused"), "{}", stderr(&out));

    let after = sb.status_json("s1-1");
    assert_eq!(after["state"], "active");
    assert_eq!(after["end_reason"], Value::Null);
    assert_eq!(before["units"], after["units"]);
    assert!(
        !sb.state.join("runs/s1-2").exists(),
        "the failed run's directory must go"
    );
}

#[test]
fn an_arm_that_fails_late_leaves_no_run_to_collide_with_and_the_old_run_as_it_was() {
    use std::sync::atomic::Ordering;
    let live = Live::start();
    let first = live.arm();
    assert_eq!(first, "s1-1");

    // The record of the new run cannot be written. The engine has started and committed by now.
    // The run it replaces must stay as it was.
    live.ports.block_run_json.store(true, Ordering::SeqCst);
    let mut plan = live.plan();
    plan.replace = Some(first.clone());
    let err = live.controller.arm(plan).unwrap_err();
    assert!(err.contains("run.json"), "{err}");
    assert!(!live.sb.state.join("runs/s1-2").exists());
    let old = live.sb.status_json(&first);
    assert_eq!(old["state"], "active", "{old}");
    assert_eq!(old["end_reason"], Value::Null);

    // A retry takes a new ID, because the failed run's rows are still in the store. The same ID
    // with another ledger would be a conflict, and the spine could not be armed again.
    live.ports.block_run_json.store(false, Ordering::SeqCst);
    let mut plan = live.plan();
    plan.replace = Some(first.clone());
    plan.ledger = live.sb.root.path().join("other-ledger.md");
    let second = live.controller.arm(plan).unwrap();
    assert_eq!(second, "s1-3");
    assert_eq!(live.sb.status_json(&first)["state"], "ended");
}

#[test]
fn an_unwritable_ledger_refuses_the_arm_before_the_store_commits_anything() {
    let live = Live::start();
    let mut plan = live.plan();
    plan.ledger = live.sb.root.path().join("no-such-dir/ledger.md");
    let err = live.controller.arm(plan).unwrap_err();
    assert!(err.contains("cannot be written"), "{err}");
    assert!(
        std::fs::read_dir(live.sb.state.join("runs"))
            .map(|d| d.count())
            .unwrap_or(0)
            == 0,
        "the failed arm must leave no directory"
    );
    // The first number is still free: nothing was committed under it.
    assert_eq!(live.arm(), "s1-1");
}

#[test]
fn replace_ends_a_parked_run() {
    // A change to a copy parks the lane at its first launch, with no seat started. A parked run
    // is not at an end state, so it blocks `arm` until `--replace` names it, and then it ends.
    let live = Live::start();
    let first = live.arm();
    std::fs::write(
        live.run_dir().join("plugins/methodkit/SKILL.md"),
        "changed copy\n",
    )
    .unwrap();
    live.controller.tick_all();
    let view = live.handle().engine.view().unwrap();
    assert_eq!(
        view.node("lane").unwrap().status,
        pf_engine::status::Status::Parked
    );
    assert!(
        live.ports.plans.lock().unwrap().is_empty(),
        "nothing launched"
    );

    // Without `--replace` the parked run blocks a second one, and the refusal names it.
    let err = live.controller.arm(live.plan()).unwrap_err();
    assert!(
        err.contains(&first) && err.contains("not at an end state"),
        "{err}"
    );

    let mut plan = live.plan();
    plan.replace = Some(first.clone());
    let second = live.controller.arm(plan).unwrap();
    assert_ne!(first, second);
    let old = live.sb.status_json(&first);
    assert_eq!(old["state"], "ended");
    assert_eq!(old["end_reason"], format!("replaced by {second}"));
    assert_eq!(live.sb.status_json(&second)["state"], "active");
}

#[test]
fn a_changed_source_file_and_a_new_source_file_reach_no_launch_and_park_nothing() {
    let live = Live::start();
    live.arm();
    let source = live.sb.plugin_dir("methodkit", "1.26.1");
    std::fs::write(source.join("SKILL.md"), "the source changed after arm\n").unwrap();
    std::fs::write(source.join("NEW.md"), "a new file in the source\n").unwrap();
    std::fs::write(
        live.sb.package.join("briefs/common.md"),
        "the source brief changed\n",
    )
    .unwrap();
    std::fs::write(live.sb.package.join("briefs/new.md"), "new brief\n").unwrap();
    live.controller.tick_all();

    // The lane launched, from the run's copies, and nothing parked.
    let view = live.handle().engine.view().unwrap();
    assert_eq!(
        view.node("lane").unwrap().status,
        pf_engine::status::Status::Running
    );
    let plan = live.last_plan();
    let run_dir = live.run_dir();
    let plugins = plan.settings.get("plugins").cloned().unwrap_or_default();
    assert!(
        plugins.contains(&run_dir.join("plugins/methodkit").display().to_string()),
        "{plugins}"
    );
    assert!(
        !plugins.contains(&source.display().to_string()),
        "the source plugin must not reach a seat: {plugins}"
    );
    // The copies are as they were at arm.
    assert_eq!(
        std::fs::read_to_string(run_dir.join("plugins/methodkit/SKILL.md")).unwrap(),
        "skill text\n"
    );
    assert!(!run_dir.join("plugins/methodkit/NEW.md").exists());
    assert_eq!(
        std::fs::read_to_string(run_dir.join("package/briefs/common.md")).unwrap(),
        "# Common\nDo the work.\n"
    );
    assert!(!run_dir.join("package/briefs/new.md").exists());
}
