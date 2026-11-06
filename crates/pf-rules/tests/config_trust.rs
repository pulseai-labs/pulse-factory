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

/// PR #28, C3: git reads an option's value and an operand after `--` as words, not as flags. A guard
/// that read a flag from any word let `--comment --get` hide a write, and let `-- --list` hide one.
#[test]
fn an_option_value_or_an_operand_is_not_read_as_a_flag() {
    for c in [
        "git config --global --comment --get core.sshCommand /tmp/x",
        "git config --comment --list core.hooksPath x",
        "git config --comment --edit core.hooksPath x",
        "git config --type --unset core.hooksPath x",
        "git config --default --get core.hooksPath x",
        "git config core.hooksPath -- --list",
        "git config core.hooksPath -- --get",
        "git config core.hooksPath -- -x",
        "git config -- core.hooksPath x",
        "git config --file f -- core.hooksPath x",
        "git config set --value p core.hooksPath x",
        "git config set --global --value p core.sshCommand /tmp/x.sh",
        "git config set --value=p core.hooksPath x",
        "git config --comm c core.hooksPath x",
        "git config -f f core.hooksPath x",
        "git config -ff core.hooksPath x",
        "git config -t bool core.hooksPath x",
    ] {
        denied_for_every_role(c, RULE);
    }
}

/// PR #28, C3 (round 2): git stops reading flags at the first operand, so `--unset`, `--get`,
/// `--list` and `-l` after the key are values, not flags: `core.hooksPath --unset` writes the
/// value `--unset`. A guard that read them as flags let the write through.
#[test]
fn a_flag_after_the_first_operand_is_an_operand() {
    for c in [
        "git config core.hooksPath --unset",
        "git config core.hooksPath /tmp/x --get",
        "git config core.hooksPath /tmp/x --list",
        "git config core.hooksPath /tmp/x -l",
        "git config core.hooksPath /tmp/x --add",
        "git config core.hooksPath /tmp/x -e",
        "git config --global remote.forge.pushurl /tmp/x.git --get",
        "git config --global core.sshCommand /tmp/x.sh --list",
        "git config --global core.sshCommand /tmp/x.sh --unset",
        "git config core.hooksPath /tmp/x --comment --get",
        "git config core.hooksPath --rename-section",
    ] {
        denied_for_every_role(c, RULE);
    }
    // A flag before the key is still a flag, after a verb too.
    for c in [
        "git config --unset core.hooksPath",
        "git config --get core.hooksPath",
        "git config get --all core.hooksPath",
        "git config unset --all core.hooksPath",
    ] {
        allowed_for_every_role(c);
    }
}

/// PR #28, C18: a `--` after the first operand is an operand, not the end of the flags. `git config
/// core.hooksPath --` writes the value `--`. The guard is a closed form after round 3: it lets a
/// call through for a key that redirects git only in a shape it reads exactly, and a `--` anywhere
/// in such a call is outside every shape.
#[test]
fn a_double_dash_in_a_call_that_names_a_redirect_key_is_denied() {
    for c in [
        "git config core.hooksPath --",
        "git config set core.hooksPath --",
        "git config core.sshCommand --",
        "git config --global core.sshCommand --",
        "git config remote.forge.pushurl --",
        "git config --global remote.forge.pushurl --",
        "git config remote.origin.proxy --",
        "git config -- core.hooksPath",
        "git config --global -- core.hooksPath",
        "git config --get core.hooksPath -- x",
        "git config --unset core.hooksPath --",
        "git config get core.hooksPath --",
        "git config core.hooksPath x --",
    ] {
        denied_for_every_role(c, RULE);
    }
}

/// PR #28, C18: one allowed form of each shape the guard reads exactly, for a key that redirects
/// git: a key alone, a read flag, a list flag, a removal flag, and the verbs `get`, `list` and
/// `unset`, each with the scope flags and the options that take a value.
#[test]
fn every_modeled_read_shape_stays_allowed() {
    for c in [
        "git config core.hooksPath",
        "git config --global core.sshCommand",
        "git config --local --file f remote.forge.pushurl",
        "git config --type bool core.hooksPath",
        "git config --get core.hooksPath",
        "git config --get-all remote.forge.pushurl",
        "git config --system --get-regexp core.hooksPath",
        "git config --worktree --get core.hooksPath",
        "git config --list",
        "git config -l",
        "git config --global --list",
        "git config --file f --list",
        "git config --unset core.hooksPath",
        "git config --global --unset-all remote.forge.pushurl",
        "git config --remove-section http",
        "git config get core.hooksPath",
        "git config get --all remote.forge.pushurl",
        "git config get --global core.sshCommand",
        "git config list",
        "git config list --global",
        "git config unset core.hooksPath",
        "git config unset --all core.hooksPath",
        "git config remove-section remote.forge",
    ] {
        allowed_for_every_role(c);
    }
}

/// PR #28, C18: a call that is outside every modeled shape is denied when any word names a key that
/// redirects git, so no later round has to guess which word is a flag.
#[test]
fn a_call_outside_every_modeled_shape_that_names_a_redirect_key_is_denied() {
    for c in [
        "git config --unknown-flag core.hooksPath x",
        "git config --global get core.hooksPath x",
        "git config --get --add core.hooksPath x",
        "git config --get --get-all core.hooksPath",
        "git config core.hooksPath x y",
        "git config set --global core.hooksPath x",
        "git config --null core.hooksPath x",
    ] {
        denied_for_every_role(c, RULE);
    }
    // No word names a key that redirects git: the write is not this rule's.
    for c in [
        "git config user.name x",
        "git config --global user.name x --",
        "git config set user.email a@b.c",
    ] {
        allowed_for_every_role(c);
    }
}

