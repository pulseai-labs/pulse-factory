//! AC-9: the rules that the real ossify blocks need (`r0.s1.w5`, Approach 8).
//!
//! The three long samples are copied from ossify 1.32.5, so no test reads a plugin cache. A deny
//! in one of them parks the seat that runs it.

mod common;

use std::path::PathBuf;

use common::{bash_in, ctx, role, shipped};
use pf_rules::{Context, Part, push_of};
use pf_workflow::rules::Allow;

/// From `skills/work-item/references/round-orchestration.md`, section 2b, the inventory read-out
/// (the block that starts `inv="$(mktemp)"`).
const INVENTORY_BLOCK: &str = r####"inv="$(mktemp)"; inv_rc=0
oss spine_inventory "<spine-id>" > "$inv" || inv_rc=$?
cat "$inv"; rm -f "$inv"
case "$inv_rc" in
  0) ;;
  3) echo "halt: the inventory above has halt rows - repair each one, then re-run /run-spine (it re-inventories); nothing was changed"; exit 1 ;;
  *) echo "halt: spine_inventory could not read the spine (rc $inv_rc) - state or usage is broken, not the spine; nothing was changed"; exit 1 ;;
esac
"####;

/// From `skills/work-item/references/round-orchestration.md`, section 2a, the fresh arm (the
/// block that starts `repo_list=""; repo_bases=""`).
const FRESH_ARM_BLOCK: &str = r####"(
repo_list=""; repo_bases=""
trap '[ -z "$repo_list" ] || rm -f -- "$repo_list"; [ -z "$repo_bases" ] || rm -f -- "$repo_bases"' EXIT
spine_branch="$(oss branch_name "<spine-id>" "<spine-slug>")"
repo_list="$(mktemp)" || exit 1; repo_bases="$(mktemp)" || exit 1
repos="$(oss get '.work_items[] | select(.spine=="<spine-id>" and .status != "abandoned") | .target_repo')" \
  || { echo "halt: cannot read the spine's work items from state"; exit 1; }
printf '%s\n' "$repos" | sort -u > "$repo_list"

# PASS 1 - CHECK every hosting repo. Mutate nothing. A halt here leaves every
# repo exactly as it was found.
while IFS= read -r repo; do
  [ -n "$repo" ] || continue
  root="$(oss repo_root "$repo")" || exit 1     # undeclared repo halts HERE
  porcelain="$(git -C "$root" status --porcelain)" || { echo "halt: cannot read status in $repo"; exit 1; }
  [ -z "$porcelain" ] || { echo "halt: $repo is dirty"; exit 1; }
  # A failed lookup is not absence, even when quiet --verify would return 1.
  ref_rc=0
  git -C "$root" show-ref --exists "refs/heads/$spine_branch" 2>/dev/null || ref_rc=$?
  # Git without --exists returns usage rc 129: retain the legacy probe only
  # there. Known limit: legacy rc 1 conflates absence and malformed loose refs.
  if [ "$ref_rc" = 129 ]; then
    ref_rc=0
    git -C "$root" show-ref --verify --quiet "refs/heads/$spine_branch" || ref_rc=$?
    [ "$ref_rc" != 1 ] || ref_rc=2
  fi
  case "$ref_rc" in
    0) git -C "$root" show-ref --verify --quiet "refs/heads/$spine_branch" \
         || { echo "halt: cannot read $spine_branch in $repo"; exit 1; } ;;
    2) ;; # absent (or ambiguous legacy rc 1)
    *) echo "halt: cannot read $spine_branch in $repo (git show-ref rc $ref_rc)"; exit 1 ;;
  esac
  if [ "$ref_rc" = 0 ]; then
    echo "halt: $spine_branch already exists in $repo - this spine has started; take the re-entry arm (§2b), not this block."; exit 1
  fi
  base_branch="$(git -C "$root" rev-parse --abbrev-ref HEAD)"
  [ "$base_branch" != "HEAD" ] || { echo "halt: $repo is in DETACHED HEAD"; exit 1; }
  printf '%s\t%s\n' "$repo" "$base_branch" >> "$repo_bases"
done < "$repo_list"

# RECORD - every repo passed; record each base in state BEFORE any cut, so a
# crash inside PASS 2 always leaves the base the re-entry arm's `cut-missing`
# repair needs.
while IFS="$(printf '\t')" read -r repo base_branch; do
  [ -n "$repo" ] || continue
  oss spine_base_set "<spine-id>" "$repo" "$base_branch" || exit 1
