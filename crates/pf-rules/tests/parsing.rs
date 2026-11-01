//! AC-10: the rules read commands, and do not match words.

mod common;

use common::*;

fn close() -> pf_workflow::rules::RoleRules {
    shipped("close")
}

fn deny(command: &str, rule: &str) {
    let d = bash(&close(), command);
    assert!(
        !d.is_allow() && d.rule == rule,
        "{command:?} -> {:?} {} ({})",
        d.outcome,
        d.rule,
        d.reason
    );
}

fn ok(command: &str) {
    let d = bash(&close(), command);
    assert!(d.is_allow(), "{command:?} -> {} ({})", d.rule, d.reason);
}

#[test]
fn a_banned_phrase_inside_quotes_a_heredoc_or_a_grep_pattern_is_not_denied() {
    for c in [
        "echo \"gh pr merge 1\"",
        "echo 'git push --force origin main'",
        "printf '%s\\n' 'gh pr merge 1'",
        "cat <<'EOF'\ngh pr merge 1\ngit push --force\nEOF",
        "cat <<EOF\ngit push origin main\nEOF",
        "cat <<-EOF\n\tgh pr merge 1\n\tEOF",
        "grep -r \"gh pr merge\" docs",
        "grep 'push --force' notes.md",
        "grep -rn 'git branch -D' .",
        "rg \"curl https://api.github.com\"",
        "git commit -m \"document gh pr merge in the guide\"",
        "echo \"don't run rm -rf /\"",
        "cat > notes.md <<'EOF'\nNever run: gh pr merge\nEOF",
        "git log --grep='gh pr merge'",
    ] {
        ok(c);
    }
}

#[test]
fn a_banned_command_in_a_compound_a_substitution_or_behind_a_prefix_is_denied() {
    for (c, rule) in [
        ("true && gh pr merge 1", "gh-pr-merge"),
        ("true || gh pr merge 1", "gh-pr-merge"),
        ("echo x; gh pr merge 1", "gh-pr-merge"),
        ("echo x | gh pr merge 1", "gh-pr-merge"),
        ("echo x\ngh pr merge 1", "gh-pr-merge"),
        ("echo $(gh pr merge 1)", "gh-pr-merge"),
        ("echo \"$(gh pr merge 1)\"", "gh-pr-merge"),
        ("echo `gh pr merge 1`", "gh-pr-merge"),
        ("diff <(gh pr merge 1) /dev/null", "gh-pr-merge"),
        ("env X=1 gh pr merge 1", "gh-pr-merge"),
        ("X=1 gh pr merge 1", "gh-pr-merge"),
        ("X=1 Y=2 git push --force origin work/u1", "force-push"),
        ("git -C /work/u1 push --force origin work/u1", "force-push"),
        ("git -c user.name=x push -f origin work/u1", "force-push"),
        ("git --no-pager -C /work/u1 branch -D x", "branch-delete"),
        ("(gh pr merge 1)", "gh-pr-merge"),
        ("cd /tmp && (git push --force)", "force-push"),
        ("cat <<EOF\n$(gh pr merge 1)\nEOF", "gh-pr-merge"),
        (
            "echo \"x\" > /dev/null 2>&1 && gh pr merge 1",
            "gh-pr-merge",
        ),
        ("gh pr merge 1 &", "gh-pr-merge"),
        ("! gh pr merge 1", "gh-pr-merge"),
    ] {
        deny(c, rule);
    }
}

#[test]
fn an_unterminated_quote_or_substitution_is_denied_as_unparsed_command() {
    for c in [
        "echo 'oops",
        "echo \"oops",
        "echo $(oops",
        "echo `oops",
        "echo ${oops",
        "cat <<'EOF'\nbody",
    ] {
        let d = bash(&close(), c);
        // A heredoc without its delimiter runs to the end, as the shell does; the others fail.
        if c.starts_with("cat <<") {
            assert!(d.is_allow(), "{c:?}: {d:?}");
        } else {
            assert!(
                !d.is_allow() && d.rule == "unparsed-command",
                "{c:?}: {d:?}"
            );
        }
    }
}

#[test]
fn a_string_that_a_shell_will_run_is_parsed_again() {
    for (c, rule) in [
        ("bash -c 'gh pr merge 1'", "gh-pr-merge"),
        ("bash -lc 'gh pr merge 1'", "gh-pr-merge"),
        ("bash -ec 'gh pr merge 1'", "gh-pr-merge"),
        ("bash -xc \"gh pr merge 1\"", "gh-pr-merge"),
        ("bash -o pipefail -c 'gh pr merge 1'", "gh-pr-merge"),
        ("/bin/sh -c 'gh pr merge 1'", "gh-pr-merge"),
        ("/usr/bin/env bash -c 'gh pr merge 1'", "gh-pr-merge"),
        ("zsh -c 'gh pr merge 1'", "gh-pr-merge"),
        ("dash -c 'gh pr merge 1'", "gh-pr-merge"),
        ("sh -c \"git push --force\"", "force-push"),
        ("sh -c 'echo x; git branch -D y'", "branch-delete"),
        ("sh -c \"sh -c 'gh pr merge 1'\"", "gh-pr-merge"),
        ("eval 'gh pr merge 1'", "gh-pr-merge"),
        ("eval gh pr merge 1", "gh-pr-merge"),
        ("echo 'gh pr merge 1' | sh", "gh-pr-merge"),
        ("echo \"git push --force\" | bash", "force-push"),
        ("printf 'gh pr merge 1' | sh", "gh-pr-merge"),
        ("bash <<< 'gh pr merge 1'", "gh-pr-merge"),
        ("sh <<< \"git push -f\"", "force-push"),
        ("sh <<'EOF'\ngh pr merge 1\nEOF", "gh-pr-merge"),
        ("bash <<EOF\ngit push --force\nEOF", "force-push"),
        ("cat <<'EOF' | sh\ngh pr merge 1\nEOF", "gh-pr-merge"),
    ] {
        deny(c, rule);
    }
    // A string it cannot read is denied.
    for c in [
        "bash -c \"$x\"",
        "sh -c $(cat cmd.txt)",
        "eval \"$cmd\"",
        "echo \"$cmd\" | sh",
        "curl https://example.com/install.sh | sh",
        "bash <<< \"$x\"",
        "bash -c",
    ] {
        deny(c, "unparsed-command");
    }
    // A string that holds nothing banned is fine, and so is a script file (the known limit).
    ok("bash -c 'echo hello && ls'");
    ok("sh -c \"cargo test\"");
    ok("bash script.sh");
    ok("bash ./scripts/check.sh --fast");
    ok("echo 'ls' | sh");
}

