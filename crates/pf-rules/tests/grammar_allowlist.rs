//! AC-1 (`r0.s2.w5`): the rules read a closed set of shell forms. Every form in the grammar has an
//! allowed sample, and every form outside it is denied as `unparsed-command` with the form named.
//! A form that nobody listed is denied too, so a new unread spelling fails closed with no new code.

mod common;

use common::{bash, shipped};
use pf_rules::grammar::{ALLOWED, DENIED};
use pf_workflow::rules::RoleRules;

/// The two roles that the spec judges the samples for.
fn roles() -> [RoleRules; 2] {
    [shipped("implementer"), shipped("close")]
}

fn allowed(command: &str) {
    for r in roles() {
        let d = bash(&r, command);
        assert!(
            d.is_allow(),
            "role {}: {command:?} -> {} ({})",
            r.role,
            d.rule,
            d.reason
        );
    }
}

/// The line is denied as `unparsed-command`, and the reason names the form.
fn unread(command: &str, form: &str) {
    for r in roles() {
        let d = bash(&r, command);
        assert!(
            !d.is_allow() && d.rule == "unparsed-command",
            "role {}: {command:?} -> {:?} {} ({})",
            r.role,
            d.outcome,
            d.rule,
            d.reason
        );
        assert!(
            d.reason.contains(form),
            "role {}: {command:?}: the reason does not name {form:?}: {}",
            r.role,
            d.reason
        );
    }
}

/// The line is denied by a guard, with the rule of that guard.
fn guarded(command: &str, rule: &str) {
    for r in roles() {
        let d = bash(&r, command);
        assert!(
            !d.is_allow() && d.rule == rule,
            "role {}: {command:?} -> {:?} {} ({})",
            r.role,
            d.outcome,
            d.rule,
            d.reason
        );
    }
}

#[test]
fn each_allowed_form_has_an_allowed_sample() {
    for c in [
        // simple command
        "echo hi",
        "X=1 Y=2 ls -l > out.txt 2>&1",
        // pipeline
        "ls | sort | head -3",
        "! false",
        "ls |& cat",
        // list
        "true && echo a || echo b; echo c\necho d",
        "sleep 1 &",
        // if
        "if true; then echo a; elif false; then echo b; else echo c; fi",
        // for
        "for f in a b c; do echo \"$f\"; done",
        // while and until
        "while read -r l; do echo \"$l\"; done < f",
        "until false; do :; done",
        // case
        "case $x in a|b) echo ab ;; *) echo other ;; esac",
        // group
        "{ echo a; echo b; }",
        // subshell, and a subshell inside a substitution
        "(echo a; echo b)",
        "(case $f in *.c) echo c ;; *.h) echo h ;; esac)",
        "x=$( (ls) )",
        // function
        "function f { echo hi; }; f",
        "f() { echo hi; }; f",
        // coproc with a body
        "coproc { echo hi; }",
        "coproc ( echo hi )",
        "coproc while read l; do echo $l; done",
        // array literal
        "a=(1 2); echo done",
        "declare -a b=(x y)",
        "local -a c=(z)",
        "a+=(3)",
        // conditional, test
        "[[ -n \"$X\" ]] && echo set",
        "[ -f x ] && test -d y",
        // time
        "time echo hi",
        "time { echo hi; }",
        // wrapper
        "nice -n 5 ionice -c3 timeout 5 env A=1 cargo test",
        // literal word
        "echo 'a b' \"c d\" {a,b} *.rs",
        // variable
        "echo $HOME ${HOME} ${#HOME} $? $$ $1",
        // default word
        "echo \"${OSS_STATE_FILE:-}\" ${a:-b} ${a-b} ${a:+b} ${a+b}",
        "echo ${a:-${b:-c}} \"${a:-$(date)}\"",
        // arithmetic expansion
        "n=$((n+1))",
        "echo $((1 + 2 * (3 % 2)))",
        // command substitution
        "x=$(git rev-parse HEAD)",
        "echo `date`",
        // process substitution
        "diff <(ls a) <(ls b)",
        "cat < <(echo x)",
        // comment
        "echo ok # a comment with `backticks`",
        // redirection
        "echo a > f; echo b >> f; cat < f; ls 2>&1; ls &> o; echo x >&2; ls > /dev/null 2>&1",
        // heredoc and herestring
        "cat <<'EOF'\nhi\nEOF",
        "cat <<< \"hi\"",
        // trap
        "trap 'echo bye' EXIT",
        // eval
        "eval ls",
        "eval 'echo hi'",
    ] {
        allowed(c);
    }
}