done < "$repo_bases"

# PASS 2 - cut, in the same order.
while IFS="$(printf '\t')" read -r repo base_branch; do
  [ -n "$repo" ] || continue
  git -C "$(oss repo_root "$repo")" checkout -q -b "$spine_branch" || exit 1
done < "$repo_bases"
)
"####;

/// From `skills/close/references/work-item-close.md`, section 4a, the PR-mode block that pushes
/// the spine branch and the item's branch.
const PR_MODE_PUSH_BLOCK: &str = r####"target_repo="$(oss get ".work_items[] | select(.id==\"$wi\") | .target_repo")"
[ "$target_repo" != "ai_workspace" ] \
  || { echo "close: $wi targets 'ai_workspace', which is the process workspace, not a hosting repo - halt"; exit 1; }
repo_root="$(oss repo_root "$target_repo")" || { echo "close: $wi targets undeclared repo '$target_repo' - halt"; exit 1; }
wi_branch="$(oss get ".work_items[] | select(.id==\"$wi\") | .branch")"
[ -n "$wi_branch" ] && [ "$wi_branch" != "null" ] \
  || { echo "close: no recorded branch for $wi - halt"; exit 1; }
base_sha="$(oss get ".work_items[] | select(.id==\"$wi\") | .base_sha")"
[ -n "$base_sha" ] && [ "$base_sha" != "null" ] \
  || { echo "close: no recorded base_sha for $wi - halt"; exit 1; }
title="$(oss get ".work_items[] | select(.id==\"$wi\") | .title")"
spine_branch="$(oss branch_name "$spine_id" "$spine_slug")"
head_branch="$(git -C "$repo_root" symbolic-ref --short HEAD)"
[ "$head_branch" = "$spine_branch" ] \
  || { echo "close: $target_repo is on '$head_branch', not '$spine_branch' - halt"; exit 1; }

origin_url="$(git -C "$repo_root" remote get-url origin)" \
  || { echo "close: $target_repo has no 'origin' remote - PR mode needs one and merges nothing locally - halt"; exit 1; }
spine_rc=0
git -C "$repo_root" ls-remote --exit-code origin "refs/heads/$spine_branch" >/dev/null || spine_rc=$?
if [ "$spine_rc" -eq 2 ]; then
  git -C "$repo_root" push -u origin "$spine_branch" \
    || { echo "close: origin has no '$spine_branch' and pushing it failed - nothing was committed or pushed; PR mode merges nothing locally - halt"; exit 1; }
  echo "close: pushed '$spine_branch' to origin as the PR base - no PR opened for it; spine close opens that"
elif [ "$spine_rc" -ne 0 ]; then
  echo "close: cannot read '$spine_branch' on origin (ls-remote rc $spine_rc) - nothing was committed or pushed; PR mode merges nothing locally - halt"; exit 1
fi

# Commit, or recognise a resume: an empty index with the recorded branch
# checked out and HEAD already past base is this block's own earlier commit.
staged="$(git -C "$wt" diff --cached --name-only)"
wt_branch="$(git -C "$wt" symbolic-ref --short HEAD)"
[ "$wt_branch" = "$wi_branch" ] \
  || { echo "close: $wt is on '$wt_branch', not the recorded '$wi_branch' - nothing was committed or pushed - halt"; exit 1; }
if [ -n "$staged" ]; then
  git -C "$wt" commit -m "<message>" \
    || { echo "close: commit failed in $wt - nothing was pushed - halt"; exit 1; }
elif [ "$(git -C "$wt" rev-list --count "$base_sha..HEAD")" -ge 1 ]; then
  echo "close: index empty and '$wi_branch' is already past base $base_sha - resuming after the commit"
else
  echo "close: gate is green but the index is empty in $wt and HEAD is not past base on '$wi_branch' - nothing was committed or pushed - halt"; exit 1
fi
[ "$(git -C "$wt" rev-list --count "$base_sha..HEAD")" -ge 1 ] \
  || { echo "close: HEAD in $wt is not past base $base_sha - no commit to open a PR for; nothing was pushed - halt"; exit 1; }
git -C "$wt" merge-base --is-ancestor "$base_sha" HEAD \
  || { echo "close: recorded base $base_sha is not an ancestor of HEAD in $wt - '$wi_branch' was rewritten off its base; nothing was pushed - halt"; exit 1; }
git -C "$wt" push -u origin "$wi_branch" \
  || { echo "close: push of '$wi_branch' failed - the commit is on the local branch; this run opened and recorded no PR - re-run /close $wi --pr to resume - halt"; exit 1; }
