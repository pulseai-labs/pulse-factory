//! AC-2: each refusal of `arm` exits non-zero, names its cause and leaves the state directory as
//! it was. Nothing here spends or reaches the network: `gh` is a stub and the git repo is local.

mod common;

use common::*;
use serde_json::json;

/// Run an arm that must be refused. The message names `cause`, the exit is 1 and the state
/// directory is as it was.
fn refused(sb: &Sandbox, args: &[String], cause: &str) {
    let before = sb.state_listing();
    let out = sb.run(args);
    let err = stderr(&out);
    assert_eq!(code(&out), 1, "stderr: {err}\nstdout: {}", stdout(&out));
    assert!(
        err.contains("pulse-factory: arm refused:"),
        "the message must start with the refusal: {err}"
    );
    assert!(err.contains(cause), "expected {cause:?} in: {err}");
    assert_eq!(sb.state_listing(), before, "the state directory changed");
    assert!(!sb.state.exists() || before.len() == sb.state_listing().len());
}

fn with(sb: &Sandbox, extra: &[&str]) -> Vec<String> {
    let mut a = sb.arm_args();
    a.extend(extra.iter().map(|s| (*s).to_string()));
    a
}

/// The normal arm with one parameter replaced.
fn replace_param(sb: &Sandbox, key: &str, value: &str) -> Vec<String> {
    let mut a = sb.arm_args();
    let prefix = format!("{key}=");
    for item in a.iter_mut() {
        if item.starts_with(&prefix) {
            *item = format!("{key}={value}");
        }
    }
    a.push("--dry-run".into());
    a
}

#[test]
fn a_seat_cannot_arm() {
    let sb = Sandbox::new();
    let before = sb.state_listing();
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = sb
        .cmd(&refs)
        .env("PF_SEAT_TOKEN", "secret-token")
        .output()
        .unwrap();
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains("arm refused"), "{err}");
    assert!(err.contains("PF_SEAT_TOKEN"), "{err}");
    assert!(
        !err.contains("secret-token"),
        "the token must not appear: {err}"
    );
    assert_eq!(sb.state_listing(), before);
}

#[test]
fn a_missing_parameter_is_named() {
    let sb = Sandbox::new();
    let mut args = sb.arm_args();
    let at = args.iter().position(|a| a.starts_with("count=")).unwrap();
    args.drain(at - 1..=at);
    args.push("--dry-run".into());
    refused(&sb, &args, "count");
}

#[test]
fn an_undeclared_parameter_is_named() {
    let sb = Sandbox::new();
    refused(&sb, &with(&sb, &["--param", "zzz=1", "--dry-run"]), "zzz");
}

#[test]
fn a_parameter_that_arm_sets_is_refused() {
    let sb = Sandbox::new();
    refused(
        &sb,
        &with(&sb, &["--param", "briefs=/tmp/x", "--dry-run"]),
        "briefs",
    );
    refused(
        &sb,
        &with(&sb, &["--param", "forge_repo=a/b", "--dry-run"]),
        "forge_repo",
    );
    refused(
        &sb,
        &with(&sb, &["--param", "spine=other", "--dry-run"]),
        "spine",
    );
}

#[test]
fn a_duplicate_parameter_is_refused() {
    let sb = Sandbox::new();
    refused(
        &sb,
        &with(&sb, &["--param", "count=3", "--dry-run"]),
        "count",
    );
}

#[test]
fn a_bad_workflow_is_refused() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.package.join("workflow.yaml"),
        "version: 1\nname: [broken\n",
    )
    .unwrap();
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    refused(&sb, &args, "workflow.yaml");

    // A name that finds no package under the share directory.
    let sb = Sandbox::new();
    let mut args = sb.arm_args();
    let at = args.iter().position(|a| a == "--workflow").unwrap();
    args[at + 1] = "nonesuch".into();
    args.push("--dry-run".into());
    refused(&sb, &args, "nonesuch");
}

#[test]
fn a_failing_plan_check_is_refused() {
    let o = Options {
        plan_check: "[false]".into(),
        ..Options::default()
    };
    let sb = Sandbox::with(&o);
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    refused(&sb, &args, "plan_check");
}

