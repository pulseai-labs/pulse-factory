//! AC-5: the environment of the chat process is an allowlist. A shell stub writes the environment it
//! got. The tests never touch the process's own environment: they pass `inherited_env`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use pf_adapter_claude::pinned;
use pf_engine::port::chat::{ChatError, ChatPort};

/// A stub that records its environment, then answers `initialize` and reads stdin to the end.
fn env_stub() -> Managed {
    managed(
        &format!("{} (Claude Code)", pinned::CLI_VERSION),
        &format!("env > \"$DIR/env\"\n{ANSWER_INITIALIZE}cat > /dev/null"),
    )
}

/// The names a shell adds to a child's environment on its own. They are not the adapter's.
const SHELL_ADDED: [&str; 4] = ["PWD", "OLDPWD", "SHLVL", "_"];

fn child_env(m: &Managed) -> BTreeMap<String, String> {
    std::fs::read_to_string(m.dir.join("env"))
        .expect("the stub recorded its environment")
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !SHELL_ADDED.contains(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The allowlist, each name set, and the names that must never reach the child.
fn inherited() -> (BTreeMap<String, String>, BTreeSet<&'static str>) {
    let allowed = [
        ("HOME", "/nonexistent-home"),
        ("USER", "dev"),
        ("LOGNAME", "dev"),
        ("PATH", "/usr/bin:/bin"),
        ("SHELL", "/bin/sh"),
        ("LANG", "C.UTF-8"),
        ("LANGUAGE", "en"),
        ("LC_ALL", "C.UTF-8"),
        ("LC_TIME", "C"),
        ("TZ", "UTC"),
        ("TMPDIR", "/tmp"),
        ("TERM", "dumb"),
        ("XDG_CONFIG_HOME", "/x/config"),
        ("XDG_DATA_HOME", "/x/data"),
        ("XDG_CACHE_HOME", "/x/cache"),
        ("XDG_STATE_HOME", "/x/state"),
        ("XDG_RUNTIME_DIR", "/x/run"),
        ("SSH_AUTH_SOCK", "/x/ssh"),
    ];
    let banned = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CONFIG_DIR",
        "PF_SEAT_TOKEN",
        "ORCA_AGENT_HOOK_TOKEN",
        "HERDR_SOCKET_PATH",
        "SEAT_MODS_ROLE",
    ];
    let mut map: BTreeMap<String, String> = allowed
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for name in banned {
        map.insert(name.to_string(), "leak".to_string());
    }
    (map, banned.into_iter().collect())
}

fn open_and_read(
    inherited: BTreeMap<String, String>,
    env: &[(&str, &str)],
    artifacts: Option<&str>,
) -> BTreeMap<String, String> {
    let m = env_stub();
    let mut cfg = config(&m);
    cfg.inherited_env = Some(inherited);
    let c = pf_adapter_claude::ClaudeChat::new(cfg);
    let work = tempfile::tempdir().unwrap();
    let mut s = spec(work.path());
    for (k, v) in env {
        s.env.insert(k.to_string(), v.to_string());
    }
    if let Some(a) = artifacts {
        s.options.insert("artifacts".into(), a.into());
    }
    let opened = c.open(&s).expect("open");
    let got = child_env(&m);
    c.stop(&opened.chat).unwrap();
    got
}

#[test]
fn the_child_holds_the_allowlist_the_env_map_and_the_two_adapter_names_only() {
    let (inherited, banned) = inherited();
    let allowed: BTreeSet<String> = inherited
        .keys()
        .filter(|k| !banned.contains(k.as_str()))
        .cloned()
        .collect();
    let got = open_and_read(
        inherited,
        &[("SEAT_MODS_ROLE", "orchestrator"), ("MY_VAR_2", "v")],
        None,
    );
    let mut want: BTreeSet<String> = allowed;
    want.extend(
        [
            "SEAT_MODS_ROLE",
            "MY_VAR_2",
            "DISABLE_AUTOUPDATER",
            "CLAUDE_CODE_ARTIFACT",
        ]
        .map(String::from),
    );
    let names: BTreeSet<String> = got.keys().cloned().collect();
    assert_eq!(names, want, "exactly these names reach the child");
    for name in banned {
        // `SEAT_MODS_ROLE` is in the env map here, so it passes with the map's value.
        if name == "SEAT_MODS_ROLE" {
            assert_eq!(
                got[name], "orchestrator",
                "the env map wins and passes through"
            );
        } else {
            assert!(!got.contains_key(name), "{name} must not reach the child");
        }
    }
    assert_eq!(got["DISABLE_AUTOUPDATER"], "1");
    assert_eq!(got["CLAUDE_CODE_ARTIFACT"], "1", "artifacts default on");
    assert_eq!(got["MY_VAR_2"], "v");
    assert_eq!(got["HOME"], "/nonexistent-home");
    assert_eq!(got["LC_TIME"], "C", "every LC_* name passes");
}

#[test]
fn an_inherited_seat_variable_does_not_pass_without_the_env_map() {
    let (inherited, _) = inherited();
    let got = open_and_read(inherited, &[], None);
    assert!(!got.contains_key("SEAT_MODS_ROLE"));
}

#[test]
fn artifacts_set_the_variable_explicitly_both_ways_and_beat_an_inherited_one() {
    for (option, want) in [(Some("on"), "1"), (Some("off"), "0"), (None, "1")] {
        let (mut inherited, _) = inherited();
        // The opposite of what the adapter must set, so a leak shows.
        inherited.insert(
            "CLAUDE_CODE_ARTIFACT".into(),
            if want == "1" { "0" } else { "1" }.into(),
        );
        inherited.insert("CLAUDE_CODE_ENTRYPOINT".into(), "cli".into());
        let got = open_and_read(inherited, &[], option);
        assert_eq!(got["CLAUDE_CODE_ARTIFACT"], want, "artifacts {option:?}");
        assert!(
            !got.contains_key("CLAUDE_CODE_ENTRYPOINT"),
            "an inherited entrypoint never reaches the child, and the adapter never sets it"
        );
        let claude_code: Vec<&String> = got
            .keys()
            .filter(|k| k.starts_with("CLAUDE_CODE_"))
            .collect();
        assert_eq!(
            claude_code,
            ["CLAUDE_CODE_ARTIFACT"],
            "the only CLAUDE_CODE_* name"
        );
    }
}

#[test]
fn a_reserved_or_malformed_env_name_is_refused_by_name_and_starts_no_process() {
    let reserved = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CONFIG_DIR",
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_ARTIFACT",
        "CLAUDE_CODE_ENTRYPOINT",
        "PF_SEAT_TOKEN",
        "ORCA_AGENT_HOOK_TOKEN",
        "HERDR_SOCKET_PATH",
        "DISABLE_AUTOUPDATER",
    ];
    let malformed = ["lower_case", "HAS-DASH", "HAS SPACE", ""];
    for name in reserved.into_iter().chain(malformed) {
        let m = env_stub();
        let c = chat(&m);
        let work = tempfile::tempdir().unwrap();
        let mut s = spec(work.path());
        s.env.insert(name.to_string(), "v".into());
        let err = c.open(&s).expect_err(name);
        assert!(matches!(err, ChatError::Refused(_)), "{name:?}: {err}");
        if !name.is_empty() {
            assert!(
                err.to_string().contains(name),
                "the refusal names {name}: {err}"
            );
        }
        assert!(
            !recorded_calls(&m).iter().any(|c| c == "run"),
            "{name:?}: no process starts"
        );
    }
}
