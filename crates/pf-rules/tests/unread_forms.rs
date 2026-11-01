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
fn a_compound_command_after_a_wrapper_does_not_hide_its_body() {
    for c in [
        "time while gh pr merge 1; do :; done",
        "time until gh pr merge 1; do :; done",
        "time if gh pr merge 1; then :; fi",
        "time -p if gh pr merge 1; then :; fi",
        "time ! gh pr merge 1",
        "nice while gh pr merge 1; do :; done",
        "time { gh pr merge 1; }",
    ] {
        denied_for_every_role(c, "gh-pr-merge");
    }
    denied_for_every_role(
        "time for i in 1 2; do git push --force origin x; done",
        "force-push",
    );
    allowed_for_every_role("time while read l; do echo $l; done < f");
    allowed_for_every_role("time if true; then echo hi; fi");
    allowed_for_every_role("time ! false");
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
    // The quoted braces of a git or gh word stay one word: the subcommand is `{merge,x}`.
    allowed_for_every_role("gh pr '{merge,x}' 1");
    allowed_for_every_role("gh pr \"{merge,x}\" 1");
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
        "setarch x86_64 gh pr merge 1",
        "linux32 gh pr merge 1",
        "prlimit --nofile=64 gh pr merge 1",
        "choom -n 0 -- gh pr merge 1",
        "numactl -C 0 gh pr merge 1",
        "cpulimit -l 50 -- gh pr merge 1",
        "xvfb-run gh pr merge 1",
        "torsocks gh pr merge 1",
        "proxychains4 gh pr merge 1",
        "ltrace gh pr merge 1",
        "sg grp -c 'gh pr merge 1'",
        "chroot / gh pr merge 1",
        "pkexec gh pr merge 1",
        "fakeroot gh pr merge 1",
        "bwrap --ro-bind / / gh pr merge 1",
        "firejail gh pr merge 1",
        "cgexec -g cpu:g gh pr merge 1",
        "eatmydata gh pr merge 1",
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

#[test]
fn a_config_that_runs_a_command_is_denied_however_it_is_given() {
    for c in [
        // An alias or a command key through the environment.
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.x GIT_CONFIG_VALUE_0='!gh pr merge 1' git x",
        "env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=alias.x GIT_CONFIG_VALUE_0='!gh pr merge 1' git x",
        "export GIT_CONFIG_COUNT=1; git x",
        "GIT_CONFIG_PARAMETERS=\"'alias.x=!gh pr merge 1'\" git x",
        // A value that the rules cannot read.
        "git -c \"$cfg\" x",
        "git -c \"alias.$n=status\" x",
        // A key that runs a command.
        "git -c core.fsmonitor='gh pr merge 1' status",
        "git -c core.sshCommand='gh pr merge 1' fetch",
        "git -c core.pager='gh pr merge 1' log",
        "git -c core.editor='sh -c x' commit",
        "git -c credential.helper='!gh pr merge 1' fetch",
        "git -c diff.x.textconv='gh pr merge 1' diff",
        "git -c pager.log='gh pr merge 1' log",
        "git --config-env core.pager=VAR log",
        "git --config-env=core.fsmonitor=VAR status",
        "git --config-env alias.x=VAR x",
        "git config alias.x status",
        "git config --add alias.x status",
        "git config --file f alias.x status",
    ] {
        denied_for_every_role(c, "unparsed-command");
    }
    // Controls: a key that runs nothing, a command key with a bare program or no value, and
    // a read or a removal of an alias.
    for c in [
        "git -c user.name=x log",
        "git -c color.ui=always log",
        "git -c core.pager=cat log",
        "git -c core.pager=less log",
        "git -c core.fsmonitor= status",
        "git -c core.pager log",
        "git config --get alias.x",
        "git config --get-all alias.x",
        "git config --get-regexp '^alias[.]'",
        "git config --unset alias.x",
        "git config --global --unset-all alias.x",
        "git config --list",
        "git config alias.x",
        "GIT_CONFIG_GLOBAL=/dev/null git log",
        "GIT_PAGER=cat git log",
    ] {
        allowed_for_every_role(c);
    }
}

#[test]
fn a_brace_that_mixes_quoted_text_hides_no_flag() {
    for c in [
        "git commit \"--no-ver\"{ify,} -m x",
        "git commit '--no-ver'{ify,} -m x",
        "git push origin work/u1 \"--for\"{ce,}",
        "git commit -m x \"--no-verify\"{,}",
        "nice git commit \"--no-ver\"{ify,} -m x",
        "echo \"x\"{a,b}",
        "rm \"-r\"{f,} /x",
    ] {
        denied_for_every_role(c, "unparsed-command");
    }
    // Controls: a variable beside a brace is dynamic and no more; quoted braces are one word.
    allowed_for_every_role("mkdir -p \"$d\"/{a,b}");
    allowed_for_every_role("echo '{a,b}'x \"{c,d}\"y");
    allowed_for_every_role("git commit -m '{a,b}' --amend");
}

#[test]
fn a_case_pattern_or_an_array_value_is_not_a_command() {
    for c in [
        "case \"$f\" in\n *.c|*.h) echo src ;;\nesac",
        "case \"$f\" in\n *.c|*.h|*.o) echo src ;;\n *) echo other ;;\nesac",
        "case $f in (a*|b*) echo x ;; (*) echo y ;; esac",
        "case $f in a*) echo x ;; b*|c*) echo y ;;& *) echo z ;; esac",
        "files=(*.rs); echo done",
        "declare -a files=(*.rs *.toml)",
        "local -a f=(src/*.rs)",
        "files+=(*.rs)",
        "for f in *.c; do echo $f; done",
    ] {
        allowed_for_every_role(c);
    }
    // The body of an arm and a substitution inside an array are still judged.
    denied_for_every_role(
        "case \"$f\" in\n *.c|*.h) gh pr merge 1 ;;\nesac",
        "gh-pr-merge",
    );
    denied_for_every_role(
        "case $f in a*|b*) echo x ;; *.c|*.h) git push --force origin x ;; esac",
        "force-push",
    );
    denied_for_every_role("files=($(gh pr merge 1))", "gh-pr-merge");
    // A glob that is a real command name is still denied.
    denied_for_every_role("/usr/bin/g* pr merge 1", "unparsed-command");
    denied_for_every_role("echo ok; *.sh", "unparsed-command");
}

#[test]
fn a_brace_sequence_of_any_size_ends() {
    // Each of these once looped for ever, or panicked, on an `i64` overflow.
    for c in [
        "echo {0..9223372036854775807..9223372036854775807}",
        "echo {1..3..9223372036854775807}",
        "echo {a..z..9223372036854775807}",
        "echo {-9223372036854775808..9223372036854775807}",
        "echo {1..3..-9223372036854775808}",
        "echo {1..3..99999999999999999999}",
    ] {
        allowed_for_every_role(c);
    }
}
