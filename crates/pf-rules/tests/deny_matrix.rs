//! AC-9: the deny matrix of ADR-0005 and the classes that a role file decides.

mod common;

use common::*;
use pf_rules::{Outcome, ToolCall, decide};
use pf_workflow::rules::Allow;

#[test]
fn a_merge_is_denied_for_every_role() {
    denied_for_every_role("gh pr merge 1", "gh-pr-merge");
    denied_for_every_role("gh pr merge 12 --squash --delete-branch", "gh-pr-merge");
    denied_for_every_role("gh -R org/repo pr merge 3", "gh-pr-merge");
}

#[test]
fn gh_api_with_any_method_but_get_is_denied_and_get_is_allowed() {
    for c in [
        "gh api -X POST repos/o/r/issues",
        "gh api repos/o/r/issues -X POST",
        "gh api --method DELETE repos/o/r/git/refs/heads/x",
        "gh api --method=PUT repos/o/r/x",
        "gh api -XPATCH repos/o/r/x",
        "gh api -X post repos/o/r/x",
        "gh api -f title=x repos/o/r/issues",
        "gh api -F body=@file repos/o/r/issues",
        "gh api --field a=1 repos/o/r/issues",
        "gh api --raw-field a=1 repos/o/r/issues",
        "gh api --input body.json repos/o/r/issues",
        "gh api -X GET -X POST repos/o/r/x",
    ] {
        denied_for_every_role(c, "gh-api-write");
    }
    for c in [
        "gh api repos/o/r/pulls",
        "gh api -X GET repos/o/r/pulls",
        "gh api -XGET repos/o/r/pulls",
        "gh api --method GET repos/o/r/pulls",
        "gh api -X GET -f state=open repos/o/r/pulls",
        "gh api repos/o/r/pulls --jq '.[0].number'",
        "gh api -H 'Accept: application/json' repos/o/r/pulls",
    ] {
        allowed_for_every_role(c);
    }
}

#[test]
fn force_push_in_every_spelling_is_denied_for_every_role() {
    for c in [
        "git push --force origin work/u1",
        "git push -f origin work/u1",
        "git push origin work/u1 --force",
        "git push --force-with-lease origin work/u1",
        "git push --force-with-lease=work/u1:abc origin work/u1",
        "git push --force-if-includes origin work/u1",
        "git push origin +work/u1",
        "git push origin +HEAD:work/u1",
        "git push -fu origin work/u1",
        "git push -uf origin work/u1",
        "git push origin +work/u1:work/u1",
    ] {
        denied_for_every_role(c, "force-push");
    }
}

#[test]
fn a_push_to_the_default_branch_is_denied_for_every_role() {
    for c in [
        "git push origin main",
        "git push -u origin main",
        "git push origin HEAD:main",
        "git push origin work/u1:main",
        "git push origin work/u1:refs/heads/main",
        "git push origin refs/heads/main",
        "git push --all origin",
        "git push --mirror origin",
    ] {
        denied_for_every_role(c, "push-default-branch");
    }
    // A bare push and `HEAD` use the current branch: on the default branch they are denied.
    let mut on_main = ctx();
    on_main.current_branch = "main".to_string();
    for r in every_role() {
        for c in ["git push", "git push origin HEAD", "git push origin"] {
            let d = bash_in(&r, &on_main, c);
            assert!(
                !d.is_allow() && d.rule == "push-default-branch",
                "{} {c}: {d:?}",
                r.role
            );
        }
    }
}

#[test]
fn branch_deletion_in_every_spelling_is_denied_for_every_role() {
    for c in [
        "git branch -d old",
        "git branch -D old",
        "git branch --delete old",
        "git branch -dr origin/old",
        "git branch -df old",
        "git push -d origin old",
        "git push --delete origin old",
        "git push origin :old",
        "git push origin work/u1 :old",
        "git update-ref -d refs/heads/old",
    ] {
        denied_for_every_role(c, "branch-delete");
    }
    allowed_for_every_role("git branch -a");
    allowed_for_every_role("git branch --list 'work/*'");
}

