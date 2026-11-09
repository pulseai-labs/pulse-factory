//! AC-11 of `r1.s3.w3`: the managed Claude Code copy. The fetch runs against a `file://` registry
//! that holds a `.tgz` of a shell script, with a `Pin::single` made from that script's SHA-256 and
//! size. Nothing here reaches a network or starts the real `claude`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::chat::*;
use common::ctl::Live;
use common::*;
use pf_adapter_claude::pinned::{self, Pin, host_platform};
use pf_adapter_claude::sha256::hash_file;
use pf_adapter_claude::{CLAUDE_CODE_RUNTIME, ClaudeChat, ClaudeConfig};
use pf_engine::port::chat::ChatPort;
use pulse_factory::controller::chat_install::{copy_is_good, copy_path, fetch_runtime};

const CLI_VERSION: &str = "2.1.319";

fn platform() -> &'static str {
    host_platform().expect("the host has a pinned platform")
}

/// The path of the tarball inside a registry directory, as the fetch builds the URL.
fn tarball_in(registry: &Path) -> PathBuf {
    registry
        .join(format!("@anthropic-ai/claude-agent-sdk-{}", platform()))
        .join("-")
        .join(format!(
            "claude-agent-sdk-{}-{}.tgz",
            platform(),
            pinned::SDK_VERSION
        ))
}

/// A registry holding one tarball with `entry` (a path inside it) set to `script`. The pin is made
/// from `script`, so a tarball built from another script does not match it.
struct Registry {
    root: tempfile::TempDir,
}

impl Registry {
    fn url(&self) -> String {
        format!("file://{}", self.root.path().join("registry").display())
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("registry")
    }

    fn share(&self) -> PathBuf {
        self.root.path().join("share")
    }
}

fn script(text: &str) -> String {
    format!("#!/bin/sh\n# {text}\necho '{CLI_VERSION} (Claude Code)'\n")
}

fn pack(registry: &Path, entry: &str, body: &str) {
    let staging = tempfile::tempdir().unwrap();
    let file = staging.path().join(entry);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, body).unwrap();
    let tgz = tarball_in(registry);
    std::fs::create_dir_all(tgz.parent().unwrap()).unwrap();
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&tgz)
        .arg("-C")
        .arg(staging.path())
        .arg(entry)
        .status()
        .unwrap();
    assert!(status.success(), "tar made the test tarball");
}

/// A registry with the tarball of `script`, and the pin for it.
fn registry(script_text: &str) -> (Registry, Pin) {
    let reg = Registry {
        root: tempfile::tempdir().unwrap(),
    };
    pack(&reg.dir(), "package/claude", script_text);
    std::fs::create_dir_all(reg.share()).unwrap();
    (reg, pin_of(script_text))
}

fn pin_of(script_text: &str) -> Pin {
    let staging = tempfile::tempdir().unwrap();
    let file = staging.path().join("claude");
    std::fs::write(&file, script_text).unwrap();
    let sha = hash_file(&file).unwrap();
    Pin::single(CLI_VERSION, platform(), &sha, script_text.len() as u64)
}

fn fetch_dirs(share: &Path) -> Vec<String> {
    std::fs::read_dir(share.join("claude"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".fetch-"))
                .collect()
        })
        .unwrap_or_default()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_missing_copy_is_fetched_checked_and_placed_with_mode_0755() {
    let text = script("the good one");
    let (reg, pin) = registry(&text);
    let dest = fetch_runtime(&pin, &reg.share(), &reg.url()).expect("a good fetch");
    assert_eq!(dest, copy_path(&reg.share(), &pin));
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), text);
    assert_eq!(mode(&dest), 0o755);
    assert!(copy_is_good(&reg.share(), &pin).unwrap());
    assert!(fetch_dirs(&reg.share()).is_empty());
}

#[test]
fn a_copy_with_one_byte_changed_is_fetched_again() {
    let text = script("the good one");
    let (reg, pin) = registry(&text);
    let dest = fetch_runtime(&pin, &reg.share(), &reg.url()).unwrap();
    // The same size, one byte different.
    let mut bytes = std::fs::read(&dest).unwrap();
    let last = bytes.len() - 2;
    bytes[last] ^= 0x01;
    std::fs::write(&dest, &bytes).unwrap();
    assert!(!copy_is_good(&reg.share(), &pin).unwrap());

    fetch_runtime(&pin, &reg.share(), &reg.url()).expect("a fetch of the altered copy");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), text);
    assert!(copy_is_good(&reg.share(), &pin).unwrap());
    assert!(fetch_dirs(&reg.share()).is_empty());
}

