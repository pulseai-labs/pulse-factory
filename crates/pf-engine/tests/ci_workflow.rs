//! AC-18: the test reads `.github/workflows/ci.yml`, ignores comment lines, and fails unless it
//! triggers on push and pull request and runs the three commands of the spec's Approach 5. It
//! also reads `rust-toolchain.toml` and fails unless the channel is 1.98.1. Each check has a
//! case over an inline bad sample that must fire.

use std::collections::BTreeSet;
use std::path::PathBuf;

const COMMANDS: [&str; 3] = [
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets --locked -- -D warnings",
    "cargo test --workspace --locked",
];

const CHANNEL: &str = "1.98.1";

/// The lines of the file without whole-line comments and without blank lines.
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .collect()
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Drop a trailing ` # comment` from a value.
fn strip_trailing_comment(value: &str) -> &str {
    match value.find(" #") {
        Some(at) => value[..at].trim_end(),
        None => value.trim_end(),
    }
}

/// The events named under the top-level `on:` key.
fn triggers(yaml: &str) -> BTreeSet<String> {
    let lines = code_lines(yaml);
    let mut found = BTreeSet::new();
    let Some(at) = lines
        .iter()
        .position(|l| indent(l) == 0 && (l.starts_with("on:") || l.starts_with("\"on\":")))
    else {
        return found;
    };
    let (_, inline) = lines[at].split_once(':').unwrap();
    let inline = strip_trailing_comment(inline.trim());
    if !inline.is_empty() {
        // `on: push` or `on: [push, pull_request]`
        for name in inline.trim_matches(['[', ']']).split(',') {
            found.insert(name.trim().trim_matches(['"', '\'']).to_string());
        }
        return found;
    }
    for line in lines[at + 1..].iter().take_while(|l| indent(l) > 0) {
        let t = line.trim();
        if let Some(item) = t.strip_prefix("- ") {
            found.insert(item.trim().to_string());
        } else if let Some((key, _)) = t.split_once(':') {
            // Only the keys one level below `on:` are events.
            let first_child = indent(lines[at + 1]);
            if indent(line) == first_child {
                found.insert(key.trim().to_string());
            }
        }
    }
    found
}

/// The commands the file runs, from `run:` lines, one-line or block form.
fn run_commands(yaml: &str) -> Vec<String> {
    let lines = code_lines(yaml);
    let mut commands = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start().trim_start_matches("- ").trim_start();
        if let Some(value) = t.strip_prefix("run:") {
            let value = strip_trailing_comment(value.trim());
            if value == "|" || value == ">" || value == "|-" || value == ">-" {
                let base = indent(lines[i]);
                let mut j = i + 1;
                while j < lines.len() && indent(lines[j]) > base {
                    commands.push(strip_trailing_comment(lines[j].trim()).to_string());
                    j += 1;
                }
                i = j;
                continue;
            }
            commands.push(value.to_string());
        }
        i += 1;
    }
    commands
}

/// A step input or flag that picks a toolchain other than the one `rust-toolchain.toml` names.
fn toolchain_override(yaml: &str) -> Option<String> {
    code_lines(yaml).into_iter().find_map(|l| {
        let t = l.trim();
        let hit = t.starts_with("toolchain:")
            || t.contains("cargo +")
            || t.contains("rustup default")
            || t.contains("rustup override");
        hit.then(|| t.to_string())
    })
}

fn check_triggers(yaml: &str) -> Result<(), String> {
    let found = triggers(yaml);
    for needed in ["push", "pull_request"] {
        if !found.contains(needed) {
            return Err(format!(
                "the workflow does not trigger on {needed}; it triggers on {found:?}"
            ));
        }
    }
    Ok(())
}

fn check_commands(yaml: &str) -> Result<(), String> {
    let run = run_commands(yaml);
    for needed in COMMANDS {
        if !run.iter().any(|c| c == needed) {
            return Err(format!(
                "the workflow does not run `{needed}`; it runs {run:?}"
            ));
        }
    }
    Ok(())
}

