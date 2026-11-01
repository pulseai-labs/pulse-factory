//! Forms that the shell reads as one thing and a parser could read as another. A deny rule must
//! fire on the command the shell runs, or the call is denied as unread. Each deny has a control
//! that stays allowed.

mod common;

use common::{allowed_for_every_role, denied_for_every_role};

#[test]
fn a_function_or_group_keyword_does_not_hide_its_body() {
    for c in [
        "function f { gh pr merge 1; }; f",
        "function f() { gh pr merge 1; }; f",
        "function f ( ) { gh pr merge 1; }",
        "coproc n { gh pr merge 1; }",
        "coproc { gh pr merge 1; }",
        "coproc gh pr merge 1",
        "time { gh pr merge 1; }",
        "time -p { gh pr merge 1; }",
        "nice { gh pr merge 1; }",
    ] {
        denied_for_every_role(c, "gh-pr-merge");
    }
    denied_for_every_role("function f { git push --force origin x; }", "force-push");
    allowed_for_every_role("function f { echo hi; }; f");
    allowed_for_every_role("coproc n { echo hi; }");
    allowed_for_every_role("time { echo hi; }");
    allowed_for_every_role("time echo hi");
}

#[test]
fn a_brace_expansion_is_read_as_the_words_it_makes() {
    for c in [
        "gh pr {merge,x} 1",
        "gh pr merge{,} 1",
        "{gh,} pr merge 1",
        "gh pr {merge,view} 1",
    ] {
        denied_for_every_role(c, "gh-pr-merge");
    }
    denied_for_every_role("git push --for{ce,} origin work/u1", "force-push");
    denied_for_every_role("git push origin work/u1 --force{,}", "force-push");
    // A brace beside a quote or a variable cannot be read: a command name or a subcommand made
    // that way is denied.
    denied_for_every_role("\"g\"{h,x} pr merge 1", "unparsed-command");
    denied_for_every_role("$x{a,b} pr merge 1", "unparsed-command");
    // Controls: nothing to expand, or nothing a rule reads.
    allowed_for_every_role("echo {a,b} f{,.bak} {1..3} {a..c} x{01..03}");
    allowed_for_every_role("echo '{gh,x}' \"{a,b}\" {} {a} ${HOME} \\{a,b\\}");
    allowed_for_every_role("echo {a,{b,c}}d");
    allowed_for_every_role("echo {1..100000}x");
}

#[test]
fn a_glob_in_a_command_name_or_subcommand_is_not_readable() {
    for c in [
        "/usr/bin/g? pr merge 1",
        "/usr/bin/g* pr merge 1",
        "/usr/bin/[g]h pr merge 1",
    ] {
        denied_for_every_role(c, "unparsed-command");
    }
    denied_for_every_role("gh pr merg? 1", "unparsed-command");
    denied_for_every_role("gh p* merge 1", "unparsed-command");
    denied_for_every_role("git pus? origin work/u1", "unparsed-command");
    denied_for_every_role("x=merge; gh pr $x 1", "unparsed-command");
    // Controls: a glob in an argument is not a subcommand.
    allowed_for_every_role("ls /tmp/*.log");
    allowed_for_every_role("gh pr view 1");
    allowed_for_every_role("gh pr list --state all");
    allowed_for_every_role("git log --oneline");
}

#[test]
fn a_wrapper_that_the_rules_do_not_read_is_denied() {
    for c in [
        "taskset -c 0 gh pr merge 1",
        "chrt -f 1 gh pr merge 1",
        "setpriv --reuid 1 gh pr merge 1",
        "unshare -r gh pr merge 1",
        "nsenter -t 1 gh pr merge 1",
        "strace -f gh pr merge 1",
        "su -c 'gh pr merge 1'",
        "runuser -u x -- gh pr merge 1",
        "script -qc 'gh pr merge 1' /dev/null",
        "busybox sh -c 'gh pr merge 1'",
        "systemd-run --user gh pr merge 1",
        "watch gh pr merge 1",
        "parallel gh pr merge ::: 1",
        "env taskset -c 0 gh pr merge 1",
    ] {
        denied_for_every_role(c, "unparsed-command");
    }
    // The wrappers that are read still give their command to the rules.
    denied_for_every_role("nice -n 5 gh pr merge 1", "gh-pr-merge");
    allowed_for_every_role("nice -n 5 echo hi");
}

#[test]
fn an_alias_that_a_call_defines_is_denied() {
    for c in [
        "git -c alias.x='push --force' x origin work/u1",
        "git -c 'alias.x=!gh pr merge 1' x",
        "git -c ALIAS.x=status x",
        "git --config-env=alias.x=VAR x",
        "git config alias.x '!gh pr merge 1'",
        "git config --global alias.x 'push --force'",
        "git config --local Alias.x status",
        "gh alias set m 'pr merge'",
        "gh alias import file.yml",
    ] {
        denied_for_every_role(c, "unparsed-command");
    }
    allowed_for_every_role("git -c core.pager=cat log");
    allowed_for_every_role("git config user.name");
    allowed_for_every_role("gh alias list");
}
