//! AC-12: `push_of` finds each push with its remote and branch.

use pf_rules::{Part, PushIntent, push_of};

fn lit(s: &str) -> Part {
    Part::Literal(s.to_string())
}

fn un(s: &str) -> Part {
    Part::Unresolved(s.to_string())
}

fn one(command: &str) -> PushIntent {
    let v = push_of(command);
    assert_eq!(v.len(), 1, "{command:?} -> {v:?}");
    v.into_iter().next().unwrap()
}

#[test]
fn plain_compound_and_git_c_forms_give_one_push_with_remote_and_branch() {
    for (c, remote, branch, dir) in [
        (
            "git push origin topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "git push -u origin topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "git push --set-upstream origin topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "git push origin HEAD:topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "git push origin local:refs/heads/topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        ("git push", Part::Absent, Part::Absent, Part::Absent),
        ("git push origin", lit("origin"), Part::Absent, Part::Absent),
        (
            "cd /w && git push origin topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "git add -A && git commit -m x && git push origin topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "git push origin topic 2>&1 | tail -3",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "git -C /work/u1 push origin topic",
            lit("origin"),
            lit("topic"),
            lit("/work/u1"),
        ),
        (
            "git -C \"/work/u1\" push -u origin topic",
            lit("origin"),
            lit("topic"),
            lit("/work/u1"),
        ),
        (
            "timeout 60 git push origin topic",
            lit("origin"),
            lit("topic"),
            Part::Absent,
        ),
        (
            "nice -n 10 git -C /w push origin topic",
            lit("origin"),
            lit("topic"),
            lit("/w"),
        ),
    ] {
        let p = one(c);
        assert_eq!((p.remote, p.branch, p.dir), (remote, branch, dir), "{c}");
    }
}

#[test]
fn a_variable_is_marked_unresolved() {
    let p = one("git -C \"$wt\" push -u origin \"$wi_branch\"");
    assert_eq!(p.remote, lit("origin"));
    assert_eq!(p.branch, un("\"$wi_branch\""));
    assert_eq!(p.dir, un("\"$wt\""));
    let p = one("git push \"$remote\" topic");
    assert_eq!((p.remote, p.branch), (un("\"$remote\""), lit("topic")));
}

#[test]
fn a_push_in_a_bash_c_string_or_a_heredoc_fed_to_a_shell_is_found() {
    let p = one("bash -c 'git -C \"$wt\" push -u origin \"$b\"'");
    assert_eq!(
        (p.remote, p.branch, p.dir),
        (lit("origin"), un("\"$b\""), un("\"$wt\""))
    );
    let p = one("sh -lc \"git push origin topic\"");
    assert_eq!(p.branch, lit("topic"));
    let p = one("bash <<'EOF'\ncd \"$wt\"\ngit push -u origin \"$wi_branch\"\nEOF");
    assert_eq!((p.remote, p.branch), (lit("origin"), un("\"$wi_branch\"")));
    let p = one("sh <<< 'git push origin topic'");
    assert_eq!(p.branch, lit("topic"));
    let p = one("echo 'git push origin topic' | sh");
    assert_eq!(p.branch, lit("topic"));
    let p = one("eval 'git push origin topic'");
    assert_eq!(p.branch, lit("topic"));
}

#[test]
fn text_that_only_mentions_a_push_is_not_one() {
    for c in [
        "grep 'git push origin topic' log.txt",
        "grep -r \"git push\" docs",
        "echo 'git push origin topic'",
        "cat <<'EOF'\ngit push origin topic\nEOF",
        "cat <<EOF > notes.md\ngit push origin topic\nEOF",
        "git commit -m 'run git push origin topic later'",
        "git pull origin topic",
        "git log --oneline",
        "# git push origin topic",
        "echo done",
        "cargo test",
        "",
        "echo 'unterminated",
    ] {
        assert!(push_of(c).is_empty(), "{c:?} -> {:?}", push_of(c));
    }
}

#[test]
fn each_branch_of_a_push_is_one_intent() {
    let v = push_of("git push origin a b");
    assert_eq!(v.len(), 2);
    assert_eq!((&v[0].branch, &v[1].branch), (&lit("a"), &lit("b")));
    let v = push_of("git push origin a && git push origin b");
    assert_eq!(v.len(), 2);
}
