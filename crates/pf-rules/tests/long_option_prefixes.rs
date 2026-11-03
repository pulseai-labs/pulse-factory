//! AC-2 (`r0.s2.w5`, canonical issue #8): git reads a long option by any unique prefix of it, so the
//! rules must too. A prefix is the option it abbreviates; a prefix that two guarded options share
//! is denied as `unparsed-command`, because git refuses it.

mod common;

use common::{allowed_for_every_role, bash, denied_for_every_role, shipped};
use pf_rules::options::{Resolved, resolve};

#[test]
fn a_unique_prefix_is_denied_with_the_rule_of_the_full_option() {
    let implementer = shipped("implementer");
    for (prefix, full, rule) in [
        (
            "git push --force-w origin b",
            "git push --force-with-lease origin b",
            "force-push",
        ),
        (
            "git push --force-i origin b",
            "git push --force-if-includes origin b",
            "force-push",
        ),
        (
            "git commit --no-verif -m x",
            "git commit --no-verify -m x",
            "no-verify",
        ),
        (
            "git commit --no-v -m x",
            "git commit --no-verify -m x",
            "no-verify",
        ),
        (
            "git merge --no-ver x",
            "git merge --no-verify x",
            "no-verify",
        ),
        (
            "git push --no-verif origin b",
            "git push --no-verify origin b",
            "no-verify",
        ),
        (
            "git push --delet origin b",
            "git push --delete origin b",
            "branch-delete",
        ),
        (
            "git push --mirr",
            "git push --mirror",
            "push-default-branch",
        ),
        (
            "git push --branch",
            "git push --branches",
            "push-default-branch",
        ),
        (
            "git push --tag origin",
            "git push --tags origin",
            "push-not-allowed",
        ),
        (
            "git branch --del x",
            "git branch --delete x",
            "branch-delete",
        ),
        (
            "git config --fil f alias.x '!gh pr merge 1'",
            "git config --file f alias.x '!gh pr merge 1'",
            "unparsed-command",
        ),
    ] {
        let p = bash(&implementer, prefix);
        let f = bash(&implementer, full);
        assert!(
            !p.is_allow() && p.rule == rule,
            "{prefix:?} -> {:?} {} ({})",
            p.outcome,
            p.rule,
            p.reason
        );
        assert_eq!(p.rule, f.rule, "{prefix:?} against {full:?}");
        // The same holds for every role: a deny does not depend on a listing.
        denied_for_every_role(prefix, rule);
    }
}

#[test]
fn a_prefix_that_two_guarded_options_share_is_denied_as_unparsed_command() {
    let implementer = shipped("implementer");
    for c in [
        "git push --f origin b",
        "git push --forc origin b",
        "git push --force- origin b",
        "git push --re origin b",
        "git config --unse alias.x",
        "git config --g alias.x",
    ] {
        let d = bash(&implementer, c);
        assert!(
            !d.is_allow() && d.rule == "unparsed-command",
            "{c:?} -> {:?} {} ({})",
            d.outcome,
            d.rule,
            d.reason
        );
        assert!(
            d.reason.contains("more than one option"),
            "{c:?}: {}",
            d.reason
        );
        denied_for_every_role(c, "unparsed-command");
    }
}

#[test]
fn an_exact_name_wins_over_the_longer_options_that_start_with_it() {
    // `--force` is also the start of `--force-with-lease`, and `--get` of `--get-all`.
    denied_for_every_role("git push --force origin b", "force-push");
    allowed_for_every_role("git config --get user.name");
    allowed_for_every_role("git config --unset user.name");
}

#[test]
fn a_word_that_is_no_prefix_of_a_guarded_option_is_left_alone() {
    // Controls: nothing here starts a guarded option.
    for c in [
        "git log --oneli",
        "git log --no-merges --oneline",
        "git diff --name-only",
        "git commit --amend --no-edit -m x",
        "git config --global user.name x",
        "git config --list",
        "git config --fil f user.name x",
        "git clean --dry-r -fdx",
        "git clean --dry -fdx",
        "git branch --list",
        "git status --short",
    ] {
        allowed_for_every_role(c);
    }
    // A push of the seat's own branch with options that no guard reads.
    let close = shipped("close");
    for c in [
        "git push --follow-tags origin work/u1",
        "git push --set-upstream origin work/u1",
        "git push --dry-run origin work/u1",
        "git push --atomic origin work/u1",
    ] {
        let d = bash(&close, c);
        assert!(d.is_allow(), "{c:?} -> {} ({})", d.rule, d.reason);
    }
}

#[test]
fn a_word_after_a_double_dash_is_an_operand_not_an_option() {
    // `--forc` after `--` names a path, as `git checkout -- <path>` does.
    allowed_for_every_role("git checkout -- --forc");
    allowed_for_every_role("git add -- --no-verif");
}

#[test]
fn a_prefix_in_a_value_taking_option_still_takes_its_value() {
    // `--repo` takes a value. If the prefix were not read, the value would pass for the remote.
    let close = shipped("close");
    let d = bash(&close, "git push --rep origin origin work/u1");
    assert!(d.is_allow(), "{} ({})", d.rule, d.reason);
    let d = bash(&close, "git push --rep=origin origin work/u1");
    assert!(d.is_allow(), "{} ({})", d.rule, d.reason);
    let d = bash(&close, "git push --exe x origin work/u1");
    assert!(d.is_allow(), "{} ({})", d.rule, d.reason);
}

#[test]
fn resolve_reads_exact_names_prefixes_and_ambiguity() {
    let names = ["force", "force-with-lease", "force-if-includes", "delete"];
    assert_eq!(resolve("force", &names), Resolved::Name("force"));
    assert_eq!(
        resolve("force-w", &names),
        Resolved::Name("force-with-lease")
    );
    assert_eq!(resolve("d", &names), Resolved::Name("delete"));
    assert_eq!(
        resolve("f", &names),
        Resolved::Ambiguous(vec!["force", "force-with-lease", "force-if-includes"])
    );
    assert_eq!(resolve("x", &names), Resolved::Other);
    assert_eq!(resolve("", &names), Resolved::Other);
    assert_eq!(resolve("forces", &names), Resolved::Other);
}

#[test]
fn each_option_list_constant_has_a_source_comment() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/options.rs");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let lines: Vec<&str> = src.lines().collect();
    let mut seen = 0;
    for (k, line) in lines.iter().enumerate() {
        if !(line.starts_with("pub const ") && line.contains("_OPTIONS")) {
            continue;
        }
        seen += 1;
        // The doc comment above the constant names its source: a man page section.
        let mut j = k;
        let mut doc = String::new();
        while j > 0 && lines[j - 1].trim_start().starts_with("///") {
            j -= 1;
            doc.push_str(lines[j]);
            doc.push('\n');
        }
        assert!(
            doc.contains("Source:") && doc.contains("(1)"),
            "{line} has no `Source:` line that names a man page"
        );
    }
    assert!(seen >= 5, "found only {seen} option lists");
}