fn check_no_override(yaml: &str) -> Result<(), String> {
    match toolchain_override(yaml) {
        None => Ok(()),
        Some(line) => Err(format!("the workflow picks its own toolchain: {line}")),
    }
}

/// The `channel` value of the `[toolchain]` table.
fn channel(toml: &str) -> Option<String> {
    let mut in_toolchain = false;
    for line in code_lines(toml) {
        let t = line.trim();
        if t.starts_with('[') {
            in_toolchain = t == "[toolchain]";
            continue;
        }
        if in_toolchain
            && let Some((key, value)) = t.split_once('=')
            && key.trim() == "channel"
        {
            return Some(
                strip_trailing_comment(value.trim())
                    .trim_matches(['"', '\''])
                    .to_string(),
            );
        }
    }
    None
}

fn check_channel(toml: &str) -> Result<(), String> {
    match channel(toml) {
        Some(c) if c == CHANNEL => Ok(()),
        Some(c) => Err(format!("the channel is {c:?}, not {CHANNEL:?}")),
        None => Err("the toolchain file names no channel".to_string()),
    }
}

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

// ------------------------------------------------------------------ the real files

#[test]
fn the_workflow_triggers_on_push_and_pull_request() {
    check_triggers(&repo_file(".github/workflows/ci.yml")).unwrap();
}

#[test]
fn the_workflow_runs_the_three_commands() {
    check_commands(&repo_file(".github/workflows/ci.yml")).unwrap();
}

#[test]
fn the_workflow_takes_its_toolchain_from_the_toolchain_file() {
    check_no_override(&repo_file(".github/workflows/ci.yml")).unwrap();
}

#[test]
fn the_toolchain_file_pins_the_channel() {
    check_channel(&repo_file("rust-toolchain.toml")).unwrap();
}

// ------------------------------------------------------------------ inline bad samples

const GOOD: &str = "\
name: ci
on:
  push:
  pull_request:
jobs:
  check:
    steps:
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets --locked -- -D warnings
      - run: cargo test --workspace --locked
";

#[test]
fn the_good_samples_pass_in_each_accepted_form() {
    check_triggers(GOOD).unwrap();
    check_commands(GOOD).unwrap();
    check_no_override(GOOD).unwrap();
    // Inline list, quoted key, and block commands are accepted too.
    check_triggers("on: [push, pull_request]\n").unwrap();
    check_triggers("on:\n  - push\n  - pull_request\n").unwrap();
    check_triggers("\"on\":\n  push:\n    branches: [main]\n  pull_request:\n").unwrap();
    check_commands(
        "jobs:\n  a:\n    steps:\n      - run: |\n          cargo fmt --all -- --check\n          cargo clippy --workspace --all-targets --locked -- -D warnings\n          cargo test --workspace --locked\n",
    )
    .unwrap();
    check_channel("[toolchain]\nchannel = \"1.98.1\"\n").unwrap();
    check_channel("[toolchain]\nchannel = '1.98.1' # pinned\n").unwrap();
}

#[test]
fn a_workflow_without_both_triggers_fires() {
    for (name, yaml) in [
        ("no on key", "name: ci\njobs: {}\n"),
        ("push only", "on:\n  push:\n"),
        ("pull request only", "on:\n  pull_request:\n"),
        ("push only, inline", "on: [push]\n"),
        ("push only, single", "on: push\n"),
        ("workflow_dispatch only", "on:\n  workflow_dispatch:\n"),
        (
            "pull_request in a comment",
            "on:\n  push:\n  # pull_request:\n",
        ),
        (
            "on key is a comment",
            "# on:\n#   push:\n#   pull_request:\njobs: {}\n",
        ),
        (
            "pull_request only under a job",
            "on:\n  push:\njobs:\n  a:\n    if: github.event_name == 'pull_request'\n",
        ),
    ] {
        assert!(
            check_triggers(yaml).is_err(),
            "the {name} sample must fire:\n{yaml}"
        );
    }
}

