//! AC-18: the managed copy of the CLI. A temporary `share_dir` holds a shell script as the copy, with
//! a test pin made from its SHA-256 and size. No network, no real `claude`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime};

use common::*;
use pf_adapter_claude::pinned::{self, Pin};
use pf_adapter_claude::sha256::{Sha256, hash_file};
use pf_adapter_claude::{ClaudeChat, ClaudeConfig};
use pf_engine::claim::sha256_hex;
use pf_engine::port::chat::{ChatError, ChatPort};

fn not_ready(r: Result<impl Sized, ChatError>) -> String {
    match r {
        Err(ChatError::RuntimeNotReady(m)) => m,
        Err(e) => panic!("expected RuntimeNotReady, got {e}"),
        Ok(_) => panic!("expected RuntimeNotReady, got a chat"),
    }
}

fn open_in(c: &ClaudeChat) -> Result<pf_engine::port::chat::ChatOpened, ChatError> {
    let work = tempfile::tempdir().unwrap();
    // The directory only has to exist while `open` runs.
    c.open(&spec(work.path()))
}

fn write_exec(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ------------------------------------------------------------------ the pin

#[test]
fn the_pin_names_the_version_the_sdk_package_and_two_platforms() {
    assert_eq!(pinned::CLI_VERSION, "2.1.319");
    assert!(
        pinned::SDK_VERSION
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "the SDK package version is a dotted number: {}",
        pinned::SDK_VERSION
    );
    assert_eq!(
        pinned::PACKAGE_PATTERN,
        "@anthropic-ai/claude-agent-sdk-<platform>"
    );
    for platform in ["linux-x64", "darwin-arm64"] {
        let p = pinned::PLATFORMS
            .iter()
            .find(|p| p.platform == platform)
            .unwrap_or_else(|| panic!("{platform} is pinned"));
        assert_eq!(p.sha256.len(), 64, "{platform}: 64 hex digits");
        assert!(
            p.sha256
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{platform}: lower-case hex"
        );
        assert!(p.size > 100_000_000, "{platform}: a real binary size");
        let release = Pin::release();
        assert_eq!(release.cli_version, "2.1.319");
        assert_eq!(
            release.platforms[platform],
            (p.sha256.to_string(), p.size),
            "Pin::release carries the constants"
        );
    }
    assert_eq!(pinned::platform_key("linux", "x86_64"), Some("linux-x64"));
    assert_eq!(
        pinned::platform_key("macos", "aarch64"),
        Some("darwin-arm64")
    );
    assert_eq!(pinned::platform_key("windows", "x86_64"), None);
    assert_eq!(pinned::platform_key("linux", "aarch64"), None);
}

// ------------------------------------------------------------------ the SHA-256

fn hex_of(chunks: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for c in chunks {
        h.update(c);
    }
    h.finish_hex()
}

#[test]
fn sha256_matches_the_fips_vectors() {
    assert_eq!(
        hex_of(&[b""]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex_of(&[b"abc"]),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex_of(&[b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"]),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    let a = [b'a'; 1000];
    let chunks: Vec<&[u8]> = (0..1000).map(|_| &a[..]).collect();
    assert_eq!(
        hex_of(&chunks),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        "one million 'a'"
    );
}

#[test]
fn sha256_does_not_depend_on_how_the_input_is_cut() {
    let data: Vec<u8> = (0..10_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let whole = hex_of(&[&data]);
    assert_eq!(
        whole,
        sha256_hex(&data),
        "agrees with the engine's own hash"
    );
    for cut in [1usize, 7, 55, 56, 63, 64, 65, 127, 4096] {
        let chunks: Vec<&[u8]> = data.chunks(cut).collect();
        assert_eq!(hex_of(&chunks), whole, "chunks of {cut}");
    }
}

#[test]
fn hash_file_agrees_with_the_engine_hash_on_a_file_larger_than_one_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big");
    let data: Vec<u8> = (0..(2 * 1024 * 1024 + 7) as u32)
        .map(|i| (i % 253) as u8)
        .collect();
    std::fs::write(&path, &data).unwrap();
    assert_eq!(hash_file(&path).unwrap(), sha256_hex(&data));
    assert!(hash_file(&dir.path().join("missing")).is_err());
}

// ------------------------------------------------------------------ resolve and verify

#[test]
fn with_no_override_the_binary_is_in_the_share_dir_and_a_good_copy_is_qualified() {
    let m = good_stub();
    assert_eq!(
        m.bin,
        m.share.join("claude").join("2.1.319").join("claude"),
        "<share_dir>/claude/<version>/claude"
    );
    let c = chat(&m);
    let opened = open_in(&c).expect("a good copy opens");
    assert!(opened.qualified);
    assert_eq!(opened.runtime_version, "2.1.319");
    assert_eq!(recorded_calls(&m), ["version", "run"]);
    c.stop(&opened.chat).unwrap();
}

#[test]
fn a_missing_copy_is_not_ready_names_the_path_and_the_fix_and_starts_nothing() {
    let m = good_stub();
    std::fs::remove_file(&m.bin).unwrap();
    let msg = not_ready(open_in(&chat(&m)));
    assert!(
        msg.contains(&m.bin.display().to_string()),
        "names the path: {msg}"
    );
    assert!(msg.contains("install step"), "names the fix: {msg}");
    assert!(
        recorded_calls(&m).is_empty(),
        "no process, not even --version"
    );
}

#[test]
fn a_copy_with_one_byte_changed_is_not_ready_and_starts_nothing() {
    let m = good_stub();
    let mut bytes = std::fs::read(&m.bin).unwrap();
    let last = bytes.len() - 1;
    bytes[last] = bytes[last].wrapping_add(1);
    std::fs::write(&m.bin, &bytes).unwrap();
    let msg = not_ready(open_in(&chat(&m)));
    assert!(
        msg.contains(&m.bin.display().to_string()),
        "names the path: {msg}"
    );
    assert!(
        msg.contains("SHA-256") && msg.contains("install step"),
        "{msg}"
    );
    assert!(recorded_calls(&m).is_empty(), "the changed copy never runs");
}

#[test]
fn a_copy_of_another_size_is_not_ready_and_starts_nothing() {
    let m = good_stub();
    let mut bytes = std::fs::read(&m.bin).unwrap();
    bytes.push(b'\n');
    std::fs::write(&m.bin, &bytes).unwrap();
    let msg = not_ready(open_in(&chat(&m)));
    assert!(
        msg.contains(&m.bin.display().to_string()),
        "names the path: {msg}"
    );
    assert!(
        msg.contains("size") && msg.contains("install step"),
        "{msg}"
    );
    assert!(recorded_calls(&m).is_empty());
}

#[test]
fn a_platform_with_no_pin_is_not_ready_and_starts_nothing() {
    let m = good_stub();
    for pin in [
        Pin::single(pinned::CLI_VERSION, "plan9-mips", "00", 1),
        Pin {
            cli_version: pinned::CLI_VERSION.to_string(),
            platforms: Default::default(),
        },
    ] {
        let mut cfg = config(&m);
        cfg.pin = pin;
        let msg = not_ready(open_in(&ClaudeChat::new(cfg)));
        assert!(msg.contains("pin"), "{msg}");
        assert!(recorded_calls(&m).is_empty());
    }
}

#[test]
fn a_copy_that_prints_another_version_is_refused_before_a_process_starts() {
    let m = managed("2.1.318 (Claude Code)", "cat > /dev/null");
    let err = open_in(&chat(&m)).expect_err("a wrong version");
    assert!(matches!(err, ChatError::Refused(_)), "{err}");
    assert!(err.to_string().contains("2.1.318"), "{err}");
    assert_eq!(recorded_calls(&m), ["version"], "only --version ran");
}

#[test]
fn a_claude_on_path_is_never_called_when_the_managed_copy_is_missing() {
    // One test changes the process environment, so this is the only one that does.
    let m = good_stub();
    std::fs::remove_file(&m.bin).unwrap();
    let decoy_dir = tempfile::tempdir().unwrap();
    let mark = decoy_dir.path().join("called");
    write_exec(
        &decoy_dir.path().join("claude"),
        &format!(
            "#!/bin/sh\necho called >> '{}'\necho '2.1.319 (Claude Code)'\n",
            mark.display()
        ),
    );
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![decoy_dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&old));
    // SAFETY: this is the only test of the file that writes the environment; the others only read
    // `PATH`, and they keep every directory it held.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    let result = open_in(&chat(&m));
    unsafe { std::env::set_var("PATH", old) };
    let msg = not_ready(result);
    assert!(msg.contains("install step"), "{msg}");
    assert!(!mark.exists(), "the claude on PATH was never called");
}

#[test]
fn a_copy_is_hashed_once_and_again_after_its_modification_time_changes() {
    let m = good_stub();
    let c = chat(&m);
    assert_eq!(c.hash_count(), 0);
    let first = open_in(&c).unwrap();
    assert_eq!(c.hash_count(), 1);
    c.stop(&first.chat).unwrap();
    let second = open_in(&c).unwrap();
    assert_eq!(c.hash_count(), 1, "an unchanged copy is not hashed again");
    c.stop(&second.chat).unwrap();
    // The same bytes, a later modification time: it verifies again, and passes.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&m.bin)
        .unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    drop(file);
    let third = open_in(&c).unwrap();
    assert_eq!(
        c.hash_count(),
        2,
        "a changed modification time hashes again"
    );
    assert!(third.qualified);
    c.stop(&third.chat).unwrap();
}

// ------------------------------------------------------------------ the override

fn override_script(dir: &std::path::Path, version_line: &str) -> std::path::PathBuf {
    let records = dir.join("override-records");
    std::fs::create_dir_all(&records).unwrap();
    let bin = dir.join("other-claude");
    write_exec(
        &bin,
        &stub_script(
            &records,
            version_line,
            &format!("{ANSWER_INITIALIZE}cat > /dev/null"),
        ),
    );
    bin
}

#[test]
fn an_override_runs_with_no_hash_check_and_is_marked_unqualified() {
    for version in ["2.1.319", "9.9.9"] {
        let m = good_stub();
        // The managed copy is gone and the pin would not match: the override ignores both.
        std::fs::remove_file(&m.bin).unwrap();
        let other = tempfile::tempdir().unwrap();
        let bin = override_script(other.path(), &format!("{version} (Claude Code)"));
        let mut cfg = config(&m);
        cfg.claude_bin_override = Some(bin);
        let c = ClaudeChat::new(cfg);
        let opened = open_in(&c).unwrap_or_else(|e| panic!("{version}: {e}"));
        assert!(
            !opened.qualified,
            "{version}: an override is never qualified"
        );
        assert_eq!(opened.runtime_version, version);
        assert_eq!(c.hash_count(), 0, "{version}: no SHA-256 check");
        assert!(recorded_calls(&m).is_empty(), "the managed copy never ran");
        c.stop(&opened.chat).unwrap();
    }
}

#[test]
fn an_override_whose_version_cannot_be_read_is_failed() {
    let m = good_stub();
    let other = tempfile::tempdir().unwrap();
    let bin = other.path().join("odd");
    write_exec(&bin, "#!/bin/sh\necho 'no version here'\n");
    let mut cfg: ClaudeConfig = config(&m);
    cfg.claude_bin_override = Some(bin);
    let err = open_in(&ClaudeChat::new(cfg)).expect_err("no readable version");
    assert!(matches!(err, ChatError::Failed(_)), "{err}");
    let mut cfg = config(&m);
    cfg.claude_bin_override = Some(other.path().join("does-not-exist"));
    let err = open_in(&ClaudeChat::new(cfg)).expect_err("cannot run");
    assert!(matches!(err, ChatError::Failed(_)), "{err}");
}