#[test]
fn no_verify_is_denied_for_every_role() {
    for c in [
        "git commit --no-verify -m x",
        "git commit -m x --no-verify",
        "git commit -n -m x",
        "git commit -nm x",
        "git commit -an -m x",
        "git push --no-verify origin work/u1",
        "git merge --no-verify x",
    ] {
        denied_for_every_role(c, "no-verify");
    }
    // `-m` takes the rest of its cluster as the message, so `-mn` is the message "n".
    allowed_for_every_role("git commit -mn");
    allowed_for_every_role("git commit -m 'no -n here'");
}

#[test]
fn an_ai_trailer_is_denied_in_each_quoting() {
    for c in [
        "git commit -m 'fix\n\nCo-Authored-By: Claude <noreply@anthropic.com>'",
        "git commit -m \"fix\n\nCo-Authored-By: Claude Sonnet <noreply@anthropic.com>\"",
        "git commit -m $'fix\\n\\nCo-Authored-By: Claude <noreply@anthropic.com>'",
        "git commit -m \"fix\" -m \"Co-Authored-By: Claude <noreply@anthropic.com>\"",
        "git commit -m \"$(cat <<'EOF'\nfix the thing\n\nCo-Authored-By: Claude <noreply@anthropic.com>\nEOF\n)\"",
        "git commit -F - <<'EOF'\nfix\n\nCo-Authored-By: Codex <noreply@openai.com>\nEOF",
        "git commit -m \"Co-Authored-\"\"By: Claude\"",
        "git commit -m \"co-authored-by: GitHub Copilot <x@y.z>\"",
        "git commit -m \"feat: x\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\"",
        "echo 'fix\n\nCo-Authored-By: Claude' | git commit -F -",
        "git tag -a v1 -m 'Co-Authored-By: Claude'",
    ] {
        denied_for_every_role(c, "ai-trailer");
    }
    // A human co-author and a message that only mentions the word are fine.
    allowed_for_every_role("git commit -m 'fix\n\nCo-Authored-By: Ada Lovelace <ada@example.com>'");
    allowed_for_every_role("git commit -m 'teach the parser about claude and codex names'");
}

#[test]
fn curl_and_wget_to_the_forge_api_are_denied_with_any_method() {
    for c in [
        "curl https://api.github.com/repos/o/r",
        "curl -s -X GET https://api.github.com/repos/o/r",
        "curl -X POST https://api.github.com/repos/o/r/issues -d '{}'",
        "curl -XDELETE https://API.GITHUB.COM/repos/o/r",
        "curl --url=https://api.github.com/user",
        "wget -qO- https://api.github.com/user",
        "wget --post-data=x https://api.github.com/repos/o/r",
        "curl \"https://api.github.com/repos/$r\"",
    ] {
        denied_for_every_role(c, "forge-api-curl");
    }
    allowed_for_every_role("curl -s https://example.com/data.json");
}

#[test]
fn a_role_may_push_its_own_branch_and_no_other() {
    let close = shipped("close");
    for c in [
        "git push origin work/u1",
        "git push -u origin work/u1",
        "git push",
        "git push origin",
        "git push origin HEAD",
        "git push origin HEAD:work/u1",
        "git push origin work/u1:refs/heads/work/u1",
        "git -C /work/u1 push origin work/u1",
        "cd /work/u1 && git push origin work/u1",
    ] {
        let d = bash(&close, c);
        assert!(d.is_allow() && d.rule == "push-own-branch", "{c}: {d:?}");
    }
    for c in [
        "git push origin work/other",
        "git push origin other:other",
        "git push origin work/u1 work/other",
    ] {
        let d = bash(&close, c);
        assert!(!d.is_allow() && d.rule == "push-other-branch", "{c}: {d:?}");
    }
    // A bare push on another branch is denied too.
    let mut elsewhere = ctx();
    elsewhere.current_branch = "work/other".to_string();
    for c in ["git push", "git push origin HEAD", "git push -u origin"] {
        let d = bash_in(&close, &elsewhere, c);
        assert!(!d.is_allow() && d.rule == "push-other-branch", "{c}: {d:?}");
    }
}