#[test]
fn a_tarball_that_differs_from_the_pin_places_nothing_and_leaves_the_old_file() {
    let (reg, pin) = registry(&script("the pinned one"));
    // The registry now serves another binary of another size, and the share holds an old file.
    pack(
        &reg.dir(),
        "package/claude",
        &script("another one entirely"),
    );
    let dest = copy_path(&reg.share(), &pin);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, "the old file").unwrap();

    let why = fetch_runtime(&pin, &reg.share(), &reg.url()).unwrap_err();
    assert!(why.contains("pin says"), "{why}");
    assert!(why.contains(&reg.url()), "the cause names the URL: {why}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "the old file");
    assert!(
        fetch_dirs(&reg.share()).is_empty(),
        "no temporary directory is left"
    );

    // The same size and another content: the SHA-256 refuses it.
    let text = script("the pinned one");
    let other = text.replace("pinned", "pinnex");
    assert_eq!(other.len(), text.len());
    pack(&reg.dir(), "package/claude", &other);
    let why = fetch_runtime(&pin, &reg.share(), &reg.url()).unwrap_err();
    assert!(why.contains("SHA-256"), "{why}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "the old file");
    assert!(fetch_dirs(&reg.share()).is_empty());
}

#[test]
fn a_404_and_a_tarball_with_no_package_claude_name_the_cause_and_the_url() {
    let text = script("the good one");
    let pin = pin_of(&text);
    let reg = Registry {
        root: tempfile::tempdir().unwrap(),
    };
    std::fs::create_dir_all(reg.dir()).unwrap();
    std::fs::create_dir_all(reg.share()).unwrap();

    // Nothing at the path: the download fails.
    let why = fetch_runtime(&pin, &reg.share(), &reg.url()).unwrap_err();
    assert!(why.contains("the download failed"), "{why}");
    assert!(why.contains(&reg.url()), "{why}");
    assert!(why.contains(&format!(
        "claude-agent-sdk-{}-{}.tgz",
        platform(),
        pinned::SDK_VERSION
    )));
    assert!(fetch_dirs(&reg.share()).is_empty());
    assert!(!copy_path(&reg.share(), &pin).exists());

    // A tarball with another file in it.
    pack(&reg.dir(), "package/other", &text);
    let why = fetch_runtime(&pin, &reg.share(), &reg.url()).unwrap_err();
    assert!(why.contains("package/claude"), "{why}");
    assert!(why.contains(&reg.url()), "{why}");
    assert!(fetch_dirs(&reg.share()).is_empty());
    assert!(!copy_path(&reg.share(), &pin).exists());
}

#[test]
fn a_good_copy_is_not_fetched() {
    let (reg, pin) = registry(&script("the good one"));
    fetch_runtime(&pin, &reg.share(), &reg.url()).unwrap();
    // The registry directory can be gone: nothing is fetched.
    std::fs::remove_dir_all(reg.dir()).unwrap();
    let again = fetch_runtime(&pin, &reg.share(), &reg.url()).expect("no fetch for a good copy");
    assert_eq!(again, copy_path(&reg.share(), &pin));
}

#[test]
fn a_controller_with_a_registry_has_a_good_copy_after_its_background_check() {
    let (reg, pin) = registry(&script("the good one"));
    let (share, url, pin_for_cfg) = (reg.share(), reg.url(), pin.clone());
    let _live = Live::start_config(BIN.into(), move |cfg| {
        cfg.chat_registry = Some(url);
        cfg.chat_managed = true;
        cfg.chat_pin = pin_for_cfg;
        cfg.chat_share_dir = share;
    });
    let end = Instant::now() + Duration::from_secs(20);
    while !copy_is_good(&reg.share(), &pin).unwrap() {
        assert!(
            Instant::now() < end,
            "no good copy after the background check"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(mode(&copy_path(&reg.share(), &pin)), 0o755);
}

#[test]
fn a_failing_fetch_refuses_a_start_and_never_runs_a_claude_from_path() {
    let stub_dir = tempfile::tempdir().unwrap();
    let marker = stub_dir.path().join("ran");
    let stub = stub_dir.path().join("claude");
    std::fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    // An empty registry: the fetch fails.
    let reg = Registry {
        root: tempfile::tempdir().unwrap(),
    };
    std::fs::create_dir_all(reg.dir()).unwrap();
    let pin = pin_of(&script("never fetched"));
    let mut cc = ClaudeConfig::new(reg.share());
    cc.pin = pin.clone();
    cc.config_dir = reg.root.path().join("claude-config");
    cc.inherited_env = Some(
        [(
            "PATH".to_string(),
            format!("{}:/usr/bin:/bin", stub_dir.path().display()),
        )]
        .into(),
    );
    let chat: Arc<dyn ChatPort> = Arc::new(ClaudeChat::new(cc));
    let (share, url, pin_for_cfg) = (reg.share(), reg.url(), pin.clone());
    let live = Live::start_config(BIN.into(), move |cfg| {
        cfg.chat_runtimes
            .insert(CLAUDE_CODE_RUNTIME.to_string(), chat);
        cfg.chat_registry = Some(url);
        cfg.chat_managed = true;
        cfg.chat_pin = pin_for_cfg;
        cfg.chat_share_dir = share;
    });
    let root = repo(&live, "alpha");
    let project = add_project(&live, &root);
    let why = cli_refused(&live, &["session", "start", "--project", &project]);
    assert!(why.contains("install step for the chat runtime"), "{why}");
    assert!(!marker.exists(), "a claude on PATH ran");
    assert!(!copy_path(&reg.share(), &pin).exists());
}

#[test]
fn the_install_command_against_an_empty_registry_exits_1_and_names_the_url() {
    let registry = tempfile::tempdir().unwrap();
    let share = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let url = format!("file://{}", registry.path().display());
    let out = Command::new(BIN)
        .args(["chat-runtime", "install", "--json"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("PF_CHAT_REGISTRY", &url)
        .env("PF_SHARE_DIR", share.path())
        .output()
        .unwrap();
    assert_eq!(
        code(&out),
        1,
        "stdout: {} stderr: {}",
        stdout(&out),
        stderr(&out)
    );
    let said = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        said.contains(&format!(
            "{url}/@anthropic-ai/claude-agent-sdk-{}/-/claude-agent-sdk-{}-{}.tgz",
            platform(),
            platform(),
            pinned::SDK_VERSION
        )),
        "the URL of the pinned version: {said}"
    );
    assert!(!share.path().join("claude").join(CLI_VERSION).exists());
}
