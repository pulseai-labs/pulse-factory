//! AC-13 of `r2.s2.w3` (the `stranded-close.sh` debt line): `scripts/demo/lib/status.sh` gives
//! `status_json <run>`. It runs `"$PF_BIN" status <run> --json`, and on a non-zero exit it tries up to
//! three more times, 0.5 s apart. The fourth failure fails with "status failed 4 times: <last
//! stderr>". It never reads a failed call's output as data. The four demo scripts that poll a run's
//! status use it in every loop and call `"$PF_BIN" status` nowhere else.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn helper() -> PathBuf {
    root().join("scripts/demo/lib/status.sh")
}

/// A stub `PF_BIN` that fails `fail_first` times, then answers `{"state":"active"}`. It counts its
/// calls in `calls`, and a failing call writes some output to stdout before it fails: the helper
/// must not read that as data.
fn stub(dir: &Path, fail_first: u32) -> PathBuf {
    let bin = dir.join("pf-stub");
    std::fs::write(
        &bin,
        format!(
            r#"#!/bin/sh
n=$(( $(cat "{calls}" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "{calls}"
if [ "$n" -le {fail_first} ]; then
  echo '{{"state":"ended","half":"written"}}'
  echo "call $n: the controller is busy" >&2
  exit 1
fi
[ "$1" = status ] && [ "$2" = "r-1" ] && [ "$3" = "--json" ] || {{ echo "bad arguments: $*" >&2; exit 2; }}
echo '{{"state":"active"}}'
"#,
            calls = dir.join("calls").display(),
        ),
    )
    .unwrap();
    let mut perm = std::fs::metadata(&bin).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&bin, perm).unwrap();
    bin
}

fn calls(dir: &Path) -> u32 {
    std::fs::read_to_string(dir.join("calls"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Source the helper and run `status_json r-1` in a script that has `set -euo pipefail`, as a demo
/// does. The script prints the answer it got, or exits with the helper's failure.
fn run_helper(bin: &Path) -> (Output, Duration) {
    let script = format!(
        "set -euo pipefail\n. '{}'\nPF_BIN='{}'\nstatus=\"$(status_json r-1)\" || exit 7\nprintf 'got: %s\\n' \"$status\"\n",
        helper().display(),
        bin.display()
    );
    let start = Instant::now();
    let out = Command::new("bash").arg("-c").arg(script).output().unwrap();
    (out, start.elapsed())
}

#[test]
fn the_helper_returns_the_answer_of_a_status_that_works() {
    let dir = tempfile::tempdir().unwrap();
    let (out, took) = run_helper(&stub(dir.path(), 0));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "got: {\"state\":\"active\"}\n"
    );
    assert_eq!(calls(dir.path()), 1);
    assert!(took < Duration::from_millis(400), "{took:?}");
}

#[test]
fn a_status_that_fails_twice_is_retried_and_its_third_answer_is_returned() {
    let dir = tempfile::tempdir().unwrap();
    let (out, took) = run_helper(&stub(dir.path(), 2));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The answer is the third call's, never the half-written output of a failed call.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "got: {\"state\":\"active\"}\n"
    );
    assert_eq!(calls(dir.path()), 3);
    assert!(
        took >= Duration::from_millis(900),
        "two gaps of 0.5 s: {took:?}"
    );
}

#[test]
fn a_status_that_fails_three_times_is_answered_by_the_fourth_call() {
    let dir = tempfile::tempdir().unwrap();
    let (out, _) = run_helper(&stub(dir.path(), 3));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(calls(dir.path()), 4);
}

#[test]
fn a_status_that_always_fails_fails_the_script_after_four_calls_and_names_the_last_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let (out, took) = run_helper(&stub(dir.path(), 99));
    assert_eq!(out.status.code(), Some(7), "the script must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("status failed 4 times: call 4: the controller is busy"),
        "{err}"
    );
    assert_eq!(calls(dir.path()), 4);
    // Nothing a failed call printed became data.
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("half"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        took >= Duration::from_millis(1400),
        "three gaps of 0.5 s: {took:?}"
    );
}

#[test]
fn the_four_scripts_source_the_helper_and_call_status_nowhere_else() {
    for name in [
        "stranded-close.sh",
        "golden-journey-a.sh",
        "golden-journey-b.sh",
        "spine-base.sh",
    ] {
        let text = std::fs::read_to_string(root().join("scripts/demo").join(name)).unwrap();
        assert!(
            text.contains("lib/status.sh"),
            "{name} does not source the helper"
        );
        assert!(
            text.contains("status_json"),
            "{name} does not call the helper"
        );
        for (i, line) in text.lines().enumerate() {
            let code = line.split('#').next().unwrap_or_default();
            assert!(
                !code.contains("\"$PF_BIN\" status") && !code.contains("$PF_BIN status"),
                "{name}:{}: calls status by hand: {line}",
                i + 1
            );
        }
    }
}
