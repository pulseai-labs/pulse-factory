//! AC-14: nothing above the adapter crate may need one agent runtime.
//!
//! The test reads every file under `pf-engine/src/` and `pf-rules/src/`. It fails, ignoring case,
//! on the name of the first runtime, on its plugin flag, on the name of the terminal crate and on
//! the word for a pseudo-terminal, each as a whole word or an identifier part. It also fails on a
//! dependency on an adapter crate in either manifest, whether normal, build, dev, target-specific,
//! renamed or inherited from the workspace. Each check has a case over an inline bad sample that
//! must fire.
//!
//! The banned words sit in the lists below, so this file is not under either `src/` on purpose.

use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ source check

/// Split text into identifier parts, lower-cased: break on every character that is not a letter
/// or a digit (so `_` and `-` alike), at a lower-to-upper case change, and between letters and
/// digits.
fn parts(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < chars.len() && chars[i].is_ascii_alphanumeric() {
            let (p, c) = (chars[i - 1], chars[i]);
            let boundary = (p.is_ascii_lowercase() && c.is_ascii_uppercase())
                || (p.is_ascii_digit() != c.is_ascii_digit())
                || (p.is_ascii_uppercase()
                    && c.is_ascii_uppercase()
                    && chars.get(i + 1).is_some_and(char::is_ascii_lowercase));
            if boundary {
                break;
            }
            i += 1;
        }
        out.push(
            chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase(),
        );
    }
    out
}

/// What is wrong with this text, as short descriptions.
fn banned_in(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for part in parts(text) {
        if part == "omp" || part == "pty" {
            hits.push(part);
        }
    }
    if text.to_ascii_lowercase().contains("--plugin-dir") {
        hits.push("--plugin-dir".to_string());
    }
    hits
}

// ------------------------------------------------------------------ manifest check

fn is_adapter(name: &str) -> bool {
    let n = name.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    n.contains("pf-adapter") || n.contains("pf_adapter")
}

/// Keys of workspace dependencies that point at an adapter crate, by key, rename or path.
fn workspace_adapter_names(workspace_manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_table = false;
    for line in workspace_manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_table = line.trim_matches(['[', ']']) == "workspace.dependencies";
            continue;
        }
        if !in_table || line.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = line.split_once('=')
            && is_adapter(line)
        {
            names.push(
                key.trim()
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .trim_matches('"')
                    .to_string(),
            );
        }
    }
    names
}

/// Every line of the manifest that depends on an adapter crate: it names one anywhere, or it is a
/// dependency that the workspace manifest maps to one.
fn adapter_dependencies(manifest: &str, workspace_manifest: &str) -> Vec<String> {
    let inherited = workspace_adapter_names(workspace_manifest);
    let mut found = Vec::new();
    let mut in_dependencies = false;
    let mut named_table: Option<String> = None;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let header = line.trim_matches(['[', ']']);
            in_dependencies = header.ends_with("dependencies");
            named_table = header.rfind("dependencies.").map(|at| {
                header[at + "dependencies.".len()..]
                    .trim_matches(['"', '\''])
                    .to_string()
            });
            if is_adapter(header) {
                found.push(line.to_string());
            }
            if named_table.as_ref().is_some_and(|n| inherited.contains(n)) {
                found.push(format!("{line} (inherited from the workspace)"));
            }
            continue;
        }
        // A description may name a neighbour, so only a dependency position counts.
        if !(in_dependencies || named_table.is_some()) {
            continue;
        }
        if is_adapter(line) {
            found.push(line.to_string());
            continue;
        }
        if in_dependencies {
            let key = line
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"');
            let base = key.split('.').next().unwrap_or(key);
            if inherited.iter().any(|n| n == base) {
                found.push(format!("{line} (inherited from the workspace)"));
            }
        }
    }
    found
}

// ------------------------------------------------------------------ files

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn engine_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rules_dir() -> PathBuf {
    engine_dir().join("../pf-rules")
}