#[test]
fn a_tag_needs_push_own_tag_and_the_own_tag() {
    let tagger = role("tagger", &[Allow::PushOwnTag]);
    let d = bash(&tagger, "git push origin refs/tags/v-u1");
    assert!(d.is_allow() && d.rule == "push-own-tag", "{d:?}");
    let d = bash(&tagger, "git push origin refs/tags/v-other");
    assert!(!d.is_allow() && d.rule == "push-other-tag", "{d:?}");
    let d = bash(&tagger, "git push origin work/u1");
    assert!(
        !d.is_allow() && d.rule == "push-not-allowed",
        "a tag role does not push branches: {d:?}"
    );
    let d = bash(&shipped("close"), "git push origin refs/tags/v-u1");
    assert!(!d.is_allow() && d.rule == "push-not-allowed", "{d:?}");
    let d = bash(&tagger, "git push --tags origin");
    assert!(!d.is_allow(), "{d:?}");
}

#[test]
fn implementer_and_verifier_cannot_push_and_close_can() {
    for name in ["implementer", "verifier", "lane"] {
        let r = shipped(name);
        let d = bash(&r, "git push origin work/u1");
        assert!(
            !d.is_allow() && d.rule == "push-not-allowed",
            "{name}: {d:?}"
        );
        let d = bash(&r, "git push");
        assert!(
            !d.is_allow() && d.rule == "push-not-allowed",
            "{name}: {d:?}"
        );
    }
    let d = bash(&shipped("close"), "git push origin work/u1");
    assert!(d.is_allow(), "{d:?}");
}

#[test]
fn a_quoted_variable_target_is_allowed_for_push_own_branch_only() {
    let close = shipped("close");
    for c in [
        "git push origin \"$b\"",
        "git push -u origin \"$wi_branch\"",
        "git -C \"$wt\" push -u origin \"$wi_branch\"",
        "git push \"$remote\" \"${branch}\"",
    ] {
        let d = bash(&close, c);
        assert!(
            d.is_allow() && d.rule == "own-branch-unverified",
            "{c}: {d:?}"
        );
    }
    let implementer = shipped("implementer");
    let d = bash(&implementer, "git push origin \"$b\"");
    assert!(!d.is_allow() && d.rule == "push-not-allowed", "{d:?}");
    // These forms change the meaning of the variable or hide an option.
    for c in [
        "git push origin \"+$b\"",
        "git push origin \":$b\"",
        "git push origin $b",
        "git push origin $F",
        "git push $F origin work/u1",
        "git push origin \"$b:$b\"",
        "git push origin \"$(git branch --show-current)\"",
        "git push origin ${b}",
    ] {
        let d = bash(&close, c);
        assert!(!d.is_allow() && d.rule == "push-unreadable", "{c}: {d:?}");
    }
}

#[test]
fn delete_is_judged_on_the_target() {
    let implementer = shipped("implementer");
    let close = shipped("close");
    for (r, c) in [
        (&implementer, "rm -rf /scratch/u1/tmp"),
        (&implementer, "rm -f /scratch/u1/a /scratch/u1/b"),
        (&implementer, "rm -rf /build/u1/debug"),
        (&implementer, "rm -rf /scratch/u1/*"),
        (&implementer, "rm -rf /scratch/u1"),
        (&close, "rm -rf /scratch/u1/tmp"),
        (&implementer, "rm -rf -- /scratch/u1/x"),
        (&implementer, "rm /scratch/u1/../u1/x"),
        // `r0.s1.w5`: a path inside the worktree is a seat's normal work, as `git rm` is.
        (&implementer, "rm -rf /work/u1/src"),
        (&close, "rm src/old.rs"),
    ] {
        let d = bash(r, c);
        assert!(d.is_allow(), "{} {c}: {d:?}", r.role);
    }
    for c in [
        "rm -rf ../other",
        "rm -rf \"$x\"",
        "rm -rf $x",
        "rm -rf /scratch/u1/../u2",
        "rm -rf /scratch/u1x/tmp",
        "rm -rf /",
        "rm -rf ~/scratch",
        "rm -rf /scratch/u1/$(date)",
        "rm -rf /scratch/u1/a /etc/passwd",
        "rm -rf *",
        "rm -rf /scratch/*/x",
        "find /work/u1 -delete",
        "find /scratch/u1 -exec rm -rf /etc \\;",
        "ls /scratch/u1 | xargs rm -rf",
        "git clean -fdx",
        "git worktree remove /work/u2",
    ] {
        let d = bash(&implementer, c);
        assert!(!d.is_allow(), "{c} should be denied: {d:?}");
    }
    // The close role has no build-target listing.
    let d = bash(&close, "rm -rf /build/u1/debug");
    assert!(
        !d.is_allow() && d.rule == "delete-outside-own-paths",
        "{d:?}"
    );
    // A role that lists no delete value may not delete at all.
    let d = bash(&role("none", &[]), "rm -rf /scratch/u1/tmp");
    assert!(!d.is_allow() && d.rule == "delete-not-allowed", "{d:?}");
    // Deletes that stay inside the own paths, through find and git.
    for c in [
        "find /scratch/u1 -name '*.tmp' -delete",
        "find /scratch/u1 -type f -exec rm {} \\;",
        "git -C /scratch/u1 clean -fd",
        "git clean -n",
        "git worktree remove /scratch/u1/wt",
    ] {
        let d = bash(&implementer, c);
        assert!(d.is_allow(), "{c}: {d:?}");
    }
}

