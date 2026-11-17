//! AC-1 (`r2.s1.w2`): `workflows/ossify-spine/scripts/stranded.sh` turns a close seat that ended with
//! no done signal into a stop file that says why. The controller runs it only after that, so it
//! always writes the stop file. It reads the worktree with four git reads and writes nothing else.
//!
//! Every test builds a temp repo with local git only: no network, no host identity.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const UNIT: &str = "r9.s9.w9";
const BRANCH: &str = "work/r9.s9.w9-close-demo";

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../workflows/ossify-spine/scripts/stranded.sh")
}

/// Run git in `dir` with no host configuration. It panics when git fails.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=paper",
            "-c",
            "user.email=paper@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare `origin`, and a worktree cloned from it on `BRANCH`, with `base` the commit at the start.
struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    origin: PathBuf,
    worktree: PathBuf,
    base: String,
    stop: PathBuf,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    let worktree = root.join("worktree");
    std::fs::create_dir_all(&seed).unwrap();
    git(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("README.md"), "root\n").unwrap();
    git(&seed, &["add", "README.md"]);
    git(&seed, &["commit", "-q", "-m", "root"]);
    git(&seed, &["push", "-q", origin.to_str().unwrap(), "main"]);
    git(
        &root,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
    );
    git(&worktree, &["checkout", "-q", "-b", BRANCH]);
    let base = git(&worktree, &["rev-parse", "HEAD"]);
    let stop = root.join("out").join("stop");
    std::fs::create_dir_all(stop.parent().unwrap()).unwrap();
    Rig {
        _dir: dir,
        root,
        origin,
        worktree,
        base,
        stop,
    }
}

impl Rig {
    fn stage(&self, names: &[&str]) {
        for n in names {
            std::fs::write(self.worktree.join(n), format!("{n}\n")).unwrap();
        }
        let mut args = vec!["add"];
        args.extend_from_slice(names);
        git(&self.worktree, &args);
    }

    fn run_with(&self, worktree: &Path, base: &str) -> Output {
        Command::new("sh")
            .arg(script())
            .arg(worktree)
            .arg(BRANCH)
            .arg(base)
            .arg(UNIT)
            .arg(&self.stop)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("sh runs")
    }

    fn run(&self) -> Output {
        self.run_with(&self.worktree, &self.base)
    }