pr_url="$(gh --repo "$origin_url" pr list --head "$wi_branch" --base "$spine_branch" --state open --json url --jq '.[0].url // ""')" \
  || { echo "close: gh pr list failed - '$wi_branch' is pushed, no PR was looked up or opened - re-run /close $wi --pr to resume - halt"; exit 1; }
if [ -z "$pr_url" ]; then
  pr_url="$(gh --repo "$origin_url" pr create --base "$spine_branch" --head "$wi_branch" \
    --title "$wi: $title" \
    --body "closed by ossify work-item close in PR mode; gate green; report at $report")" \
    || { echo "close: gh pr create failed - '$wi_branch' is pushed, no PR is recorded - re-run /close $wi --pr to resume - halt"; exit 1; }
else
  echo "close: '$wi_branch' already has an open PR into '$spine_branch' - reusing it, not opening another"
fi
oss work_item_pr "$wi" "$pr_url" \
  || { echo "close: PR $pr_url is open but state refused to record it - no pr_url is recorded - re-run /close $wi --pr to resume - halt"; exit 1; }
echo "close: $wi PR $pr_url"
"####;

/// A lane works from the method's root and has no worktree.
fn lane_ctx() -> Context {
    let mut c = ctx();
    c.cwd = PathBuf::from("/method");
    c.paths.worktree = PathBuf::new();
    c.own_branch = String::new();
    c.current_branch = String::new();
    c
}

/// A close seat works from the method's root. Its worktree and push directory are the item's.
fn close_at_root() -> Context {
    let mut c = ctx();
    c.cwd = PathBuf::from("/method");
    c.push_dir = PathBuf::from("/work/u1");
    c
}

fn allowed(r: &pf_workflow::rules::RoleRules, c: &Context, command: &str) {
    let d = bash_in(r, c, command);
    assert!(
        d.is_allow(),
        "{} {command:?} -> {} ({})",
        r.role,
        d.rule,
        d.reason
    );
}

fn denied(r: &pf_workflow::rules::RoleRules, c: &Context, command: &str) -> String {
    let d = bash_in(r, c, command);
    assert!(
        !d.is_allow(),
        "{} {command:?} should be denied: {} ({})",
        r.role,
        d.rule,
        d.reason
    );
    d.rule
}

#[test]
fn the_real_lane_blocks_are_allowed_for_lane() {
    let lane = shipped("lane");
    for (name, block) in [
        ("inventory", INVENTORY_BLOCK),
        ("fresh arm", FRESH_ARM_BLOCK),
    ] {
        let d = bash_in(&lane, &lane_ctx(), block);
        assert!(d.is_allow(), "{name}: {} ({})", d.rule, d.reason);
    }
    // They read as text: the inline copies still hold what the test is about.
    assert!(INVENTORY_BLOCK.starts_with("inv=\"$(mktemp)\""));
    assert!(FRESH_ARM_BLOCK.contains("repo_list=\"\"; repo_bases=\"\""));
    assert!(FRESH_ARM_BLOCK.contains("trap '"));
}

#[test]
fn the_real_close_push_block_is_allowed_for_close() {
    let close = shipped("close");
    let d = bash_in(&close, &close_at_root(), PR_MODE_PUSH_BLOCK);
    assert!(d.is_allow(), "{} ({})", d.rule, d.reason);
    // Its pushes name a directory and a branch in variables. The adapter must measure them.
    let pushes = push_of(PR_MODE_PUSH_BLOCK);
    assert_eq!(pushes.len(), 2, "{pushes:?}");
    for p in &pushes {
        assert!(matches!(p.branch, Part::Unresolved(_)), "{p:?}");
        assert!(matches!(p.dir, Part::Unresolved(_)), "{p:?}");
    }
    // The same block is not allowed for a role that may not push.
    let implementer = shipped("implementer");
    assert_eq!(
        denied(&implementer, &close_at_root(), PR_MODE_PUSH_BLOCK),
        "push-not-allowed"
    );
}

