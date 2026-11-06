//! r1.s1.w3: a seat may not set the git config that the controller would trust (ADR-0005).
//!
//! The rule `git-config-trust` denies every way a seat writes one of the keys that redirect git:
//! `git config <key> <value>` in any scope, `git -c` and `git --config-env`, a rename of a section
//! into one of them, and `git config --edit`. The deny holds for every value and every role.

mod common;

use common::*;

const RULE: &str = "git-config-trust";

#[test]
fn every_write_of_core_hooks_path_is_denied() {
    for c in [
        "git config core.hooksPath .githooks",
        "git config core.hooksPath /dev/null",
        "git config core.hooksPath ''",
        "git config --local core.hooksPath x",
        "git config --global core.hooksPath x",
        "git config --system core.hooksPath x",
        "git config --worktree core.hooksPath x",
        "git config --file f core.hooksPath x",
        "git config set core.hooksPath x",
        "git config --add core.hooksPath x",
        "git config --replace-all core.hooksPath x",
        "git config CORE.HOOKSPATH x",
        "git config core.HooksPath x",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn a_remote_command_or_transport_key_is_denied() {
    for c in [
        "git config remote.origin.receivepack x",
        "git config remote.origin.uploadPack x",
        "git config remote.origin.vcs x",
        // A subsection with a dot in it.
        "git config remote.a.b.vcs x",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn a_url_rewrite_key_is_denied() {
    for c in [
        "git config url.file:///tmp/x.insteadOf https://github.com/",
        "git config url.https://evil/.pushInsteadOf https://github.com/",
        "git config URL.x.INSTEADOF y",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn an_include_key_is_denied() {
    for c in [
        "git config include.path f",
        "git config includeIf.gitdir:/x/.path f",
    ] {
        denied_for_every_role(c, RULE);
    }
}

/// The transport keys that the controller's push reads from the global config (PR #37, C1). A bare
/// value passes `config_runs_command` for some of them. Here no value passes.
#[test]
fn a_transport_key_is_denied_for_every_value() {
    for c in [
        "git config --global core.sshCommand /tmp/x.sh",
        "git config --global core.gitProxy p",
        "git config --global http.proxy http://127.0.0.1:1",
        "git config http.https://github.com/.proxy p",
        "git config --global http.sslVerify false",
        "git config http.https://github.com/.sslVerify false",
        "git config --global credential.helper store",
        "git config credential.https://github.com.helper x",
        "git -c core.sshCommand=x push origin work/u1",
        "git -c http.sslVerify=false fetch",
        "git -c credential.helper=store fetch",
        // Moved from `unread_forms.rs`: a value that runs a command was `unparsed-command`. The key
        // redirects git, so the rule is now `git-config-trust` for any value.
        "git -c core.sshCommand='gh pr merge 1' fetch",
        "git -c credential.helper='!gh pr merge 1' fetch",
        "git config --global credential.helper '!gh pr merge 1'",
        "git config --file f core.sshCommand 'sh -c x'",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn a_rename_into_one_of_the_sections_is_denied() {
    for c in [
        "git config --rename-section foo core",
        "git config --rename-section foo url.x",
        "git config rename-section foo remote.y",
        "git config --rename-section foo include",
        "git config --rename-section foo includeIf.gitdir:/x/",
        "git config --rename-section foo CORE",
        "git config --global --rename-section foo core",
        // The sections of the transport keys.
        "git config --rename-section foo http",
        "git config --rename-section foo http.https://github.com/",
        "git config --rename-section foo credential.https://github.com",
    ] {
        denied_for_every_role(c, RULE);
    }
    // A new section that is a variable cannot be read.
    denied_for_every_role("git config --rename-section foo \"$s\"", "unparsed-command");
    // A rename into a section with no redirecting key stays allowed.
    allowed_for_every_role("git config --rename-section foo bar");
    allowed_for_every_role("git config --rename-section foo remote");
}

#[test]
fn opening_the_config_in_an_editor_is_denied() {
    for c in [
        "git config --edit",
        "git config -e",
        "git config edit",
        "git config --global --edit",
        "git config --file f --edit",
        // Moved from `unread_forms.rs`, where it was allowed: `--edit` was read as a key.
        "git config --edit --file \"$f\"",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn git_dash_c_and_config_env_are_denied_for_every_value() {
    for c in [
        "git -c core.hooksPath=/dev/null commit -m x",
        // No `=`: git reads the value as `true`.
        "git -c core.hooksPath status",
        "git -c core.hooksPath= status",
        "git -c url.x.insteadOf=https://github.com/ push origin work/u1",
        "git -c remote.origin.receivepack=x push origin work/u1",
        "git -c include.path=f status",
        "git -c CORE.HOOKSPATH=x status",
        "git --config-env core.hooksPath=VAR status",
        "git --config-env=url.x.insteadOf=VAR fetch",
        "git --config-env=core.hooksPath=VAR status",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn a_prefix_of_a_long_option_is_read_as_the_option() {
    for c in [
        "git config --fil f core.hooksPath x",
        "git config --edi",
        "git config --rename-s foo core",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn a_quoted_key_and_a_wrapped_or_compound_call_are_read() {
    for c in [
        "git config \"core.hooksPath\" x",
        "git config 'core.hooksPath' x",
        "nice git config core.hooksPath x",
        "true && git config url.x.insteadOf y",
    ] {
        denied_for_every_role(c, RULE);
    }
}

#[test]
fn a_read_a_listing_a_removal_and_another_key_stay_allowed() {
    for c in [
        "git config --get core.hooksPath",
        // A lone key is a read.
        "git config core.hooksPath",
        "git config get core.hooksPath",
        "git config --get-regexp '^url\\.'",
        "git config --list",
        "git config list",
        "git config --unset core.hooksPath",
        "git config unset url.x.insteadOf",
        "git config --remove-section url.x",
        // A rename away from the sections.
        "git config --rename-section url.x foo",
        "git config remote.origin.url",
        "git config user.name x",
        "git -c user.name=x log",
        "git -c core.pager=cat log",
    ] {
        allowed_for_every_role(c);
    }
}

#[test]
fn a_key_that_is_a_variable_stays_unparsed_command() {
    for c in [
        "git -c \"$k=x\" status",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=x git status",
    ] {
        denied_for_every_role(c, "unparsed-command");
    }
}

#[test]
fn the_reason_names_the_key() {
    let r = shipped("implementer");
    for (c, key) in [
        ("git -c core.hooksPath=x status", "core.hooksPath"),
        (
            "git --config-env=url.x.insteadOf=V fetch",
            "url.x.insteadOf",
        ),
        ("git config remote.origin.vcs x", "remote.origin.vcs"),
        ("git config --global http.proxy p", "http.proxy"),
    ] {
        let d = bash(&r, c);
        assert_eq!(d.rule, RULE, "{c}: {d:?}");
        assert!(d.reason.contains(key), "{c}: {}", d.reason);
    }
}