/// PR #28, C19: a word with an unquoted glob has no key text. The shell expands it to the names of
/// files in the seat's directory, which a seat can create, and git then sets the key. A call
/// outside the read shapes that has such a word is not readable, as a variable is not.
#[test]
fn a_glob_word_in_a_call_outside_the_read_shapes_is_denied() {
    for c in [
        "git config core.hooks[P]ath /evil",
        "git config core.hooks[P]ath",
        "git config *",
        "git config --global core.hooks[P]ath x",
        "git config --add core.hooks[P]ath x",
        "git config ali[a]s.x '!gh pr merge 1'",
        "git config alias.x '!gh pr merge'*",
        "git config --rename-section old htt[p]",
        "git config --rename-section ht* x",
        "git config --file * core.hooksPath x",
        "git config --bool core.hooks[P]ath",
        "git -c core.hooks[P]ath=x status",
        "git -c 'core.x=1' -c core.hooks[P]ath=x status",
        "git --config-env=core.hooks[P]ath=VAR status",
    ] {
        denied_for_every_role(c, "unparsed-command");
    }
}

/// PR #28, C19: a glob that no shape reads as a key stays allowed: after a read flag or a verb it
/// is an operand of a read, and git refuses a flag that the shell expands into it.
#[test]
fn a_glob_operand_of_a_read_and_a_quoted_glob_stay_allowed() {
    for c in [
        "git config --get-regexp core.*",
        "git config --unset core.hooks[P]ath",
        "git config get core.*",
        "git config --get-regexp 'core.*'",
        "git config user.name 'Foo*'",
        "git -c 'core.x=*' status",
    ] {
        allowed_for_every_role(c);
    }
}

/// PR #28, C20: a read of a key that redirects git with a type flag, `--no-includes`, a color
/// flag or a unique prefix of a flag stays allowed, as it was before the closed form. A write
/// with the same flags is still denied.
#[test]
fn a_read_with_a_type_flag_or_a_prefix_stays_allowed() {
    for c in [
        "git config --bool http.sslVerify",
        "git config --int core.hooksPath",
        "git config --bool-or-int http.sslVerify",
        "git config --bool-or-str http.sslVerify",
        "git config --path http.sslVerify",
        "git config --expiry-date core.hooksPath",
        "git config --no-includes http.sslVerify",
        "git config --no-type core.hooksPath",
        "git config --fixed-value --get core.hooksPath x",
        "git config --get --bool http.sslVerify",
        "git config --get-color core.hooksPath red",
        "git config --get-colorbool core.hooksPath",
        "git config --glo core.hooksPath",
        "git config --glo --get core.hooksPath",
        "git config --incl --get core.hooksPath",
    ] {
        allowed_for_every_role(c);
    }
    for c in [
        "git config --bool core.hooksPath x",
        "git config --path http.sslVerify x",
        "git config --no-includes http.sslVerify x",
        "git config --glo core.hooksPath x",
        "git config --int core.hooksPath x --",
    ] {
        denied_for_every_role(c, RULE);
    }
}

/// A short flag inside a cluster is a flag: `-ze` is `-z` and `--edit`.
#[test]
fn a_short_flag_in_a_cluster_is_read() {
    for c in ["git config -ze", "git config -ez", "git config -z -e"] {
        denied_for_every_role(c, RULE);
    }
    for c in [
        "git config -zl",
        "git config -lz",
        "git config -z --get core.hooksPath",
    ] {
        allowed_for_every_role(c);
    }
}

/// The controller's push reads `remote.<name>.pushurl` and `remote.<name>.proxy` of the global
/// config again after its check (PR #28, C1).
#[test]
fn a_push_url_or_proxy_key_of_a_remote_is_denied() {
    for c in [
        "git config remote.origin.pushurl /tmp/x.git",
        "git config --global remote.forge.pushurl /tmp/x.git",
        "git config --add remote.origin.pushurl /tmp/x.git",
        "git config remote.origin.proxy http://127.0.0.1:1",
        "git -c remote.origin.pushurl=x push origin work/u1",
    ] {
        denied_for_every_role(c, RULE);
    }
}

/// The options that take a value hide it from the guards, so a read or a removal that uses them is
/// still a read or a removal.
#[test]
fn a_read_that_uses_an_option_with_a_value_stays_allowed() {
    for c in [
        "git config --comment c --unset core.hooksPath",
        "git config --type bool --get core.hooksPath",
        "git config --get --default x core.hooksPath",
        "git config get --value p core.hooksPath",
        "git config get --url https://github.com/ http.proxy",
        "git config --file f --get core.hooksPath",
        "git config -f f --list",
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

/// A rename deny reason quotes the new section name, which the seat wrote. A `user:password@` in
/// that name must not reach the reason, as in the reason of a `-c` key.
#[test]
fn a_rename_reason_hides_a_credential_in_the_new_section() {
    let r = shipped("implementer");
    for c in [
        "git config --rename-section foo url.https://user:hunter2secret@evil/",
        "git config rename-section foo http.https://user:hunter2secret@evil/",
        "git config --rename-section foo credential.https://user:hunter2secret@evil/",
    ] {
        let d = bash(&r, c);
        assert_eq!(d.rule, RULE, "{c}: {d:?}");
        assert!(!d.reason.contains("hunter2secret"), "{c}: {}", d.reason);
        assert!(d.reason.contains("[redacted]"), "{c}: {}", d.reason);
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