#[test]
fn a_delete_inside_the_worktree_is_normal_work() {
    for name in ["lane", "implementer", "verifier", "close"] {
        let r = shipped(name);
        allowed(&r, &ctx(), "rm src/old.rs");
        allowed(&r, &ctx(), "rm -f /work/u1/src/old.rs /work/u1/a/b.txt");
        allowed(&r, &ctx(), "rmdir src/empty");
        allowed(&r, &ctx(), "unlink src/old.rs");
        allowed(&r, &ctx(), "rm -rf target");
        allowed(&r, &ctx(), "rm *.o");
    }
    // A role with no listing at all gets it too.
    allowed(&role("none", &[]), &ctx(), "rm src/old.rs");
    // The worktree itself, its .git and a glob that may reach it are not.
    for name in ["implementer", "close"] {
        let r = shipped(name);
        for c in [
            "rm -rf .",
            "rm -rf .git",
            "rm -rf /work/u1",
            "rm -rf /work/u1/.git/objects",
            "rm -rf ./.git/hooks",
            "rm -rf *",
            "rm -rf .*",
            "rm -rf ./*",
        ] {
            denied(&r, &ctx(), c);
        }
    }
    // A lane has no worktree, so its files are not the worktree's.
    let lane = shipped("lane");
    assert_eq!(
        denied(&lane, &lane_ctx(), "rm .ossify/x"),
        "delete-outside-own-paths"
    );
    assert_eq!(
        denied(&lane, &lane_ctx(), "rm src/old.rs"),
        "delete-outside-own-paths"
    );
}

#[test]
fn a_relative_path_follows_each_literal_cd() {
    let implementer = shipped("implementer");
    allowed(&implementer, &ctx(), "cd src && rm old.rs");
    allowed(&implementer, &ctx(), "cd /work/u1/src && rm old.rs");
    denied(&implementer, &ctx(), "cd ../other && rm -rf .");
    denied(&implementer, &ctx(), "cd ../other && rm -rf x");
    denied(&implementer, &ctx(), "cd /etc && rm passwd");
    // A subshell is not told apart: the path must hold after every cd that could have run.
    denied(&implementer, &ctx(), "(cd src) ; rm ../../x");
    // After a cd to a variable, a relative path has no meaning. An absolute one does.
    assert_eq!(
        denied(&implementer, &ctx(), "cd \"$x\" && rm y"),
        "delete-unreadable"
    );
    denied(&implementer, &ctx(), "cd \"$x\" && rm -rf .");
    denied(&implementer, &ctx(), "pushd /work/u1 && rm y");
    denied(&implementer, &ctx(), "cd && rm y");
    allowed(&implementer, &ctx(), "cd \"$x\" && rm /work/u1/y");
}

#[test]
fn a_variable_from_mktemp_or_empty_is_deletable_only_for_delete_own_scratch() {
    let implementer = shipped("implementer");
    let close = shipped("close");
    let none = role("none", &[Allow::PushOwnBranch]);
    allowed(&implementer, &ctx(), "x=\"$(mktemp)\"; rm -f \"$x\"");
    allowed(&implementer, &ctx(), "x=$(mktemp -d); rm -rf \"$x\"");
    allowed(&implementer, &ctx(), "x=\"\"; rm -rf \"$x\"");
    // The assignment may sit after the use, and in a function or branch.
    allowed(
        &implementer,
        &lane_ctx(),
        "cleanup() { rm -f \"$t\"; }; t=\"$(mktemp)\"; cleanup",
    );
    // A role with no `delete_own_scratch` gets nothing from it.
    assert_eq!(
        denied(&none, &lane_ctx(), "x=\"$(mktemp)\"; rm -f \"$x\""),
        "delete-unreadable"
    );
    let _ = close;
    // Any other assignment anywhere in the call, in any order, ends it.
    for c in [
        "x=\"$(mktemp)\"; x=../other; rm -rf \"$x\"",
        "x=../other; x=\"$(mktemp)\"; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; rm -rf \"$x\"; x=../other",
        "for x in ../o; do rm -rf \"$x\"; done",
        "x=\"$(mktemp)\"; for x in ../o; do rm -rf \"$x\"; done",
        "x=\"$(mktemp)\"; read -r x; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; read -r a x; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; export x=../other; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; x+=/..; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; x=$(echo ../o); rm -rf \"$x\"",
        "x=\"$(mktemp ; echo ../o)\"; rm -rf \"$x\"",
        "x=\"\"; : \"${x:=../other}\"; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; printf -v x %s ../o; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; declare -n y=x; y=../o; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; eval 'x=../o'; rm -rf \"$x\"",
        "x=\"$(mktemp)\"; sh -c 'x=../o; rm -rf \"$x\"'",
        // Never assigned in the call: the variable may hold anything.
        "rm -rf \"$x\"",
        "rm -rf $x",
    ] {
        denied(&implementer, &lane_ctx(), c);
    }
    // The word must be the variable alone.
    for c in [
        "x=\"$(mktemp)\"; rm -rf \"$x/..\"",
        "x=\"$(mktemp)\"; rm -rf \"$x/other\"",
        "x=\"\"; rm -rf \"$x/home/dev\"",
        "x=\"$(mktemp)\"; rm -rf \"${x}/..\"",
        "x=\"$(mktemp)\"; rm -rf $x",
    ] {
        denied(&implementer, &lane_ctx(), c);
    }
}