#[test]
fn a_workflow_missing_a_command_or_with_a_weaker_one_fires() {
    let fmt = COMMANDS[0];
    let clippy = COMMANDS[1];
    let test = COMMANDS[2];
    let with = |lines: &[&str]| {
        let mut s = String::from("on: [push, pull_request]\njobs:\n  a:\n    steps:\n");
        for l in lines {
            s.push_str(&format!("      - run: {l}\n"));
        }
        s
    };
    for (name, yaml) in [
        ("no fmt", with(&[clippy, test])),
        ("no clippy", with(&[fmt, test])),
        ("no test", with(&[fmt, clippy])),
        ("nothing", with(&[])),
        (
            "fmt without --check",
            with(&["cargo fmt --all", clippy, test]),
        ),
        (
            "clippy without -D warnings",
            with(&[fmt, "cargo clippy --workspace --all-targets --locked", test]),
        ),
        (
            "clippy without --locked",
            with(&[
                fmt,
                "cargo clippy --workspace --all-targets -- -D warnings",
                test,
            ]),
        ),
        (
            "clippy without --all-targets",
            with(&[
                fmt,
                "cargo clippy --workspace --locked -- -D warnings",
                test,
            ]),
        ),
        (
            "test without --locked",
            with(&[fmt, clippy, "cargo test --workspace"]),
        ),
        (
            "test without --workspace",
            with(&[fmt, clippy, "cargo test --locked"]),
        ),
        (
            "a command only in a comment",
            format!("{}      # - run: {fmt}\n", with(&[clippy, test])),
        ),
        (
            "a command only in a comment line of a block",
            format!(
                "{}      - run: |\n          # {fmt}\n          echo hi\n",
                with(&[clippy, test])
            ),
        ),
        (
            "a command only in a name",
            format!("{}      - name: {fmt}\n", with(&[clippy, test])),
        ),
    ] {
        assert!(
            check_commands(&yaml).is_err(),
            "the {name} sample must fire:\n{yaml}"
        );
    }
}

#[test]
fn a_workflow_that_picks_its_own_toolchain_fires() {
    for (name, yaml) in [
        (
            "an action input",
            "      - uses: dtolnay/rust-toolchain@stable\n        with:\n          toolchain: nightly\n",
        ),
        ("a plus flag", "      - run: cargo +nightly test\n"),
        ("rustup default", "      - run: rustup default nightly\n"),
    ] {
        assert!(
            check_no_override(yaml).is_err(),
            "the {name} sample must fire:\n{yaml}"
        );
    }
    // A comment that names a toolchain is not an override.
    check_no_override("      # toolchain: nightly\n").unwrap();
}

#[test]
fn a_toolchain_file_with_another_channel_fires() {
    for (name, toml) in [
        ("stable", "[toolchain]\nchannel = \"stable\"\n"),
        ("nightly", "[toolchain]\nchannel = \"nightly\"\n"),
        ("an older patch", "[toolchain]\nchannel = \"1.98.0\"\n"),
        ("a newer patch", "[toolchain]\nchannel = \"1.98.2\"\n"),
        ("a prefix", "[toolchain]\nchannel = \"1.98\"\n"),
        ("no channel", "[toolchain]\nprofile = \"minimal\"\n"),
        ("no toolchain table", "channel = \"1.98.1\"\n"),
        (
            "channel only in a comment",
            "[toolchain]\n# channel = \"1.98.1\"\nprofile = \"minimal\"\n",
        ),
        (
            "channel in another table",
            "[other]\nchannel = \"1.98.1\"\n",
        ),
    ] {
        assert!(
            check_channel(toml).is_err(),
            "the {name} sample must fire:\n{toml}"
        );
    }
}