#[test]
fn a_plan_check_that_prints_a_non_number_is_refused() {
    let sb = Sandbox::new();
    refused(&sb, &replace_param(&sb, "count", "abc"), "whole number");
}

#[test]
fn a_plugin_below_its_minimum_is_refused() {
    let o = Options {
        plugin_min: "9.0.0".into(),
        ..Options::default()
    };
    let sb = Sandbox::with(&o);
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    refused(&sb, &args, "methodkit");
}

#[test]
fn a_missing_plugin_root_is_refused() {
    let sb = Sandbox::new();
    let mut args = sb.arm_args();
    args.extend([
        "--plugin-root".into(),
        "/nonesuch/plugins".into(),
        "--dry-run".into(),
    ]);
    refused(&sb, &args, "/nonesuch/plugins");
}

#[test]
fn dry_run_with_yes_is_refused() {
    let sb = Sandbox::new();
    refused(&sb, &with(&sb, &["--dry-run", "--yes"]), "--dry-run");
}

#[test]
fn a_spine_name_with_a_slash_is_refused() {
    let sb = Sandbox::new();
    let mut args = sb.arm_args();
    args[1] = "a/b".into();
    args.push("--dry-run".into());
    refused(&sb, &args, "a/b");
}

#[test]
fn a_relative_directory_value_is_refused() {
    let sb = Sandbox::new();
    for value in [".", "./x", "../x"] {
        refused(&sb, &replace_param(&sb, "repo", value), "repo");
    }
}

#[test]
fn a_non_github_origin_is_refused() {
    let sb = Sandbox::new();
    sb.set_origin("https://gitlab.com/acme/widgets.git");
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    refused(&sb, &args, "GitHub");
    assert!(
        sb.gh_calls().is_empty(),
        "gh must not be called for a non-GitHub origin"
    );
}

#[test]
fn a_failing_gh_is_refused() {
    let sb = Sandbox::new();
    sb.gh_fails();
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    refused(&sb, &args, "gh");
}

#[test]
fn a_run_with_a_running_session_is_refused() {
    let sb = Sandbox::new();
    let _stub = Stub::start(
        &sb.state,
        vec![json!({"id": "s0-1", "spine": "s0", "state": "active", "running_sessions": 1})],
    );
    let mut args = sb.arm_args();
    args.push("--yes".into());
    refused(&sb, &args, "s0-1");
    // Even with --replace naming it: a live seat blocks the new run.
    let mut args = sb.arm_args();
    args.extend(["--replace".into(), "s0-1".into(), "--yes".into()]);
    refused(&sb, &args, "running session");
}

#[test]
fn an_earlier_run_not_at_an_end_state_is_refused_unless_replace_names_it() {
    let sb = Sandbox::new();
    let stub = Stub::start(
        &sb.state,
        vec![
            json!({"id": "s0-1", "spine": "s0", "state": "active", "running_sessions": 0}),
            json!({"id": "s0-2", "spine": "s0", "state": "ended", "running_sessions": 0}),
        ],
    );
    let mut args = sb.arm_args();
    args.push("--yes".into());
    refused(&sb, &args, "s0-1");
    assert!(
        !stub.verbs().contains(&"arm".to_string()),
        "nothing may be sent"
    );

    // --replace must name a run that is active.
    let mut args = sb.arm_args();
    args.extend(["--replace".into(), "s0-2".into(), "--yes".into()]);
    refused(&sb, &args, "s0-2");
    let mut args = sb.arm_args();
    args.extend(["--replace".into(), "nonesuch-9".into(), "--yes".into()]);
    refused(&sb, &args, "nonesuch-9");
    assert!(!stub.verbs().contains(&"arm".to_string()));
}

#[test]
fn a_value_with_a_line_break_a_quote_or_a_placeholder_stays_data() {
    let sb = Sandbox::new();
    let proof = sb.root.path().join("pwned");
    let value = format!("x\"y\n{{param.count}}'; touch {}; '", proof.display());
    let mut args = replace_param(&sb, "spine_branch", &value);
    args.push("--json".into());
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!proof.exists(), "a parameter value must run nothing");
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let merge = v["merges"][0]["base"].as_str().unwrap().to_string();
    assert_eq!(
        merge, value,
        "the value must reach the summary as it was, rendered once"
    );
    assert_eq!(sb.state_listing(), Vec::<String>::new());
}
