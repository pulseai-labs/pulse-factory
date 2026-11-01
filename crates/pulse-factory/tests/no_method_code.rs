//! AC-12: the binary holds no planning method's name or command (ADR-0002). A workflow package
//! names the tools, and `arm` takes their values as `--param`.

use std::path::{Path, PathBuf};

/// Each forbidden word, and whether it also matches as the start of a longer identifier word.
const FORBIDDEN: [&str; 6] = [
    "ossify",
    "oss",
    "run-spine",
    "work_item",
    "worktree_path",
    "SPINE_",
];

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// The forbidden words in `text`, as a whole word or an identifier part. An underscore and a
/// hyphen split an identifier, so `oss_cmd` and `oss-cmd` hold `oss`; `cross` and `process` do not.
fn flags(text: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for word in FORBIDDEN {
        let mut from = 0;
        while let Some(i) = text[from..].find(word) {
            let start = from + i;
            let end = start + word.len();
            let before = text[..start].chars().next_back();
            let after = text[end..].chars().next();
            let open_before = before.is_none_or(|c| !is_word(c));
            let open_after = word.ends_with('_') || after.is_none_or(|c| !is_word(c));
            if open_before && open_after {
                found.push(word);
                break;
            }
            from = start + 1;
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn each_check_fires_on_a_bad_sample() {
    let bad: [(&str, &str); 6] = [
        ("run the ossify skill", "ossify"),
        ("call oss get now", "oss"),
        ("let c = oss_cmd;", "oss"),
        ("then run-spine starts", "run-spine"),
        ("work_item_status", "work_item"),
        ("env SPINE_STOP_FILE=x", "SPINE_"),
    ];
    for (sample, word) in bad {
        assert!(
            flags(sample).contains(&word),
            "{word} must fire on {sample:?}"
        );
    }
    assert!(flags("a unit.vars.worktree_path var").contains(&"worktree_path"));
    for good in [
        "the cross and the boss",
        "process and mass",
        "a worktree is a directory",
        "ossification",
        "FOSS",
    ] {
        assert!(
            flags(good).is_empty(),
            "{good:?} must not fire: {:?}",
            flags(good)
        );
    }
}

#[test]
fn the_sources_hold_no_planning_method() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("../pf-ipc/src"), &mut files);
    assert!(
        files.len() >= 3,
        "the check read only {} files",
        files.len()
    );
    let mut hits = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (n, line) in text.lines().enumerate() {
            for w in flags(line) {
                hits.push(format!("{}:{}: {w}", f.display(), n + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the binary names a planning method:\n{}",
        hits.join("\n")
    );
}

#[test]
fn the_engine_does_not_depend_on_the_binary_or_its_ipc() {
    let toml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pf-engine/Cargo.toml"
    ))
    .unwrap();
    assert!(
        toml.contains("[package]"),
        "the engine's Cargo.toml was not read"
    );
    for name in ["pulse-factory", "pf-ipc"] {
        assert!(!toml.contains(name), "pf-engine lists {name}");
    }
    // The checker itself sees a dependency line.
    assert!("pf-ipc = { path = \"../pf-ipc\" }".contains("pf-ipc"));
}