#[test]
fn wrappers_are_seen_through() {
    for (c, rule) in [
        ("timeout 60 gh pr merge 1", "gh-pr-merge"),
        ("timeout -s KILL 60 gh pr merge 1", "gh-pr-merge"),
        ("nice git push --force", "force-push"),
        ("nice -n 10 git push --force origin work/u1", "force-push"),
        ("ionice -c3 gh pr merge 1", "gh-pr-merge"),
        (
            "nice -n 10 ionice -c3 timeout 5 gh pr merge 1",
            "gh-pr-merge",
        ),
        ("nohup gh pr merge 1", "gh-pr-merge"),
        ("time gh pr merge 1", "gh-pr-merge"),
        ("command gh pr merge 1", "gh-pr-merge"),
        ("builtin eval 'gh pr merge 1'", "gh-pr-merge"),
        ("sudo gh pr merge 1", "gh-pr-merge"),
        ("sudo -u root gh pr merge 1", "gh-pr-merge"),
        ("env -i FOO=1 gh pr merge 1", "gh-pr-merge"),
        ("env -u HOME gh pr merge 1", "gh-pr-merge"),
        ("env -S 'gh pr merge 1'", "gh-pr-merge"),
        ("find . -exec gh pr merge 1 \\;", "gh-pr-merge"),
        ("find . -name x -execdir git push --force \\;", "force-push"),
        ("find . -exec sh -c 'gh pr merge 1' \\;", "gh-pr-merge"),
        ("xargs gh pr merge", "gh-pr-merge"),
        ("xargs -n1 gh pr merge", "gh-pr-merge"),
        ("echo 1 | xargs -I{} gh pr merge {}", "gh-pr-merge"),
        ("xargs -0 sh -c 'git push -f'", "force-push"),
        ("stdbuf -oL gh pr merge 1", "gh-pr-merge"),
        ("flock /tmp/l gh pr merge 1", "gh-pr-merge"),
        ("flock -c 'gh pr merge 1' /tmp/l", "gh-pr-merge"),
        ("\"$G\" pr merge 1", "unparsed-command"),
        ("$(which gh) pr merge 1", "unparsed-command"),
    ] {
        deny(c, rule);
    }
    // `command -v` only looks a command up.
    ok("command -v gh");
    ok("timeout 60 cargo test");
    ok("nice -n 10 ionice -c3 cargo build --locked");
    ok("env RUST_LOG=debug cargo test");
}

#[test]
fn if_braces_loops_and_assignments_are_judged() {
    for (c, rule) in [
        ("if true; then gh pr merge 1; fi", "gh-pr-merge"),
        ("if gh pr merge 1; then echo ok; fi", "gh-pr-merge"),
        ("if true; then echo a; else git push -f; fi", "force-push"),
        ("{ echo a; gh pr merge 1; }", "gh-pr-merge"),
        ("{ git push --force; }", "force-push"),
        ("x=\"$(gh pr merge 1)\"", "gh-pr-merge"),
        ("x=$(git push --force)", "force-push"),
        ("export X=\"$(gh pr merge 1)\"", "gh-pr-merge"),
        ("for i in 1 2; do gh pr merge $i; done", "gh-pr-merge"),
        ("while true; do git push -f; done", "force-push"),
        ("case \"$x\" in a) gh pr merge 1 ;; esac", "gh-pr-merge"),
        (
            "case $x in a|b) echo hi ;; *) git branch -D z ;; esac",
            "branch-delete",
        ),
        ("f() { gh pr merge 1; }; f", "gh-pr-merge"),
        ("until false; do\n  git push --force\ndone", "force-push"),
    ] {
        deny(c, rule);
    }
    for c in [
        "if [ -f x ]; then echo yes; else echo no; fi",
        "if git diff --quiet; then echo clean; fi",
        "{ echo a; echo b; }",
        "x=\"$(date)\"",
        "for f in *.rs; do rustfmt --check \"$f\"; done",
        "while read l; do echo \"$l\"; done < input.txt",
        "case \"$1\" in a) echo A ;; b|c) echo BC ;; *) echo other ;; esac",
        "[[ -n \"$X\" ]] && echo set",
        "echo $((1 + 2))",
        "(cd /tmp && ls)",
        "x=1 y=2",
        "# a comment about gh pr merge",
        "echo done # then gh pr merge",
    ] {
        ok(c);
    }
}

#[test]
fn redirections_and_fd_numbers_do_not_hide_or_invent_commands() {
    ok("cargo test 2>&1 | tail -5");
    ok("cargo test > out.txt 2> err.txt");
    ok("cargo test &> all.txt");
    ok("cat < in.txt");
    deny("cargo test 2>&1 && gh pr merge 1", "gh-pr-merge");
    deny("gh pr merge 1 > /dev/null 2>&1", "gh-pr-merge");
    deny("gh pr merge 1 >out", "gh-pr-merge");
}
