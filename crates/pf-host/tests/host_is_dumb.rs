//! AC-8: the host stays dumb (SPINE.md G1). Its manifest names no engine, workflow, rule or adapter
//! crate (and no async runtime), and its source names no runtime, workflow or rule word. In the
//! style of `crates/pulse-factory/tests/no_method_code.rs`: each check has a case over a bad sample
//! that must fire.

use std::path::{Path, PathBuf};

/// Crates the host must not depend on. `pf-adapter-` is a prefix: every adapter crate.
const FORBIDDEN_CRATES: [&str; 5] = [
    "pf-engine",
    "pf-workflow",
    "pf-rules",
    "pf-adapter-",
    "tokio",
];

/// Words the source must not hold, as a whole word or an identifier part.
const FORBIDDEN_WORDS: [&str; 7] = [
    "omp",
    "claude",
    "stream-json",
    "stream_json",
    "workflow",
    "rule",
    "rules",
];

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// The forbidden words in `text`. An underscore and a hyphen split an identifier, so `omp_cmd`
/// holds `omp`; `compile` and `ruling` do not.
fn flags(text: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for word in FORBIDDEN_WORDS {
        let mut from = 0;
        while let Some(i) = text[from..].find(word) {
            let start = from + i;
            let end = start + word.len();
            let before = text[..start].chars().next_back();
            let after = text[end..].chars().next();
            if before.is_none_or(|c| !is_word(c)) && after.is_none_or(|c| !is_word(c)) {
                found.push(word);
                break;
            }
            from = start + 1;
        }
    }
    found
}

/// The forbidden crate names in the dependency tables of a manifest.
fn manifest_hits(toml: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let mut in_deps = false;
    for line in toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t.contains("dependencies");
            continue;
        }
        if in_deps && !t.starts_with('#') {
            for name in FORBIDDEN_CRATES {
                if t.starts_with(name) {
                    hits.push(format!("{t} (names {name})"));
                }
            }
        }
    }
    hits
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
    for (sample, word) in [
        ("a runtime called omp", "omp"),
        ("let c = omp_cmd;", "omp"),
        ("spawn claude here", "claude"),
        ("parse stream-json lines", "stream-json"),
        ("fn stream_json()", "stream_json"),
        ("the workflow file", "workflow"),
        ("apply the rule", "rule"),
        ("these rules", "rules"),
    ] {
        assert!(
            flags(sample).contains(&word),
            "{word} must fire on {sample:?}"
        );
    }
    for good in [
        "compile and company",
        "a ruling and a rulebook",
        "the cloud, not claudette",
        "frompt",
    ] {
        assert!(
            flags(good).is_empty(),
            "{good:?} must not fire: {:?}",
            flags(good)
        );
    }
    for sample in [
        "[dependencies]\npf-engine = { path = \"../pf-engine\" }\n",
        "[dependencies]\npf-workflow.workspace = true\n",
        "[dev-dependencies]\npf-rules = { path = \"../pf-rules\" }\n",
        "[dependencies]\npf-adapter-omp = { path = \"../x\" }\n",
        "[dependencies]\ntokio.workspace = true\n",
    ] {
        assert!(!manifest_hits(sample).is_empty(), "must fire on {sample:?}");
    }
    // A mention outside a dependency table is not a dependency.
    assert!(manifest_hits("[package]\ndescription = \"no pf-engine here\"\n").is_empty());
}

#[test]
fn the_manifest_names_no_engine_workflow_rule_or_adapter_crate() {
    let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(toml.contains("[package]"), "the manifest was not read");
    assert!(
        toml.contains("portable-pty"),
        "the manifest lacks its own dependencies"
    );
    let hits = manifest_hits(&toml);
    assert!(
        hits.is_empty(),
        "the host's manifest names a forbidden crate:\n{}",
        hits.join("\n")
    );
}

#[test]
fn the_source_names_no_runtime_workflow_or_rule() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    assert!(
        files.len() >= 8,
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
        "the host's source names a runtime, workflow or rule:\n{}",
        hits.join("\n")
    );
}