fn assert_source_is_neutral(dir: &Path, min_files: usize) {
    let mut files = Vec::new();
    files_under(&dir.join("src"), &mut files);
    assert!(
        files.len() >= min_files,
        "the walk of {} found {} files",
        dir.display(),
        files.len()
    );
    let mut findings = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            for hit in banned_in(line) {
                findings.push(format!("{}:{}: {hit}", file.display(), n + 1));
            }
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

// ------------------------------------------------------------------ tests

#[test]
fn the_engine_source_needs_no_runtime() {
    assert_source_is_neutral(&engine_dir(), 10);
}

#[test]
fn the_rules_source_needs_no_runtime() {
    assert_source_is_neutral(&rules_dir(), 3);
}

#[test]
fn neither_manifest_names_the_terminal_crate() {
    for dir in [engine_dir(), rules_dir()] {
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert_eq!(
            banned_in(&manifest),
            Vec::<String>::new(),
            "{}",
            dir.display()
        );
    }
}

#[test]
fn neither_manifest_depends_on_an_adapter_crate() {
    let workspace = std::fs::read_to_string(engine_dir().join("../../Cargo.toml")).unwrap();
    for dir in [engine_dir(), rules_dir()] {
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert_eq!(
            adapter_dependencies(&manifest, &workspace),
            Vec::<String>::new(),
            "{}",
            dir.display()
        );
        // The check reads real dependencies: each manifest holds at least one.
        assert!(manifest.contains("[dependencies]"), "{}", dir.display());
    }
}

#[test]
fn every_banned_word_fires_on_an_inline_bad_sample() {
    let bad = [
        ("the runtime name", "use omp::Session;"),
        ("the runtime name, upper case", "const OMP: u32 = 1;"),
        (
            "the runtime name, as an identifier part",
            "struct OmpConfig;",
        ),
        ("the runtime name, in snake case", "fn start_omp_seat() {}"),
        (
            "the runtime name, in a comment",
            "// a free name such as omp",
        ),
        ("the runtime name, in a string", "let n = \"omp\";"),
        ("the plugin flag", "args.push(\"--plugin-dir\");"),
        ("the plugin flag, upper case", "// --PLUGIN-DIR is the flag"),
        ("the terminal crate", "use portable_pty::native_pty_system;"),
        (
            "the terminal crate, in a manifest",
            "portable-pty = \"=0.9.0\"",
        ),
        ("the pseudo-terminal word", "let pty = open();"),
        (
            "the pseudo-terminal word, in camel case",
            "struct PtyDriver;",
        ),
        (
            "the pseudo-terminal word, upper case",
            "const PTY_SIZE: u16 = 24;",
        ),
    ];
    for (name, sample) in bad {
        assert!(
            !banned_in(sample).is_empty(),
            "the {name} sample must fire: {sample}"
        );
    }
}

#[test]
fn ordinary_words_that_contain_a_banned_word_pass() {
    let good = [
        "fn compile() {}",
        "// the company and its comp",
        "let empty = Vec::new();",
        "let prompt = ask();",
        "let promptly = 1; let ompl = 2;",
        "let dir = plugin_dir(); // a plugin directory",
        "// a plugin-dir style flag is a runtime detail",
        "let typtyp = 1;",
    ];
    for sample in good {
        assert_eq!(banned_in(sample), Vec::<String>::new(), "{sample}");
    }
}

#[test]
fn every_kind_of_adapter_dependency_fires_on_an_inline_bad_sample() {
    let workspace = "[workspace.dependencies]\nserde = \"1\"\nseat = { package = \"pf-adapter-omp\", path = \"crates/x\" }\nfake = { path = \"crates/pf-adapter-fake\" }\n";
    let bad = [
        (
            "normal",
            "[dependencies]\npf-adapter-fake = { path = \"../pf-adapter-fake\" }\n",
        ),
        (
            "normal, version",
            "[dependencies]\npf-adapter-omp = \"1\"\n",
        ),
        (
            "underscore spelling",
            "[dependencies]\npf_adapter_fake = \"1\"\n",
        ),
        ("dev", "[dev-dependencies]\npf-adapter-fake = \"1\"\n"),
        ("build", "[build-dependencies]\npf-adapter-fake = \"1\"\n"),
        (
            "target-specific",
            "[target.'cfg(unix)'.dependencies]\npf-adapter-fake = \"1\"\n",
        ),
        (
            "renamed",
            "[dependencies]\nagent = { package = \"pf-adapter-fake\", path = \"x\" }\n",
        ),
        (
            "table form",
            "[dependencies.pf-adapter-fake]\nversion = \"1\"\n",
        ),
        (
            "table form, renamed",
            "[dependencies.agent]\npackage = \"pf-adapter-fake\"\n",
        ),
        (
            "dotted key",
            "[dependencies]\npf-adapter-fake.path = \"../pf-adapter-fake\"\n",
        ),
        (
            "path to an adapter under another name",
            "[dependencies]\nagent = { path = \"../pf-adapter-fake\" }\n",
        ),
        (
            "inherited by key",
            "[dependencies]\nfake.workspace = true\n",
        ),
        (
            "inherited rename, inline",
            "[dependencies]\nseat = { workspace = true }\n",
        ),
        (
            "inherited rename, table form",
            "[dependencies.seat]\nworkspace = true\n",
        ),
    ];
    for (name, manifest) in bad {
        assert!(
            !adapter_dependencies(manifest, workspace).is_empty(),
            "the {name} sample must fire:\n{manifest}"
        );
    }
    let good = "[package]\nname = \"pf-engine\"\ndescription = \"pf-adapter-omp is a neighbour\"\n[dependencies]\nserde.workspace = true\nserde_json = \"1\"\n[dev-dependencies]\ntempfile = \"3\"\n";
    assert_eq!(adapter_dependencies(good, workspace), Vec::<String>::new());
}