#[test]
fn a_trap_body_is_read_as_a_command() {
    let lane = shipped("lane");
    let d = bash_in(&lane, &lane_ctx(), "trap 'gh pr merge 1' EXIT");
    assert!(!d.is_allow() && d.rule == "gh-pr-merge", "{d:?}");
    let d = bash_in(&lane, &lane_ctx(), "trap -- 'gh pr merge 1' EXIT");
    assert!(!d.is_allow() && d.rule == "gh-pr-merge", "{d:?}");
    let d = bash_in(&lane, &lane_ctx(), "trap \"$handler\" EXIT");
    assert!(!d.is_allow() && d.rule == "unparsed-command", "{d:?}");
    let d = bash_in(&lane, &lane_ctx(), "trap 'rm -rf /etc' EXIT");
    assert!(!d.is_allow(), "{d:?}");
    // The body may name a variable before its mktemp, as ossify's lane does.
    allowed(
        &lane,
        &lane_ctx(),
        "inv=\"\"; trap 'rm -f \"$inv\"' EXIT; inv=\"$(mktemp)\"",
    );
    allowed(
        &lane,
        &lane_ctx(),
        "inv=\"$(mktemp)\"; trap 'rm -f \"$inv\"' EXIT",
    );
    // A trap that only lists or resets runs nothing.
    allowed(&lane, &lane_ctx(), "trap -p");
    allowed(&lane, &lane_ctx(), "trap - EXIT");
    allowed(&lane, &lane_ctx(), "trap '' INT");
}

#[test]
fn a_bare_cargo_clean_is_denied_for_every_role() {
    for name in ["lane", "implementer", "verifier", "close"] {
        let r = shipped(name);
        for c in [
            "cargo clean",
            "cargo clean --release",
            "cargo +nightly clean",
            "nice -n 10 cargo clean",
            "cargo clean --target-dir /x",
        ] {
            let d = bash_in(&r, &ctx(), c);
            assert!(
                !d.is_allow() && d.rule == "cargo-clean",
                "{name} {c}: {d:?}"
            );
        }
        for c in [
            "cargo clean -p pf-rules",
            "cargo clean --package pf-rules",
            "cargo clean -p pf-rules -p pf-engine",
            "cargo clean --package=pf-rules",
            "cargo build --release",
            "mv target target.old",
        ] {
            allowed(&r, &ctx(), c);
        }
    }
    // `git clean` still needs a target in the scratch or the build target.
    let implementer = shipped("implementer");
    denied(&implementer, &ctx(), "git clean -fdx");
    denied(&implementer, &ctx(), "find . -delete");
    denied(&implementer, &ctx(), "git worktree remove /work/u2");
}

#[test]
fn a_push_from_the_worktree_with_a_variable_branch_is_unverified_for_close_only() {
    let close = shipped("close");
    let implementer = shipped("implementer");
    // From a working directory inside the worktree.
    let d = bash_in(&close, &ctx(), "git push origin \"$b\"");
    assert!(d.is_allow() && d.rule == "own-branch-unverified", "{d:?}");
    assert_eq!(
        denied(&implementer, &ctx(), "git push origin \"$b\""),
        "push-not-allowed"
    );
    // From the method's root, both are denied.
    let mut root = ctx();
    root.cwd = PathBuf::from("/method");
    assert_eq!(
        denied(&close, &root, "git push origin \"$b\""),
        "push-outside-worktree"
    );
    assert_eq!(
        denied(&implementer, &root, "git push origin \"$b\""),
        "push-not-allowed"
    );
    // The push directory counts as well as the worktree.
    let mut pd = root.clone();
    pd.push_dir = PathBuf::from("/repo");
    pd.cwd = PathBuf::from("/repo/sub");
    let d = bash_in(&close, &pd, "git push origin \"$b\"");
    assert!(d.is_allow() && d.rule == "own-branch-unverified", "{d:?}");
}

