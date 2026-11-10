//! AC-11: every fixture says where it came from, and the replay binary refuses a fixture whose body
//! changed after recording. The replay starts no other program: it also runs with an empty `PATH`.

mod common;

use std::process::{Command, Output, Stdio};

use common::*;
use pf_adapter_claude::pinned;
use pf_engine::claim::sha256_hex;

const REPLAY: &str = env!("CARGO_BIN_EXE_pf-claude-replay");

fn replay(args: &[&str]) -> Output {
    Command::new(REPLAY)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("the replay binary runs with an empty environment")
}

fn fixture_path(name: &str) -> String {
    fixtures_dir()
        .join(format!("{name}.jsonl"))
        .display()
        .to_string()
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

#[test]
fn every_header_names_the_version_the_recorder_the_case_and_the_hash_of_its_body() {
    let all = all_fixtures();
    assert!(!all.is_empty());
    for f in all {
        let h = &f.header["pf_fixture"];
        let obj = h
            .as_object()
            .unwrap_or_else(|| panic!("{}: a header", f.name));
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            ["body_sha256", "case", "cli_version", "recorded", "recorder"],
            "{}: the header holds these fields only",
            f.name
        );
        assert_eq!(h["cli_version"], "2.1.319", "{}", f.name);
        assert_eq!(h["cli_version"], pinned::CLI_VERSION, "{}: the pin", f.name);
        // Every fixture is a recording: none is hand-written.
        assert_eq!(h["recorder"], "real_probe_chat", "{}", f.name);
        assert_eq!(
            h["case"],
            f.name.as_str(),
            "{}: the case is the file name",
            f.name
        );
        assert!(
            is_date(h["recorded"].as_str().unwrap_or("")),
            "{}: recorded",
            f.name
        );

        let text =
            std::fs::read_to_string(fixtures_dir().join(format!("{}.jsonl", f.name))).unwrap();
        let body: Vec<&str> = text.lines().skip(1).filter(|l| !l.is_empty()).collect();
        assert_eq!(
            h["body_sha256"].as_str().unwrap(),
            sha256_hex(body.join("\n").as_bytes()),
            "{}: body_sha256 is the hash of the body lines joined with a newline",
            f.name
        );
    }
}

#[test]
fn every_body_line_has_to_a_time_and_a_message() {
    for f in all_fixtures() {
        let text =
            std::fs::read_to_string(fixtures_dir().join(format!("{}.jsonl", f.name))).unwrap();
        let mut last = 0u64;
        for (i, line) in text.lines().skip(1).enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let to = v["to"].as_str().unwrap_or("");
            assert!(
                to == "cli" || to == "host",
                "{} line {}: to is {to:?}",
                f.name,
                i + 2
            );
            let t = v["t_ms"]
                .as_u64()
                .unwrap_or_else(|| panic!("{} line {}: t_ms", f.name, i + 2));
            assert!(t >= last, "{} line {}: time goes backward", f.name, i + 2);
            last = t;
            assert!(v["msg"].is_object(), "{} line {}: msg", f.name, i + 2);
        }
    }
}

#[test]
fn the_replay_prints_the_pinned_version_for_each_fixture_with_an_empty_path() {
    for f in all_fixtures() {
        let out = replay(&["--fixture", &fixture_path(&f.name), "--version"]);
        assert!(out.status.success(), "{}: {:?}", f.name, out.status);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "2.1.319 (Claude Code)",
            "{}",
            f.name
        );
    }
}

#[test]
fn the_replay_refuses_a_body_changed_after_recording_with_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let text = std::fs::read_to_string(fixture_path("plain_turn")).unwrap();
    // One character of the body changes; the header still carries the old hash.
    let changed = text.replacen("\"t_ms\":0", "\"t_ms\":1", 1);
    assert_ne!(changed, text, "the edit applied");
    let path = dir.path().join("plain_turn.jsonl");
    std::fs::write(&path, changed).unwrap();
    for extra in [&["--version"][..], &[][..]] {
        let mut args = vec!["--fixture", path.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = replay(&args);
        assert_eq!(out.status.code(), Some(2), "args {args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("changed after recording"), "{err}");
        assert!(
            out.stdout.is_empty(),
            "nothing is played from a changed body"
        );
    }
}

#[test]
fn the_replay_refuses_a_missing_file_a_missing_header_and_no_fixture_with_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let nofile = dir.path().join("none.jsonl");
    assert_eq!(
        replay(&["--fixture", nofile.to_str().unwrap()])
            .status
            .code(),
        Some(2)
    );
    let bare = dir.path().join("bare.jsonl");
    std::fs::write(&bare, "{\"msg\":{},\"t_ms\":0,\"to\":\"cli\"}\n").unwrap();
    assert_eq!(
        replay(&["--fixture", bare.to_str().unwrap()]).status.code(),
        Some(2)
    );
    assert_eq!(replay(&[]).status.code(), Some(2), "no --fixture");
    assert_eq!(
        replay(&["--fixture", &fixture_path("models"), "--pace", "fast"])
            .status
            .code(),
        Some(2),
        "an unknown pace"
    );
}