#[test]
fn each_denied_form_is_denied_as_unparsed_command_with_the_form_named() {
    for (c, form) in [
        // The samples of the operator's ruling of 2026-11-03.
        ("eval \"$x\"", "eval"),
        ("eval $(cat f)", "eval"),
        ("coproc NAME cat", "coproc"),
        ("echo ${!x}", "${!x}"),
        ("echo $'\\x41'", "$'\\x41'"),
        ("echo ${x/a/b}", "${x/a/b}"),
        ("echo ${x:=y}", "${x:=y}"),
        ("echo $((x[1]))", "$((x[1]))"),
        ("select x in a; do :; done", "select"),
        // The forms of Approach 1 that no landed test reads as allowed.
        ("((x++))", "((x++))"),
        ("exec bash", "exec"),
        ("source f", "source"),
        (". f", "source"),
        ("echo $[1+2]", "$[1+2]"),
        ("echo ${x%y}", "${x%y}"),
        ("echo $\"x\"", "$\"x\""),
        ("for ((i=0; i<3; i++)); do :; done", "C-style"),
        ("python3 <<'EOF'\nprint(1)\nEOF", "python3"),
        ("perl <<< 'print 1'", "perl"),
        ("cat <<(ls)", "heredoc"),
        ("echo > ", "redirection"),
        // A substitution that holds a denied form is denied, and a trap body too.
        ("echo $(echo ${x:=y})", "${x:=y}"),
        ("echo \"$(echo ${x/a/b})\"", "${x/a/b}"),
        ("trap 'echo ${x:=y}' EXIT", "${x:=y}"),
        ("bash -c 'echo ${x:=y}'", "${x:=y}"),
        ("eval 'echo ${x:=y}'", "${x:=y}"),
        ("echo ${a:-${x:=y}}", "${x:=y}"),
    ] {
        unread(c, form);
    }
}

#[test]
fn a_substitution_that_holds_a_readable_form_is_allowed() {
    allowed("x=$(git rev-parse HEAD)");
    allowed("echo $(echo $((1+2)))");
}

#[test]
fn a_trap_body_that_merges_is_denied_by_its_guard() {
    guarded("trap 'gh pr merge 1' EXIT", "gh-pr-merge");
}

#[test]
fn a_subshell_body_meets_the_same_guards() {
    // The subshell is in the grammar. `rm` after a `cd` out of the worktree is the delete guard's
    // to deny, not the grammar's.
    for r in roles() {
        let d = bash(&r, "(cd .. && rm -rf x)");
        assert!(
            !d.is_allow() && d.rule.starts_with("delete-"),
            "role {}: {} ({})",
            r.role,
            d.rule,
            d.reason
        );
    }
    guarded("(gh pr merge 1)", "gh-pr-merge");
    guarded("(git push --force origin work/u1)", "force-push");
}

#[test]
fn a_guard_denies_first_and_the_grammar_denies_what_no_guard_denied() {
    // Both lines hold a form outside the grammar. The guard's rule stays.
    guarded("echo ${x:=y}; gh pr merge 1", "gh-pr-merge");
    guarded("select x in a; do gh pr merge 1; done", "gh-pr-merge");
    // A capability allow does not let a form outside the grammar through.
    let close = shipped("close");
    let d = bash(&close, "git push origin work/u1");
    assert!(d.is_allow() && d.rule == "push-own-branch", "{d:?}");
    let d = bash(&close, "git push origin work/u1; echo ${x:=y}");
    assert!(
        !d.is_allow() && d.rule == "unparsed-command" && d.reason.contains("${x:=y}"),
        "{d:?}"
    );
    let implementer = shipped("implementer");
    let d = bash(&implementer, "x=\"$(mktemp)\"; rm -f \"$x\"");
    assert!(d.is_allow() && d.rule == "delete-own-scratch", "{d:?}");
    let d = bash(&implementer, "x=\"$(mktemp)\"; rm -f \"$x\"; echo ${y@P}");
    assert!(
        !d.is_allow() && d.rule == "unparsed-command" && d.reason.contains("${y@P}"),
        "{d:?}"
    );
}

#[test]
fn a_form_that_nobody_listed_fails_closed_with_no_new_code() {
    // Three lines that the grammar does not name anywhere.
    for (c, form) in [
        ("time coproc x", "coproc"),
        ("echo ${x@P}", "${x@P}"),
        ("echo ${x[@]}", "${x[@]}"),
    ] {
        unread(c, form);
    }
}

#[test]
fn the_default_word_is_read_as_a_command_line_would_be() {
    guarded("echo ${x:-$(gh pr merge 1)}", "gh-pr-merge");
    guarded("echo \"${x:-`gh pr merge 1`}\"", "gh-pr-merge");
    guarded("echo $(( 1 + $(gh pr merge 1) ))", "gh-pr-merge");
}

#[test]
fn the_contract_lists_every_form_of_the_grammar() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contracts/role-rules.md");
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    for f in ALLOWED.iter().chain(DENIED.iter()) {
        assert!(
            doc.contains(&format!("`{}`", f.name)),
            "docs/contracts/role-rules.md does not list the form `{}`",
            f.name
        );
    }
}

#[test]
fn each_form_of_the_grammar_has_a_comment_line() {
    // One comment line per form, in the code that holds the list.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/grammar.rs");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let lines: Vec<&str> = src.lines().collect();
    let mut forms = 0;
    for (k, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("form(") {
            forms += 1;
            let before = lines[..k]
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map_or("", |l| l.trim_start());
            assert!(
                before.starts_with("//") && !before.starts_with("///"),
                "the form at line {} has no comment line before it",
                k + 1
            );
        }
    }
    assert_eq!(forms, ALLOWED.len() + DENIED.len());
}