#[test]
fn a_push_is_judged_on_the_literal_half_and_the_unread_half_is_marked() {
    let close = shipped("close");
    let implementer = shipped("implementer");
    let c = ctx();
    // The literal branch decides.
    assert_eq!(
        denied(&close, &c, "git switch main && git push"),
        "push-default-branch"
    );
    assert_eq!(
        denied(&close, &c, "git checkout main && git push origin HEAD"),
        "push-default-branch"
    );
    assert_eq!(
        denied(&close, &c, "git switch other && git push"),
        "push-other-branch"
    );
    allowed(&close, &c, "git switch work/u1 && git push");
    allowed(
        &close,
        &c,
        "git switch -c work/u1 && git push -u origin HEAD",
    );
    assert_eq!(
        denied(&close, &c, "git -C ../other push"),
        "push-outside-worktree"
    );
    assert_eq!(
        denied(&close, &c, "git -C /elsewhere push origin work/u1"),
        "push-outside-worktree"
    );
    assert_eq!(
        denied(&close, &c, "cd ../other && git push origin work/u1"),
        "push-outside-worktree"
    );
    assert_eq!(
        denied(&implementer, &c, "git -C ../other push"),
        "push-not-allowed"
    );
    // A variable directory with a literal branch: the branch is judged as itself.
    assert_eq!(
        denied(&close, &c, "git -C \"$wt\" push origin other"),
        "push-other-branch"
    );
    assert_eq!(
        denied(&close, &c, "git -C \"$wt\" push origin main"),
        "push-default-branch"
    );
    let d = bash_in(&close, &c, "git -C \"$wt\" push origin work/u1");
    assert!(d.is_allow() && d.rule == "own-branch-unverified", "{d:?}");
    // A variable branch with the working directory.
    let d = bash_in(&close, &c, "git push origin \"$b\"");
    assert!(d.is_allow() && d.rule == "own-branch-unverified", "{d:?}");
    // A switch to a variable makes a bare push unverified, for close only.
    let d = bash_in(&close, &c, "git checkout \"$b\" && git push");
    assert!(d.is_allow() && d.rule == "own-branch-unverified", "{d:?}");
    let d = bash_in(&close, &c, "git switch \"$b\" && git push origin HEAD");
    assert!(d.is_allow() && d.rule == "own-branch-unverified", "{d:?}");
    assert_eq!(
        denied(&implementer, &c, "git checkout \"$b\" && git push"),
        "push-not-allowed"
    );
    // A cd to a variable makes the directory unread.
    let d = bash_in(&close, &c, "cd \"$wt\" && git push origin work/u1");
    assert!(d.is_allow() && d.rule == "own-branch-unverified", "{d:?}");
    // Restoring files switches nothing.
    allowed(&close, &c, "git checkout -- src/a.rs && git push");
    // Every landed deny still comes first.
    for cmd in [
        "git push --delete origin \"$b\"",
        "git push origin \":$b\"",
        "git push origin \"+$b\"",
        "git -C \"$wt\" push --force origin work/u1",
        "git switch work/u1 && git push -f",
    ] {
        denied(&close, &c, cmd);
    }
    assert_eq!(
        denied(&close, &c, "git push --delete origin \"$b\""),
        "branch-delete"
    );
    // `":$b"` cannot be read, and the landed deny for that comes first.
    assert_eq!(
        denied(&close, &c, "git push origin \":$b\""),
        "push-unreadable"
    );
}

#[test]
fn push_of_marks_each_form_that_the_rules_allow_unread() {
    let unread = |p: &pf_rules::PushIntent| {
        matches!(p.branch, Part::Unresolved(_)) || matches!(p.dir, Part::Unresolved(_))
    };
    for (cmd, want) in [
        ("git push origin \"$b\"", true),
        ("git -C \"$wt\" push origin work/u1", true),
        ("git checkout \"$b\" && git push", true),
        ("git switch \"$b\" && git push origin HEAD", true),
        ("cd \"$wt\" && git push origin work/u1", true),
        ("git push origin work/u1", false),
        ("git -C /work/u1 push origin work/u1", false),
        ("git switch work/u1 && git push", false),
        ("cd /work/u1 && git push origin work/u1", false),
    ] {
        let v = push_of(cmd);
        assert!(!v.is_empty(), "{cmd}");
        assert_eq!(v.iter().any(unread), want, "{cmd}: {v:?}");
    }
    // A bare push after a literal switch names that branch.
    let v = push_of("git switch work/u1 && git push");
    assert_eq!(v[0].branch, Part::Literal("work/u1".to_string()), "{v:?}");
}