#[test]
fn write_tools_follow_the_worktree_and_the_listing() {
    let implementer = shipped("implementer");
    let nothing = role("nothing", &[]);
    let call = |tool: &str, path: &str| ToolCall::write(tool, path);
    // Inside the worktree is a seat's normal work, listed or not.
    for r in [&implementer, &nothing] {
        for tool in ["write", "edit", "Write", "multiedit", "notebook_edit"] {
            let d = decide(r, &ctx(), &call(tool, "/work/u1/src/main.rs"));
            assert!(
                d.is_allow() && d.rule == "write-in-worktree",
                "{tool}: {d:?}"
            );
        }
        let d = decide(r, &ctx(), &call("write", "src/lib.rs"));
        assert!(d.is_allow(), "a relative path is read from the cwd: {d:?}");
    }
    // Outside it needs the listing and a path inside the seat's own paths.
    let d = decide(
        &implementer,
        &ctx(),
        &call("write", "/reports/u1/report.md"),
    );
    assert!(
        d.is_allow() && d.rule == "write-own-report-and-run-files",
        "{d:?}"
    );
    let d = decide(&implementer, &ctx(), &call("edit", "/run/u1/state/x.json"));
    assert!(d.is_allow(), "{d:?}");
    let d = decide(&nothing, &ctx(), &call("write", "/reports/u1/report.md"));
    assert!(!d.is_allow() && d.rule == "write-outside-worktree", "{d:?}");
    for path in [
        "/etc/passwd",
        "/work/u2/x",
        "/work/u1/../u2/x",
        "/run/u1/../u2/x",
        "/reports/u1/other.md",
        "../u2/x",
        "/work/u1x/a",
    ] {
        let d = decide(&implementer, &ctx(), &call("write", path));
        assert!(
            !d.is_allow() && d.rule == "write-outside-worktree",
            "{path}: {d:?}"
        );
    }
    // A read tool is not limited.
    let d = decide(&nothing, &ctx(), &call("read", "/etc/passwd"));
    assert!(d.is_allow(), "{d:?}");
    assert_eq!(d.outcome, Outcome::Allow);
}

#[test]
fn the_shipped_role_files_load_and_decide() {
    for name in SHIPPED_ROLES {
        let r = shipped(name);
        assert_eq!(r.role, name);
        // The deny list holds for each shipped role.
        assert!(!bash(&r, "gh pr merge 1").is_allow());
        assert!(!bash(&r, "git push --force origin work/u1").is_allow());
        assert!(
            bash(&r, "cargo test --workspace").is_allow(),
            "a seat needs a shell"
        );
    }
    assert!(!shipped("implementer").allow.contains(&Allow::PushOwnBranch));
    assert!(!shipped("verifier").allow.contains(&Allow::PushOwnBranch));
    assert!(shipped("close").allow.contains(&Allow::PushOwnBranch));
}

#[test]
fn other_commands_that_are_not_on_the_deny_list_are_allowed() {
    allowed_for_every_role("cargo build --workspace --locked");
    allowed_for_every_role("git status --porcelain && git diff --stat");
    allowed_for_every_role("git commit -m 'r0.s1.w3: add the adapter'");
    allowed_for_every_role("git log --oneline | head -5");
    allowed_for_every_role("ls -la /tmp");
    allowed_for_every_role("true && true || true; :");
}