    /// Run it and demand exit 0 and a stop file; return the stop text.
    fn stop_text(&self) -> String {
        let out = self.run();
        assert!(
            out.status.success(),
            "exit {:?}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(&self.stop).expect("the stop file exists")
    }

    /// The state that the script must not change: refs, index bytes and the refs of `origin`.
    fn snapshot(&self) -> (String, Vec<u8>, String, String) {
        (
            git(&self.worktree, &["for-each-ref"]),
            std::fs::read(self.worktree.join(".git/index")).unwrap_or_default(),
            git(&self.origin, &["for-each-ref"]),
            git(&self.worktree, &["status", "--porcelain"]),
        )
    }
}

#[test]
fn staged_files_make_a_stop_that_names_the_item_the_branch_each_file_and_the_count() {
    let r = rig();
    r.stage(&["alpha.txt", "beta.txt"]);
    let text = r.stop_text();
    let first = text.lines().next().unwrap();
    for want in [
        UNIT,
        BRANCH,
        "the close seat ended before the item PR opened",
    ] {
        assert!(first.contains(want), "line 1 lacks {want:?}: {first}");
    }
    for want in ["alpha.txt", "beta.txt", "2 files"] {
        assert!(text.contains(want), "the stop lacks {want:?}:\n{text}");
    }
    assert!(
        text.contains("answer to run close again; the ruling is appended to rulings.md"),
        "the stop ends with the next step:\n{text}"
    );
    assert!(
        text.trim_end()
            .ends_with("answer to run close again; the ruling is appended to rulings.md"),
        "the next step is the last line:\n{text}"
    );
}

#[test]
fn more_than_ten_staged_files_list_ten_and_the_count() {
    let r = rig();
    let names: Vec<String> = (1..=12).map(|i| format!("f{i:02}.txt")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    r.stage(&refs);
    let text = r.stop_text();
    assert!(text.contains("12 files"), "the count:\n{text}");
    // Git lists a staged path set in name order, so the first ten are f01 to f10.
    for n in &names[..10] {
        assert!(text.contains(n.as_str()), "{n} is listed:\n{text}");
    }
    for n in &names[10..] {
        assert!(
            !text.contains(n.as_str()),
            "{n} is past the first ten:\n{text}"
        );
    }
}

#[test]
fn unstaged_changes_are_named() {
    let r = rig();
    std::fs::write(r.worktree.join("loose.txt"), "x\n").unwrap();
    std::fs::write(r.worktree.join("README.md"), "changed\n").unwrap();
    let text = r.stop_text();
    assert!(text.contains("unstaged"), "{text}");
    assert!(text.contains("loose.txt"), "{text}");
    assert!(text.contains("README.md"), "{text}");
}

#[test]
fn commits_past_the_base_and_a_branch_on_origin_are_named() {
    let r = rig();
    std::fs::write(r.worktree.join("done.txt"), "x\n").unwrap();
    git(&r.worktree, &["add", "done.txt"]);
    git(&r.worktree, &["commit", "-q", "-m", "work"]);
    git(&r.worktree, &["push", "-q", "origin", BRANCH]);
    let text = r.stop_text();
    assert!(text.contains("1 commit past the base"), "{text}");
    assert!(
        text.contains("origin has the branch") && !text.contains("no branch on origin"),
        "{text}"
    );
}

#[test]
fn a_clean_worktree_gets_a_stop_that_says_nothing_was_staged_or_committed() {
    let r = rig();
    let text = r.stop_text();
    assert!(
        text.contains("nothing staged, no commit past the base, no branch on origin"),
        "{text}"
    );
    assert!(text.contains(UNIT) && text.contains(BRANCH), "{text}");
}

#[test]
fn a_failing_ls_remote_is_named_and_the_script_still_exits_0() {
    let r = rig();
    r.stage(&["alpha.txt"]);
    git(
        &r.worktree,
        &[
            "remote",
            "set-url",
            "origin",
            r.root.join("no-such-origin.git").to_str().unwrap(),
        ],
    );
    let text = r.stop_text();
    assert!(text.contains("origin not read:"), "{text}");
    assert!(
        !text.contains("no branch on origin") && !text.contains("origin has the branch"),
        "a failed read claims nothing about origin:\n{text}"
    );
    assert!(text.contains("alpha.txt"), "the other facts stay:\n{text}");
    // Only the first line of git's error is named.
    let line = text
        .lines()
        .find(|l| l.contains("origin not read:"))
        .unwrap();
    assert!(!line.trim_end().ends_with("origin not read:"), "{line}");
}

#[test]
fn an_unreadable_worktree_exits_non_zero_and_writes_nothing() {
    let r = rig();
    for bad in [r.root.join("no-such-worktree"), r.root.join("out")] {
        let out = r.run_with(&bad, &r.base);
        assert!(!out.status.success(), "{bad:?}: {:?}", out.status);
        assert!(!r.stop.exists(), "{bad:?}: a stop file was written");
        assert!(
            String::from_utf8_lossy(&out.stderr).starts_with("stranded.sh: "),
            "the failure says why on stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn a_stop_file_that_cannot_be_written_exits_non_zero() {
    let r = rig();
    r.stage(&["alpha.txt"]);
    let out = Command::new("sh")
        .arg(script())
        .arg(&r.worktree)
        .arg(BRANCH)
        .arg(&r.base)
        .arg(UNIT)
        .arg(r.root.join("no-such-dir").join("stop"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "{:?}", out.status);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("stranded.sh: "));
}

#[test]
fn a_stop_path_that_is_a_symbolic_link_is_refused() {
    let r = rig();
    let target = r.root.join("elsewhere.txt");
    std::fs::write(&target, "keep\n").unwrap();
    std::os::unix::fs::symlink(&target, &r.stop).unwrap();
    let out = r.run();
    assert!(!out.status.success(), "{:?}", out.status);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("stranded.sh: "));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep\n");
}

#[test]
fn it_writes_no_ref_no_index_and_nothing_on_origin() {
    let r = rig();
    r.stage(&["alpha.txt", "beta.txt"]);
    std::fs::write(r.worktree.join("loose.txt"), "x\n").unwrap();
    let before = r.snapshot();
    r.stop_text();
    assert_eq!(
        before,
        r.snapshot(),
        "refs, index, origin and status are unchanged"
    );
}

#[test]
fn a_base_that_is_not_a_sha_is_named_and_the_script_goes_on() {
    let r = rig();
    r.stage(&["alpha.txt"]);
    // A value that starts with `-` must never reach git as an option. Unguarded, the script runs
    // `git rev-list "--output=<root>/planted.txt..HEAD"`, and git creates the file named
    // `planted.txt..HEAD`: the script appends `..HEAD` to the value. So the test looks for that
    // name, and for the guard's own message. A git usage error also says `commits not read:`, so
    // the prefix alone proves nothing.
    let planted = r.root.join("planted.txt");
    let created = r.root.join("planted.txt..HEAD");
    let out = r.run_with(&r.worktree, &format!("--output={}", planted.display()));
    assert!(out.status.success(), "{:?}", out.status);
    assert!(
        !created.exists(),
        "the base value reached git as an option: {} exists",
        created.display()
    );
    assert!(!planted.exists(), "the base value reached git as an option");
    let text = std::fs::read_to_string(&r.stop).unwrap();
    assert!(
        text.contains("commits not read: the base is not a SHA"),
        "{text}"
    );
    assert!(text.contains("alpha.txt"), "{text}");
}
